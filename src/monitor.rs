//! Resource monitoring: how much of the audio deadline the effects use, JACK
//! dropouts (xruns) and the health of the board (CPU, RAM, temperature,
//! Raspberry Pi throttling).
//!
//! The audio thread only touches atomics ([`DspMeter::record`]); everything
//! else is read from /proc and /sys by the UI thread once per second. Values
//! that cannot be read on the current machine are `None`.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Load (fraction of the period) at which the audio thread is close to
/// missing its deadline
pub const DSP_WARNING: f32 = 0.70;
pub const DSP_CRITICAL: f32 = 0.90;
/// SoC temperature, °C (the Pi starts throttling at 80-85 °C)
pub const TEMP_WARNING: f32 = 70.0;
pub const TEMP_CRITICAL: f32 = 80.0;
/// Available RAM fraction
const MEMORY_WARNING: f32 = 0.10;

/// Time spent in the audio callback, relative to the buffer period.
/// Written by the audio thread, read and reset by the UI.
#[derive(Debug, Default)]
pub struct DspMeter {
    /// Sum of the loads since the last read, in 1/10000
    sum: AtomicU32,
    cycles: AtomicU32,
    /// Highest load since the last read, in 1/10000
    peak: AtomicU32,
    xruns: AtomicU32,
}

/// DSP load over a read window
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DspStats {
    /// Average and highest fraction of the period used by the callback
    pub average: f32,
    pub peak: f32,
    /// Audio cycles in the window (0 = the callback did not run)
    pub cycles: u32,
    /// xruns since start
    pub xruns: u32,
}

impl DspMeter {
    /// Record one audio cycle (real-time safe)
    pub fn record(&self, elapsed: Duration, period: Duration) {
        if period.is_zero() {
            return;
        }
        let load =
            (elapsed.as_secs_f32() / period.as_secs_f32() * 10_000.0).min(1_000_000.0) as u32;
        self.sum.fetch_add(load, Ordering::Relaxed);
        self.cycles.fetch_add(1, Ordering::Relaxed);
        self.peak.fetch_max(load, Ordering::Relaxed);
    }

    /// Count a JACK xrun (called from the JACK notification thread)
    pub fn xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Stats since the previous call
    pub fn take(&self) -> DspStats {
        let cycles = self.cycles.swap(0, Ordering::Relaxed);
        let sum = self.sum.swap(0, Ordering::Relaxed);
        let peak = self.peak.swap(0, Ordering::Relaxed);
        DspStats {
            average: if cycles > 0 {
                sum as f32 / cycles as f32 / 10_000.0
            } else {
                0.0
            },
            peak: peak as f32 / 10_000.0,
            cycles,
            xruns: self.xruns.load(Ordering::Relaxed),
        }
    }
}

/// Time each block of the chain spends processing audio, by instance id.
/// The chain builder hands every block the counter of its id; the audio
/// thread only adds to it, the UI reads it.
#[derive(Debug, Default)]
pub struct BlockMeter {
    counters: Mutex<HashMap<usize, Arc<AtomicU64>>>,
}

impl BlockMeter {
    /// The meter of the running chain
    pub fn global() -> &'static BlockMeter {
        static METER: OnceLock<BlockMeter> = OnceLock::new();
        METER.get_or_init(BlockMeter::default)
    }

    /// Nanoseconds counter of the block `instance_id` (not real-time safe:
    /// called when building the chain)
    pub fn counter(&self, instance_id: usize) -> Arc<AtomicU64> {
        self.counters
            .lock()
            .unwrap()
            .entry(instance_id)
            .or_default()
            .clone()
    }

    /// Nanoseconds the block has spent processing so far, if it was built
    pub fn spent(&self, instance_id: usize) -> Option<u64> {
        let counters = self.counters.lock().unwrap();
        Some(counters.get(&instance_id)?.load(Ordering::Relaxed))
    }
}

/// Share of one CPU core a block uses, from two readings of
/// [`BlockMeter::spent`] taken `elapsed` apart
pub fn block_load(before: u64, after: u64, elapsed: Duration) -> f32 {
    if elapsed.is_zero() {
        return 0.0;
    }
    after.saturating_sub(before) as f32 / elapsed.as_nanos() as f32
}

