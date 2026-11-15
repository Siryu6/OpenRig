//! Sound going into and out of one block, and its control outputs (a
//! compressor's gain reduction…), for the views drawn over its controls.
//!
//! The audio thread copies the block's input and output into [`BlockTap`],
//! only for the block being watched (one atomic read for the others); the UI
//! reads the last samples and turns them into levels per frequency with
//! [`Analyzer`].

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Samples kept of each side: twice the analysis window, so the UI reads
/// whole windows while the audio thread writes
const TAP_SIZE: usize = 8192;
/// Control ports kept, by port index
const CONTROLS: usize = 128;
/// Samples analysed at a time (85 ms at 48 kHz, 11.7 Hz per bin)
pub const WINDOW: usize = 4096;

/// Last samples into and out of the watched block. The audio thread writes,
/// the UI reads; a read racing a write sees a few samples of the next cycle,
/// which a spectrum does not show.
pub struct BlockTap {
    /// Instance id + 1 of the watched block, 0 for none
    watched: AtomicUsize,
    rate: AtomicU32,
    /// f32 bits
    input: Box<[AtomicU32]>,
    output: Box<[AtomicU32]>,
    /// Samples written so far
    written: AtomicUsize,
    /// Last value of each control port of the watched block, f32 bits
    controls: Box<[AtomicU32]>,
}

impl BlockTap {
    /// The tap of the running chain (created when the chain is built, never
    /// by the audio thread)
    pub fn global() -> &'static BlockTap {
        static TAP: OnceLock<BlockTap> = OnceLock::new();
        TAP.get_or_init(BlockTap::new)
    }

    fn new() -> Self {
        let ring = || (0..TAP_SIZE).map(|_| AtomicU32::new(0)).collect();
        BlockTap {
            watched: AtomicUsize::new(0),
            rate: AtomicU32::new(48_000f32.to_bits()),
            input: ring(),
            output: ring(),
            written: AtomicUsize::new(0),
            controls: (0..CONTROLS).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    /// Watch the block `instance_id`, or none
    pub fn watch(&self, instance_id: Option<usize>) {
        self.watched
            .store(instance_id.map_or(0, |id| id + 1), Ordering::Relaxed);
    }

    /// True if the block `instance_id` is watched (real-time safe)
    pub fn watches(&self, instance_id: usize) -> bool {
        self.watched.load(Ordering::Relaxed) == instance_id + 1
    }

    pub fn set_rate(&self, rate: f32) {
        self.rate.store(rate.to_bits(), Ordering::Relaxed);
    }

    pub fn rate(&self) -> f32 {
        f32::from_bits(self.rate.load(Ordering::Relaxed))
    }

    /// Record the block's input, before it processes `l` (and `r`, if
    /// `stereo`) in place (real-time safe)
    pub fn record_input(&self, l: &[f32], r: &[f32], stereo: bool) {
        self.write(&self.input, l, r, stereo);
    }

    /// Record the block's output for the same samples, and move on (real-time
    /// safe)
    pub fn record_output(&self, l: &[f32], r: &[f32], stereo: bool) {
        self.write(&self.output, l, r, stereo);
        self.written.fetch_add(l.len(), Ordering::Release);
    }

    fn write(&self, ring: &[AtomicU32], l: &[f32], r: &[f32], stereo: bool) {
        let start = self.written.load(Ordering::Relaxed);
        for (i, s) in l.iter().enumerate() {
            let s = match r.get(i) {
                Some(r) if stereo => (s + r) * 0.5,
                _ => *s,
            };
            ring[(start + i) % TAP_SIZE].store(s.to_bits(), Ordering::Relaxed);
        }
    }

    /// Record the block's control values, by port index (real-time safe)
    pub fn record_controls(&self, values: &[f32]) {
        for (slot, v) in self.controls.iter().zip(values) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// Last value of the control port `index` of the watched block
    pub fn control(&self, index: usize) -> Option<f32> {
        let slot = self.controls.get(index)?;
        Some(f32::from_bits(slot.load(Ordering::Relaxed)))
    }

    /// The last `input` and `output` samples (as many as the slices hold, at
    /// most half the tap), and how many samples were written so far
    pub fn read(&self, input: &mut [f32], output: &mut [f32]) -> usize {
        let written = self.written.load(Ordering::Acquire);
        for (ring, out) in [(&self.input, input), (&self.output, output)] {
            let n = out.len().min(TAP_SIZE / 2);
            let start = written.wrapping_sub(n);
            for (i, s) in out[..n].iter_mut().enumerate() {
                *s = f32::from_bits(ring[start.wrapping_add(i) % TAP_SIZE].load(Ordering::Relaxed));
            }
        }
        written
    }
}

/// Levels per frequency of a window of samples
pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    frame: Vec<f32>,
    bins: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Vec<f32>,
}

impl Default for Analyzer {
    fn default() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let window = (0..WINDOW)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Self {
            frame: fft.make_input_vec(),
            bins: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            power: vec![0.0; WINDOW / 2 + 1],
            fft,
            window,
        }
    }
}

