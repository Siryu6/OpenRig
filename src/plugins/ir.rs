//! Impulse responses (`.wav`): cabinets, and rooms / reverbs ("spaces").
//!
//! The file is decoded (one side of a stereo file, or both mixed to mono),
//! resampled to the audio rate and trimmed off the audio thread. The convolution has no latency: the start
//! of the response is convolved in the audio callback, block by block; the
//! tail of long responses (rooms) is convolved by a worker thread, which has
//! a whole tail block of time to do it, so the callback cost stays flat.

use std::cell::UnsafeCell;
use std::f64::consts::PI;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Context, Result};
use fft_convolver::FFTConvolver;

/// Longest impulse response kept (long reverbs are cut)
const MAX_SECONDS: f32 = 10.0;
/// Trailing samples quieter than this (relative to the peak) are dropped
const TRIM_THRESHOLD: f32 = 1e-4; // -80 dB
/// Half length of the resampling filter, in input samples
const SINC_HALF_TAPS: i64 = 32;
/// Block size of the tail convolution. The head covers the first two tail
/// blocks of the response (170 ms at 48 kHz: any cabinet), so the worker
/// has one tail block (85 ms) to deliver each result.
const TAIL_BLOCK: usize = 4096;

/// An impulse response ready to be convolved
pub struct ImpulseResponse {
    pub samples: Vec<f32>,
    /// Gain giving the impulse response unity gain over the guitar band, so
    /// that captures of different loudness sound alike
    pub normalize_gain: f32,
}

impl ImpulseResponse {
    /// Read a WAV file and prepare it for `sample_rate`. `channel` picks one
    /// side of a stereo response (the last one if the file has fewer);
    /// `None` averages the channels into a mono response. Not real-time safe.
    pub fn load(path: &Path, sample_rate: f32, channel: Option<usize>) -> Result<Self> {
        let reader = hound::WavReader::open(path)
            .with_context(|| format!("Cannot read impulse response {}", path.display()))?;
        let spec = reader.spec();
        let channels = spec.channels.max(1) as usize;
        let interleaved: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .into_samples::<i32>()
                    .map(|s| s.map(|s| s as f32 * scale))
                    .collect::<Result<_, _>>()?
            }
        };
        let side = |c: usize| -> Vec<f32> { interleaved.chunks(channels).map(|f| f[c]).collect() };
        let rate = spec.sample_rate as f32;
        let energy = |samples: &[f32]| band_energy(samples, rate);
        let (samples, side_energy) = match channel {
            Some(c) => {
                let samples = side(c.min(channels - 1));
                let e = energy(&samples);
                (samples, Some(e))
            }
            None => (
                interleaved
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect(),
                None,
            ),
        };
        let mut response = Self::from_samples(samples, spec.sample_rate as f32, sample_rate)
            .with_context(|| format!("Invalid impulse response {}", path.display()))?;
        if let Some(e) = side_energy.filter(|&e| e > 0.0 && channels > 1) {
            // Both sides get the same gain (from their mean energy), so
            // normalizing keeps the stereo image
            let mean = (0..channels).map(|c| energy(&side(c))).sum::<f32>() / channels as f32;
            response.normalize_gain *= (e / mean).sqrt();
        }
        Ok(response)
    }

    /// Number of channels of a WAV file (1 if it cannot be read)
    pub fn channel_count(path: &Path) -> usize {
        hound::WavReader::open(path).map_or(1, |r| r.spec().channels.max(1) as usize)
    }

    /// Prepare raw mono samples recorded at `file_rate` for `sample_rate`
    pub fn from_samples(samples: Vec<f32>, file_rate: f32, sample_rate: f32) -> Result<Self> {
        let mut samples = if (file_rate - sample_rate).abs() > 0.5 {
            resample(&samples, file_rate as f64, sample_rate as f64)
        } else {
            samples
        };
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if !(peak > 0.0 && peak.is_finite()) {
            return Err(anyhow!("the file is silent"));
        }
        let end = samples
            .iter()
            .rposition(|s| s.abs() > peak * TRIM_THRESHOLD)
            .map_or(0, |i| i + 1);
        samples.truncate(end.min((MAX_SECONDS * sample_rate) as usize));

        let energy = band_energy(&samples, sample_rate);
        if !energy.is_finite() || energy <= 0.0 {
            return Err(anyhow!("the file has no sound in the guitar range"));
        }
        Ok(Self {
            normalize_gain: 1.0 / energy.sqrt(),
            samples,
        })
    }

    /// Convolver for this response, processing blocks of up to `max_block`
    pub fn convolver(&self, max_block: usize) -> Result<Convolver> {
        let error = |e| anyhow!("Cannot set up the convolution: {e:?}");
        let head_len = self.samples.len().min(2 * TAIL_BLOCK);
        let mut head = FFTConvolver::default();
        head.init(max_block.max(1), &self.samples[..head_len])
            .map_err(error)?;
        let tail = if self.samples.len() > head_len {
            let mut convolver = FFTConvolver::default();
            convolver
                .init(TAIL_BLOCK, &self.samples[head_len..])
                .map_err(error)?;
            Some(Tail::start(convolver)?)
        } else {
            None
        };
        Ok(Convolver { head, tail })
    }
}