/// Peak levels of the input and both outputs, for the level meters.
/// Written by the audio thread, read and reset by the UI.
#[derive(Debug, Default)]
pub struct LevelMeter {
    /// f32 bits: the order of the bits of non-negative floats is their
    /// numeric order, so `fetch_max` keeps the highest peak
    input: AtomicU32,
    left: AtomicU32,
    right: AtomicU32,
}

/// Peak amplitudes (1.0 = full scale) over a read window
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Levels {
    pub input: f32,
    pub left: f32,
    pub right: f32,
}

impl LevelMeter {
    /// Record one audio cycle (real-time safe)
    pub fn record(&self, input: &[f32], left: &[f32], right: &[f32]) {
        for (slot, buffer) in [
            (&self.input, input),
            (&self.left, left),
            (&self.right, right),
        ] {
            let peak = buffer.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if peak.is_finite() {
                slot.fetch_max(peak.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Peaks since the previous call
    pub fn take(&self) -> Levels {
        let take = |slot: &AtomicU32| f32::from_bits(slot.swap(0, Ordering::Relaxed));
        Levels {
            input: take(&self.input),
            left: take(&self.left),
            right: take(&self.right),
        }
    }
}

/// Board health snapshot
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemStats {
    /// Whole-machine CPU usage, and the busiest core (0..1)
    pub cpu: Option<f32>,
    pub cpu_max_core: Option<f32>,
    pub memory_used_mb: Option<u32>,
    pub memory_total_mb: Option<u32>,
    pub temperature: Option<f32>,
    /// Raspberry Pi firmware flags, current state only (see [`Throttling`])
    pub throttling: Option<Throttling>,
}

/// Current Raspberry Pi firmware limits (`vcgencmd get_throttled`, bits 0-3)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Throttling {
    pub under_voltage: bool,
    pub frequency_capped: bool,
    pub throttled: bool,
    pub soft_temperature_limit: bool,
}

impl Throttling {
    pub fn from_bits(bits: u32) -> Self {
        Self {
            under_voltage: bits & 0x1 != 0,
            frequency_capped: bits & 0x2 != 0,
            throttled: bits & 0x4 != 0,
            soft_temperature_limit: bits & 0x8 != 0,
        }
    }

    pub fn any(&self) -> bool {
        self.under_voltage || self.frequency_capped || self.throttled || self.soft_temperature_limit
    }
}

/// Overall state, worst of all the checks
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Ok,
    Warning,
    Critical,
}

/// Level and a short reason, for the status indicator
pub fn assess(dsp: Option<&DspStats>, new_xruns: u32, system: &SystemStats) -> (Level, String) {
    let mut worst = (Level::Ok, String::new());
    let mut flag = |level: Level, reason: String| {
        if level > worst.0 {
            worst = (level, reason);
        }
    };

    if let Some(dsp) = dsp.filter(|d| d.cycles > 0) {
        let load = dsp.average.max(dsp.peak * 0.9);
        if load >= DSP_CRITICAL {
            flag(
                Level::Critical,
                format!("DSP overloaded ({:.0}%): remove effects", load * 100.0),
            );
        } else if load >= DSP_WARNING {
            flag(
                Level::Warning,
                format!("DSP load high ({:.0}%)", load * 100.0),
            );
        }
    }
    if new_xruns > 0 {
        flag(Level::Warning, format!("{} audio dropout(s)", new_xruns));
    }
    if let Some(t) = system.temperature {
        if t >= TEMP_CRITICAL {
            flag(Level::Critical, format!("CPU too hot ({:.0} °C)", t));
        } else if t >= TEMP_WARNING {
            flag(Level::Warning, format!("CPU hot ({:.0} °C)", t));
        }
    }
    if let Some(throttling) = system.throttling {
        if throttling.under_voltage {
            flag(
                Level::Critical,
                "Under-voltage: check the power supply".into(),
            );
        } else if throttling.throttled || throttling.frequency_capped {
            flag(Level::Warning, "CPU throttled by the firmware".into());
        } else if throttling.soft_temperature_limit {
            flag(Level::Warning, "Soft temperature limit reached".into());
        }
    }
    if let (Some(used), Some(total)) = (system.memory_used_mb, system.memory_total_mb) {
        if total > 0 && (total - used.min(total)) as f32 / total as f32 <= MEMORY_WARNING {
            flag(
                Level::Warning,
                format!("Low memory ({} / {} MB)", used, total),
            );
        }
    }
    worst
}

/// CPU time counters of one /proc/stat line
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CpuTimes {
    busy: u64,
    total: u64,
}

impl CpuTimes {
    fn usage_since(&self, previous: &CpuTimes) -> Option<f32> {
        let total = self.total.checked_sub(previous.total)?;
        let busy = self.busy.checked_sub(previous.busy)?;
        (total > 0).then(|| (busy as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// Parse /proc/stat: the aggregate line, then one entry per core
fn parse_proc_stat(text: &str) -> Vec<CpuTimes> {
    text.lines()
        .filter(|line| line.starts_with("cpu"))
        .filter_map(|line| {
            let fields: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|f| f.parse().ok())
                .collect();
            // user nice system idle iowait irq softirq steal
            let total: u64 = fields.iter().take(8).sum();
            let idle = fields.get(3)? + fields.get(4).unwrap_or(&0);
            Some(CpuTimes {
                busy: total - idle,
                total,
            })
        })
        .collect()
}

/// (used, total) in MB from /proc/meminfo
fn parse_meminfo(text: &str) -> Option<(u32, u32)> {
    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find(|l| l.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    Some((
        (total.saturating_sub(available) / 1024) as u32,
        (total / 1024) as u32,
    ))
}

/// Reads the board state; keeps the previous CPU counters to compute usage
pub struct SystemMonitor {
    previous_cpu: Vec<CpuTimes>,
    thermal_zone: Option<PathBuf>,
    throttled_file: Option<PathBuf>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        Self {
            previous_cpu: Vec::new(),
            thermal_zone: find_thermal_zone(Path::new("/sys/class/thermal")),
            throttled_file: find_throttled_file(),
        }
    }

    pub fn sample(&mut self) -> SystemStats {
        let mut stats = SystemStats::default();

        if let Ok(text) = fs::read_to_string("/proc/stat") {
            let cpu = parse_proc_stat(&text);
            if cpu.len() == self.previous_cpu.len() && !cpu.is_empty() {
                stats.cpu = cpu[0].usage_since(&self.previous_cpu[0]);
                stats.cpu_max_core = cpu[1..]
                    .iter()
                    .zip(&self.previous_cpu[1..])
                    .filter_map(|(now, before)| now.usage_since(before))
                    .reduce(f32::max);
            }
            self.previous_cpu = cpu;
        }
        if let Some((used, total)) = fs::read_to_string("/proc/meminfo")
            .ok()
            .as_deref()
            .and_then(parse_meminfo)
        {
            stats.memory_used_mb = Some(used);
            stats.memory_total_mb = Some(total);
        }
        stats.temperature = self
            .thermal_zone
            .as_ref()
            .and_then(|zone| fs::read_to_string(zone.join("temp")).ok())
            .and_then(|t| t.trim().parse::<f32>().ok())
            .map(|millis| millis / 1000.0);
        stats.throttling = self
            .throttled_file
            .as_ref()
            .and_then(|file| fs::read_to_string(file).ok())
            .and_then(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
            .map(Throttling::from_bits);
        stats
    }
}

/// CPU/SoC thermal zone: the Pi's `cpu-thermal`, x86 `x86_pkg_temp`...
/// falling back to the first zone that reads
fn find_thermal_zone(root: &Path) -> Option<PathBuf> {
    let mut zones: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone"))
        })
        .filter(|p| fs::read_to_string(p.join("temp")).is_ok())
        .collect();
    zones.sort();
    let kind = |p: &PathBuf| {
        fs::read_to_string(p.join("type"))
            .unwrap_or_default()
            .to_lowercase()
    };
    zones
        .iter()
        .find(|p| {
            ["cpu", "soc", "pkg", "k10temp"]
                .iter()
                .any(|k| kind(p).contains(k))
        })
        .or(zones.first())
        .cloned()
}

/// `get_throttled` attribute of the Raspberry Pi firmware driver; its parent
/// device name depends on the model (`soc:firmware`, `soc@107c000000:firmware`...)
fn find_throttled_file() -> Option<PathBuf> {
    let platform = Path::new("/sys/devices/platform");
    let mut dirs = vec![platform.to_path_buf()];
    if let Ok(entries) = fs::read_dir(platform) {
        dirs.extend(
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir()),
        );
    }
    dirs.iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flat_map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with("firmware"))
        })
        .map(|p| p.join("get_throttled"))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_meter_keeps_the_peak_until_read() {
        let meter = LevelMeter::default();
        meter.record(&[0.1, -0.5], &[0.25], &[]);
        meter.record(&[0.2], &[-0.75], &[f32::NAN]);
        let levels = meter.take();
        assert_eq!((levels.input, levels.left, levels.right), (0.5, 0.75, 0.0));
        assert_eq!(meter.take(), Levels::default());
    }

