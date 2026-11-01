//! Settings → Audio & system: the settings of pedalboard.txt (boot
//! partition of the pedalboard OS) read from `/run/pedalboard/config` and
//! changed through the root helper `/usr/libexec/pedalboard/settings` (a
//! FIFO `request` and a file `status` in `PEDALBOARD_SETTINGS_DIR`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Directory of the root helper, set by the launcher of the pedalboard OS
pub const DIR_ENV: &str = "PEDALBOARD_SETTINGS_DIR";
/// The settings as read at boot (or after the last change)
pub const CONFIG_FILE: &str = "/run/pedalboard/config";

pub const SAMPLE_RATES: [u32; 4] = [44100, 48000, 88200, 96000];
pub const PERIODS: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];
pub const PERIOD_COUNTS: [u32; 3] = [2, 3, 4];
pub const ROTATIONS: [u32; 4] = [0, 90, 180, 270];
/// Wi-Fi countries offered ("" first: the driver's default, the most
/// restrictive channels)
pub const COUNTRIES: [&str; 33] = [
    "", "AT", "AU", "BE", "BR", "CA", "CH", "CN", "CZ", "DE", "DK", "ES", "FI", "FR", "GB", "GR",
    "HU", "IE", "IL", "IN", "IT", "JP", "KR", "LU", "MX", "NL", "NO", "NZ", "PL", "PT", "SE", "US",
    "ZA",
];

/// The settings of pedalboard.txt
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemConfig {
    /// "auto" or the id of a sound card (/proc/asound/cards)
    pub audio_device: String,
    pub sample_rate: u32,
    /// Frames per period, and periods in the buffer
    pub period: u32,
    pub periods: u32,
    pub display_rotation: u32,
    /// Two letters, or empty
    pub wifi_country: String,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            audio_device: "auto".into(),
            sample_rate: 48000,
            period: 128,
            periods: 2,
            display_rotation: 0,
            wifi_country: String::new(),
        }
    }
}

impl SystemConfig {
    pub fn load() -> Self {
        fs::read_to_string(CONFIG_FILE)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// `KEY='value'` lines; a later line wins, unknown keys and values are
    /// skipped
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('\'');
            let number = value.parse::<u32>().ok();
            match key.trim() {
                "audio_device" if valid_device(value) => config.audio_device = value.into(),
                "sample_rate" => config.sample_rate = number.unwrap_or(config.sample_rate),
                "period" => config.period = number.unwrap_or(config.period),
                "periods" => config.periods = number.unwrap_or(config.periods),
                "display_rotation" => {
                    config.display_rotation = number.unwrap_or(config.display_rotation)
                }
                "wifi_country" if valid_country(value) => config.wifi_country = value.into(),
                _ => {}
            }
        }
        config
    }

    /// Latency of the buffer one way, in ms
    pub fn latency_ms(&self) -> f32 {
        self.period as f32 * self.periods as f32 * 1000.0 / self.sample_rate.max(1) as f32
    }

    /// `key=value` pairs that differ from `old`, for the root helper
    pub fn changes(&self, old: &SystemConfig) -> Vec<String> {
        let mut pairs = Vec::new();
        let mut add = |key: &str, new: String, old: String| {
            if new != old {
                pairs.push(format!("{key}={new}"));
            }
        };
        add(
            "audio_device",
            self.audio_device.clone(),
            old.audio_device.clone(),
        );
        add(
            "sample_rate",
            self.sample_rate.to_string(),
            old.sample_rate.to_string(),
        );
        add("period", self.period.to_string(), old.period.to_string());
        add("periods", self.periods.to_string(), old.periods.to_string());
        add(
            "display_rotation",
            self.display_rotation.to_string(),
            old.display_rotation.to_string(),
        );
        add(
            "wifi_country",
            self.wifi_country.clone(),
            old.wifi_country.clone(),
        );
        pairs
    }
}

/// A card id as JACK and pedalboard.txt take it
fn valid_device(id: &str) -> bool {
    id == "auto"
        || (!id.is_empty()
            && id.len() <= 32
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

fn valid_country(code: &str) -> bool {
    code.is_empty() || (code.len() == 2 && code.chars().all(|c| c.is_ascii_uppercase()))
}

/// A sound card: its id (for pedalboard.txt) and its name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundCard {
    pub id: String,
    pub name: String,
}

/// The sound cards of the system (none where /proc/asound is missing)
pub fn sound_cards() -> Vec<SoundCard> {
    fs::read_to_string("/proc/asound/cards")
        .map(|text| parse_cards(&text))
        .unwrap_or_default()
}

/// Lines like ` 0 [AudioBox       ]: USB-Audio - AudioBox USB 96`
fn parse_cards(text: &str) -> Vec<SoundCard> {
    text.lines()
        .filter_map(|line| {
            let (number, rest) = line.trim_start().split_once(' ')?;
            number.parse::<u32>().ok()?;
            let rest = rest.trim_start().strip_prefix('[')?;
            let (id, rest) = rest.split_once(']')?;
            let id = id.trim();
            let name = rest.trim_start_matches(':').trim();
            // "driver - long name": the long name reads better
            let name = name.split_once(" - ").map_or(name, |(_, long)| long);
            valid_device(id).then(|| SoundCard {
                id: id.into(),
                name: name.into(),
            })
        })
        .collect()
}

/// What a change applied needs, from the helper's status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Saved: the audio restarts (and the app with it)
    Audio,
    /// Saved: the app restarts to turn the screen
    Display,
    /// Saved: applies at the next system start
    Reboot,
    /// Saved, nothing else to do
    Saved,
    Failed(String),
}