/// Zero-latency convolution, real-time safe (no allocation, no lock)
pub struct Convolver {
    head: FFTConvolver<f32>,
    tail: Option<Tail>,
}

impl Convolver {
    /// Convolve `input` into `output` (same length)
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        if self.head.process(input, output).is_err() {
            output.fill(0.0);
        }
        if let Some(tail) = &mut self.tail {
            tail.process(input, output);
        }
    }
}

const IDLE: u8 = 0;
/// A block was handed to the worker
const PENDING: u8 = 1;
/// The worker has written the result
const DONE: u8 = 2;

/// Buffers exchanged with the worker. `state` says who owns them: the
/// audio thread when IDLE or DONE, the worker when PENDING.
struct Shared {
    input: UnsafeCell<Box<[f32]>>,
    output: UnsafeCell<Box<[f32]>>,
    state: AtomicU8,
    stop: AtomicBool,
}

// Access to the buffers is serialized by `state` (acquire / release).
unsafe impl Sync for Shared {}

/// Tail of the response, starting `2 * TAIL_BLOCK` samples in. Input block
/// k is handed over when complete, at k+1; its result is only needed from
/// k+2, which leaves the worker a whole block of time.
struct Tail {
    input: Box<[f32]>,
    output: Box<[f32]>,
    /// Position in the current block (input and output alike)
    pos: usize,
    /// The worker missed a block boundary: its next result is a block late
    late: bool,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Tail {
    fn start(mut convolver: FFTConvolver<f32>) -> Result<Self> {
        let block = || vec![0.0f32; TAIL_BLOCK].into_boxed_slice();
        let shared = Arc::new(Shared {
            input: UnsafeCell::new(block()),
            output: UnsafeCell::new(block()),
            state: AtomicU8::new(IDLE),
            stop: AtomicBool::new(false),
        });
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("ir-tail".into())
            .spawn(move || {
                let shared = worker_shared;
                crate::audio::flush_denormals();
                loop {
                    while shared.state.load(Ordering::Acquire) != PENDING {
                        if shared.stop.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::park();
                    }
                    // PENDING: the worker owns the buffers
                    let (input, output) =
                        unsafe { (&*shared.input.get(), &mut *shared.output.get()) };
                    if convolver.process(input, output).is_err() {
                        output.fill(0.0);
                    }
                    shared.state.store(DONE, Ordering::Release);
                }
            })?;
        Ok(Self {
            input: block(),
            output: block(),
            pos: 0,
            late: false,
            shared,
            worker: Some(worker),
        })
    }

    /// Add the tail to `output` and collect `input` for the worker
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        for (x, y) in input.iter().zip(output.iter_mut()) {
            *y += self.output[self.pos];
            self.input[self.pos] = *x;
            self.pos += 1;
            if self.pos == TAIL_BLOCK {
                self.pos = 0;
                self.exchange();
            }
        }
    }