    #[test]
    fn dsp_meter_averages_and_resets() {
        let meter = DspMeter::default();
        let period = Duration::from_millis(10);
        meter.record(Duration::from_millis(2), period);
        meter.record(Duration::from_millis(6), period);
        meter.xrun();

        let stats = meter.take();
        assert!((stats.average - 0.4).abs() < 1e-3);
        assert!((stats.peak - 0.6).abs() < 1e-3);
        assert_eq!((stats.cycles, stats.xruns), (2, 1));

        let stats = meter.take();
        assert_eq!((stats.cycles, stats.peak, stats.xruns), (0, 0.0, 1));
    }

    #[test]
    fn proc_stat_usage() {
        let before = parse_proc_stat("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\n");
        let after = parse_proc_stat("cpu  150 0 150 900 0 0 0 0 0 0\ncpu0 100 0 100 400 0 0 0 0\n");
        assert_eq!(after.len(), 2);
        assert!((after[0].usage_since(&before[0]).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(after[1].usage_since(&before[1]), Some(1.0));
    }

    #[test]
    fn meminfo_used_is_total_minus_available() {
        let text = "MemTotal:        8000000 kB\nMemFree:  100 kB\nMemAvailable:    6000000 kB\n";
        assert_eq!(parse_meminfo(text), Some((1953, 7812)));
    }

    #[test]
    fn assessment_reports_the_worst_problem() {
        let calm = DspStats {
            average: 0.2,
            peak: 0.3,
            cycles: 100,
            xruns: 0,
        };
        assert_eq!(assess(Some(&calm), 0, &SystemStats::default()).0, Level::Ok);

        let busy = DspStats {
            average: 0.75,
            peak: 0.8,
            cycles: 100,
            xruns: 0,
        };
        assert_eq!(
            assess(Some(&busy), 0, &SystemStats::default()).0,
            Level::Warning
        );

        let pi = SystemStats {
            temperature: Some(60.0),
            throttling: Some(Throttling::from_bits(0x50005)),
            ..Default::default()
        };
        let (level, reason) = assess(Some(&busy), 0, &pi);
        assert_eq!(level, Level::Critical);
        assert!(reason.contains("voltage"));

        // No audio callback: DSP stats are ignored
        let idle = DspStats {
            average: 0.0,
            peak: 0.0,
            cycles: 0,
            xruns: 3,
        };
        assert_eq!(assess(Some(&idle), 0, &SystemStats::default()).0, Level::Ok);
        assert_eq!(
            assess(Some(&idle), 2, &SystemStats::default()).0,
            Level::Warning
        );
    }

    #[test]
    fn sampling_twice_reads_cpu_on_linux() {
        let mut monitor = SystemMonitor::new();
        monitor.sample();
        std::thread::sleep(Duration::from_millis(20));
        let stats = monitor.sample();
        if cfg!(target_os = "linux") {
            assert!(stats.cpu.is_some());
            assert!(stats.memory_total_mb.unwrap_or(0) > 0);
        }
    }
}