/// The root helper of the pedalboard OS
pub struct Helper {
    dir: PathBuf,
}

impl Helper {
    /// Only on the pedalboard OS, where the launcher sets the directory
    pub fn from_env() -> Option<Self> {
        std::env::var_os(DIR_ENV).map(|dir| Self { dir: dir.into() })
    }

    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.into() }
    }

    /// Ask for the `key=value` pairs to be written; returns the request id
    /// the status refers to
    pub fn request(&self, pairs: &[String]) -> Result<String> {
        send_request(&self.dir, "settings helper", &pairs.join(" "))
    }

    /// Status of request `id`; `None` until the helper is done with it
    pub fn status(&self, id: &str) -> Option<Status> {
        let text = fs::read_to_string(self.dir.join("status")).ok()?;
        parse_status(&text, id)
    }
}

/// Write request `args` to the FIFO `request` of the root helper in `dir`
/// (`name` in the errors); returns the request id its status refers to.
/// The settings helper and the system installer take the same protocol.
pub(crate) fn send_request(dir: &Path, name: &str, args: &str) -> Result<String> {
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    );
    let mut options = OpenOptions::new();
    options.write(true);
    // Non-blocking: without the helper reading, opening fails instead
    // of waiting forever
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut fifo = options
        .open(dir.join("request"))
        .with_context(|| format!("The {name} is not running"))?;
    writeln!(fifo, "{id} {args}").with_context(|| format!("Cannot reach the {name}"))?;
    Ok(id)
}

/// The state and the detail in a root helper's status `text`, when it is
/// about request `id`: `<id> <state> [detail]`
pub(crate) fn request_state<'a>(text: &'a str, id: &str) -> Option<(&'a str, &'a str)> {
    let line = text.lines().next()?;
    let rest = line.strip_prefix(id)?.strip_prefix(' ')?;
    Some(rest.split_once(' ').unwrap_or((rest, "")))
}

fn parse_status(text: &str, id: &str) -> Option<Status> {
    match request_state(text, id)? {
        ("done", "audio") => Some(Status::Audio),
        ("done", "display") => Some(Status::Display),
        ("done", "reboot") => Some(Status::Reboot),
        ("done", _) => Some(Status::Saved),
        ("error", "") => Some(Status::Failed("The settings could not be saved".into())),
        ("error", message) => Some(Status::Failed(message.into())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_config_written_at_boot() {
        let config = SystemConfig::parse(
            "# Defaults\naudio_device='auto'\nsample_rate='48000'\nperiod='128'\nperiods='2'\n\
             display_rotation='0'\nwifi_country=''\naudio_device='AudioBox'\nperiod='256'\n\
             wifi_country='fr'\n",
        );
        assert_eq!(config.audio_device, "AudioBox");
        assert_eq!(config.period, 256);
        // Not two capitals: the default stays
        assert_eq!(config.wifi_country, "");
        assert!((config.latency_ms() - 512.0 / 48.0).abs() < 1e-3);
    }

    #[test]
    fn only_changes_are_sent() {
        let old = SystemConfig::default();
        let mut new = old.clone();
        assert!(new.changes(&old).is_empty());
        new.sample_rate = 96000;
        new.wifi_country = "FR".into();
        assert_eq!(new.changes(&old), ["sample_rate=96000", "wifi_country=FR"]);
        new.wifi_country.clear();
        new.audio_device = "pisound".into();
        assert_eq!(
            new.changes(&old),
            ["audio_device=pisound", "sample_rate=96000"]
        );
    }

    #[test]
    fn lists_the_sound_cards() {
        let cards = parse_cards(
            " 0 [vc4hdmi0       ]: vc4-hdmi - vc4-hdmi-0\n                      vc4-hdmi-0\n\
             \x20 2 [AudioBox       ]: USB-Audio - AudioBox USB 96\n\
             \x20                     PreSonus AudioBox USB 96 at usb-xhci-hcd.1-1, full speed\n",
        );
        assert_eq!(
            cards,
            [
                SoundCard {
                    id: "vc4hdmi0".into(),
                    name: "vc4-hdmi-0".into()
                },
                SoundCard {
                    id: "AudioBox".into(),
                    name: "AudioBox USB 96".into()
                },
            ]
        );
    }

    #[test]
    fn helper_status() {
        assert_eq!(parse_status("- idle\n", "1-2"), None);
        assert_eq!(parse_status("1-2 done audio\n", "1-2"), Some(Status::Audio));
        assert_eq!(parse_status("1-2 done none\n", "1-2"), Some(Status::Saved));
        assert_eq!(
            parse_status("1-2 done reboot\n", "1-2"),
            Some(Status::Reboot)
        );
        assert_eq!(
            parse_status("1-2 error Invalid setting\n", "1-2"),
            Some(Status::Failed("Invalid setting".into()))
        );
        assert_eq!(parse_status("1-22 done audio\n", "1-2"), None);
    }

    #[cfg(unix)]
    #[test]
    fn request_without_helper_fails_fast() {
        let dir = std::env::temp_dir().join(format!("pb-settings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(Helper::new(&dir).request(&["period=256".into()]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