impl Analyzer {
    /// Level in dB at each of `freqs` (rising), a full-scale sine reading 0
    /// dB: the loudest bin between the points around it, or the bins either
    /// side where they are further apart than the points (the low end)
    pub fn levels(&mut self, samples: &[f32], rate: f32, freqs: &[f32]) -> Vec<f32> {
        let n = samples.len().min(WINDOW);
        self.frame.fill(0.0);
        for i in 0..n {
            self.frame[i] = samples[samples.len() - n + i] * self.window[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.frame, &mut self.bins, &mut self.scratch)
            .is_err()
        {
            return vec![f32::NEG_INFINITY; freqs.len()];
        }
        // A full-scale sine reads WINDOW / 4 through the Hann window
        let scale = 4.0 / WINDOW as f32;
        for (p, b) in self.power.iter_mut().zip(&self.bins) {
            *p = b.norm_sqr() * scale * scale;
        }
        let hz_per_bin = rate / WINDOW as f32;
        let last = self.power.len() - 1;
        let bin = |f: f32| (f / hz_per_bin).clamp(0.0, last as f32);
        freqs
            .iter()
            .enumerate()
            .map(|(i, &f)| {
                let around = |j: Option<usize>| j.and_then(|j| freqs.get(j)).copied();
                let lo = around(i.checked_sub(1)).map_or(f, |p| (p * f).sqrt());
                let hi = around(Some(i + 1)).map_or(f, |n| (n * f).sqrt());
                let (a, b) = (bin(lo).ceil() as usize, bin(hi).floor() as usize);
                let power = if a <= b {
                    self.power[a..=b].iter().copied().fold(0.0, f32::max)
                } else {
                    let x = bin(f);
                    let (k, t) = (x.floor() as usize, x.fract());
                    self.power[k] * (1.0 - t) + self.power[(k + 1).min(last)] * t
                };
                10.0 * power.max(1e-20).log10()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_sine_reads_0_db_at_its_frequency() {
        let rate = 48_000.0;
        let f0 = 1000.0;
        let sine: Vec<f32> = (0..WINDOW)
            .map(|i| (2.0 * std::f32::consts::PI * f0 * i as f32 / rate).sin())
            .collect();
        let freqs = [100.0, 500.0, 1000.0, 2000.0, 10_000.0];
        let levels = Analyzer::default().levels(&sine, rate, &freqs);
        assert!(levels[2].abs() < 1.5, "{levels:?}");
        assert!(levels[0] < -60.0 && levels[4] < -60.0, "{levels:?}");
    }

    #[test]
    fn the_tap_reads_what_the_watched_block_wrote() {
        let tap = BlockTap::new();
        tap.watch(Some(7));
        assert!(tap.watches(7) && !tap.watches(8));
        let input: Vec<f32> = (0..300).map(|i| i as f32).collect();
        let output: Vec<f32> = input.iter().map(|s| -s).collect();
        tap.record_input(&input, &input, false);
        tap.record_output(&output, &output, false);
        let (mut i, mut o) = (vec![0.0; 4], vec![0.0; 4]);
        tap.read(&mut i, &mut o);
        assert_eq!(i, [296.0, 297.0, 298.0, 299.0]);
        assert_eq!(o, [-296.0, -297.0, -298.0, -299.0]);
    }
}