    /// Block boundary: take the previous result, hand over the new block
    fn exchange(&mut self) {
        match self.shared.state.load(Ordering::Acquire) {
            PENDING => {
                // Worker late (CPU overloaded): drop this block rather than
                // wait. The tail is wrong for a moment but the callback
                // never blocks.
                self.output.fill(0.0);
                self.late = true;
                return;
            }
            // The result of a late worker belongs to the block just played:
            // playing it now would put the tail one block out of time
            DONE if self.late => {
                self.output.fill(0.0);
                self.late = false;
            }
            DONE => self
                .output
                .copy_from_slice(unsafe { &*self.shared.output.get() }),
            _ => self.output.fill(0.0),
        }
        unsafe { &mut *self.shared.input.get() }.copy_from_slice(&self.input);
        self.shared.state.store(PENDING, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// Guitar band used to normalize responses
const BAND_LOW_HZ: f32 = 80.0;
const BAND_HIGH_HZ: f32 = 5000.0;
const BAND_POINTS: usize = 48;

/// Mean power gain of `samples` over the guitar band, with points evenly
/// spread in log frequency (pink noise). The energy of the whole response
/// would count the treble a cabinet cuts: in the band, a cabinet normalized
/// that way is 5 to 10 dB louder than the dry signal.
fn band_energy(samples: &[f32], sample_rate: f32) -> f32 {
    let high = BAND_HIGH_HZ.min(sample_rate * 0.45);
    let total: f64 = (0..BAND_POINTS)
        .map(|i| {
            let t = i as f32 / (BAND_POINTS - 1) as f32;
            let freq = BAND_LOW_HZ * (high / BAND_LOW_HZ).powf(t);
            let step = std::f64::consts::TAU * freq as f64 / sample_rate as f64;
            let (sin, cos) = step.sin_cos();
            // Rotate the phasor instead of calling sin/cos per sample
            let (mut re, mut im) = (1.0f64, 0.0f64);
            let (mut sum_re, mut sum_im) = (0.0f64, 0.0f64);
            for &s in samples {
                sum_re += s as f64 * re;
                sum_im -= s as f64 * im;
                (re, im) = (re * cos - im * sin, re * sin + im * cos);
            }
            sum_re * sum_re + sum_im * sum_im
        })
        .sum();
    (total / BAND_POINTS as f64) as f32
}

/// Windowed-sinc resampling (offline, for impulse responses)
fn resample(input: &[f32], from: f64, to: f64) -> Vec<f32> {
    let ratio = to / from;
    // Lowpass below the lower Nyquist frequency when downsampling
    let cutoff = ratio.min(1.0);
    let half = SINC_HALF_TAPS as f64;
    let len = (input.len() as f64 * ratio).ceil() as usize;
    (0..len)
        .map(|n| {
            let t = n as f64 / ratio;
            let center = t.floor() as i64;
            let mut acc = 0.0;
            for k in center - SINC_HALF_TAPS + 1..=center + SINC_HALF_TAPS {
                let Some(&x) = usize::try_from(k).ok().and_then(|k| input.get(k)) else {
                    continue;
                };
                let d = t - k as f64;
                let sinc = if d == 0.0 {
                    1.0
                } else {
                    (PI * cutoff * d).sin() / (PI * cutoff * d)
                };
                // Blackman window over [-half, half]
                let w = 0.42 + 0.5 * (PI * d / half).cos() + 0.08 * (2.0 * PI * d / half).cos();
                acc += x as f64 * cutoff * sinc * w.max(0.0);
            }
            acc as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: u16, frames: &[Vec<f32>]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in frames {
            for &s in frame {
                writer.write_sample((s * 8_388_607.0) as i32).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pedalboard-ir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn loads_stereo_24_bit_wav_and_trims_silence() {
        let path = temp("cab.wav");
        let mut frames = vec![vec![0.0, 0.0]; 1000];
        frames[0] = vec![1.0, 0.0];
        frames[1] = vec![0.5, 0.5];
        write_wav(&path, 48000, 2, &frames);

        let ir = ImpulseResponse::load(&path, 48000.0, None).unwrap();
        assert_eq!(ir.samples.len(), 2);
        assert!((ir.samples[0] - 0.5).abs() < 1e-5);
        assert!((ir.samples[1] - 0.5).abs() < 1e-5);
        // Averaging two samples is flat over the guitar band: no gain change
        assert!(
            (ir.normalize_gain - 1.0).abs() < 0.05,
            "{}",
            ir.normalize_gain
        );

        // Each side on its own, normalized with the same gain
        assert_eq!(ImpulseResponse::channel_count(&path), 2);
        let left = ImpulseResponse::load(&path, 48000.0, Some(0)).unwrap();
        let right = ImpulseResponse::load(&path, 48000.0, Some(1)).unwrap();
        assert!((left.samples[0] - 1.0).abs() < 1e-5);
        assert!((right.samples[0]).abs() < 1e-5 && (right.samples[1] - 0.5).abs() < 1e-5);
        assert!((left.normalize_gain - right.normalize_gain).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn normalizes_over_the_guitar_band() {
        // A dirac is left as it is, whatever its level
        let dirac = ImpulseResponse::from_samples(vec![0.25], 48000.0, 48000.0).unwrap();
        assert!((dirac.normalize_gain - 4.0).abs() < 1e-3);

        // A dull response (8-sample average, cut above ~2.5 kHz) is loud in
        // the band for its energy: it is turned down further
        let dull = ImpulseResponse::from_samples(vec![0.125; 8], 48000.0, 48000.0).unwrap();
        let unit_energy = 1.0 / (8.0f32 * 0.125 * 0.125).sqrt();
        assert!(
            dull.normalize_gain < unit_energy * 0.7,
            "{}",
            dull.normalize_gain
        );
        assert!(dull.normalize_gain > 1.0);
    }

    #[test]
    fn resamples_to_the_audio_rate() {
        // A 1 kHz sine keeps its frequency (and roughly its level)
        let input: Vec<f32> = (0..4410)
            .map(|i| (i as f32 * 1000.0 * std::f32::consts::TAU / 44100.0).sin())
            .collect();
        let output = resample(&input, 44100.0, 48000.0);
        assert_eq!(output.len(), 4800);
        let expected: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 1000.0 * std::f32::consts::TAU / 48000.0).sin())
            .collect();
        let error = output[100..4700]
            .iter()
            .zip(&expected[100..4700])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(error < 0.01, "max error {error}");
    }

    #[test]
    fn convolves_without_latency() {
        let ir = ImpulseResponse::from_samples(vec![1.0, 0.5, 0.25], 48000.0, 48000.0).unwrap();
        let mut convolver = ir.convolver(64).unwrap();
        let mut input = vec![0.0f32; 64];
        input[0] = 1.0;
        let mut output = vec![0.0f32; 64];
        convolver.process(&input, &mut output);
        assert!((output[0] - 1.0).abs() < 1e-5);
        assert!((output[1] - 0.5).abs() < 1e-5);
        assert!((output[2] - 0.25).abs() < 1e-5);
        assert!(output[3].abs() < 1e-5);
    }

    #[test]
    fn long_responses_match_direct_convolution() {
        // Long enough to use the worker thread for the tail
        let len = 3 * TAIL_BLOCK + 123;
        let samples: Vec<f32> = (0..len)
            .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
            .collect();
        let ir = ImpulseResponse::from_samples(samples.clone(), 48000.0, 48000.0).unwrap();
        let mut convolver = ir.convolver(128).unwrap();
        assert!(convolver.tail.is_some());

        let total = len + 2 * TAIL_BLOCK;
        let input: Vec<f32> = (0..total)
            .map(|i| if i % 1000 == 0 { 1.0 } else { 0.0 })
            .collect();
        let mut output = vec![0.0f32; total];
        for (i, o) in input.chunks(100).zip(output.chunks_mut(100)) {
            convolver.process(i, o);
            // Real time gives the worker 85 ms per block; leave it time here
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        for n in (0..total).step_by(37) {
            let expected: f32 = (0..=n.min(len - 1))
                .map(|k| samples[k] * input[n - k])
                .sum();
            assert!(
                (output[n] - expected).abs() < 1e-3,
                "sample {n}: {} vs {expected}",
                output[n]
            );
        }
    }

    #[test]
    fn rejects_silent_files() {
        assert!(ImpulseResponse::from_samples(vec![0.0; 100], 48000.0, 48000.0).is_err());
        assert!(ImpulseResponse::load(Path::new("/nonexistent.wav"), 48000.0, None).is_err());
    }
}
