//! Small DSP pieces of the built-in blocks: filters, tone stack, noise gate.
//!
//! The tone stack and the noise gate follow the ones of the Neural Amp
//! Modeler plugin (MIT), so captures react to them the same way.

use std::f64::consts::PI;

/// Second order IIR filter (RBJ audio EQ cookbook), transposed direct form
/// II. Computed in f64: low shelves at 48 kHz are noisy in f32.
#[derive(Clone, Debug)]
pub struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b: [1.0, 0.0, 0.0],
            a: [0.0, 0.0],
            z: [0.0; 2],
        }
    }
}

/// Filter shapes, with their frequency in Hz
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    LowShelf { freq: f32, q: f32, gain_db: f32 },
    HighShelf { freq: f32, q: f32, gain_db: f32 },
    Peak { freq: f32, q: f32, gain_db: f32 },
    HighPass { freq: f32 },
    LowPass { freq: f32 },
}

impl Biquad {
    /// Change the shape, keeping the filter state (no click)
    pub fn set(&mut self, shape: Shape, sample_rate: f32) {
        let sr = sample_rate as f64;
        let w = |freq: f32| 2.0 * PI * (freq as f64).clamp(1.0, 0.45 * sr) / sr;
        let butterworth = std::f64::consts::FRAC_1_SQRT_2;
        let (b, a) = match shape {
            Shape::LowShelf { freq, q, gain_db } | Shape::HighShelf { freq, q, gain_db } => {
                let w0 = w(freq);
                let (cos, alpha) = (w0.cos(), w0.sin() / (2.0 * q as f64));
                let a = 10f64.powf(gain_db as f64 / 40.0);
                let k = 2.0 * a.sqrt() * alpha;
                let sign = if matches!(shape, Shape::LowShelf { .. }) {
                    1.0
                } else {
                    -1.0
                };
                // A high shelf is the low shelf with the cos sign flipped
                let c = sign * cos;
                (
                    [
                        a * ((a + 1.0) - (a - 1.0) * c + k),
                        sign * 2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                        a * ((a + 1.0) - (a - 1.0) * c - k),
                    ],
                    [
                        (a + 1.0) + (a - 1.0) * c + k,
                        -sign * 2.0 * ((a - 1.0) + (a + 1.0) * c),
                        (a + 1.0) + (a - 1.0) * c - k,
                    ],
                )
            }
            Shape::Peak { freq, q, gain_db } => {
                let w0 = w(freq);
                let (cos, alpha) = (w0.cos(), w0.sin() / (2.0 * q as f64));
                let a = 10f64.powf(gain_db as f64 / 40.0);
                (
                    [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                    [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
                )
            }
            Shape::HighPass { freq } | Shape::LowPass { freq } => {
                let w0 = w(freq);
                let (cos, alpha) = (w0.cos(), w0.sin() / (2.0 * butterworth));
                let b = if matches!(shape, Shape::HighPass { .. }) {
                    let h = (1.0 + cos) / 2.0;
                    [h, -2.0 * h, h]
                } else {
                    let l = (1.0 - cos) / 2.0;
                    [l, 2.0 * l, l]
                };
                (b, [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
            }
        };
        self.b = b.map(|x| x / a[0]);
        self.a = [a[1] / a[0], a[2] / a[0]];
    }

    pub fn reset(&mut self) {
        self.z = [0.0; 2];
    }

    pub fn process(&mut self, buffer: &mut [f32]) {
        let ([b0, b1, b2], [a1, a2]) = (self.b, self.a);
        let [mut z1, mut z2] = self.z;
        for sample in buffer {
            let x = *sample as f64;
            let y = b0 * x + z1;
            z1 = b1 * x - a1 * y + z2;
            z2 = b2 * x - a2 * y;
            *sample = y as f32;
        }
        // Flush denormals left by a decaying tail
        self.z = [z1, z2].map(|z| if z.abs() < 1e-20 { 0.0 } else { z });
    }
}

/// A biquad following a control value, recomputed only when it changes
#[derive(Debug)]
struct Band {
    filter: Biquad,
    value: f32,
}

impl Band {
    fn new() -> Self {
        Self {
            filter: Biquad::default(),
            value: f32::NAN,
        }
    }

    /// Update the shape for `value`; returns false when the band is flat
    fn update(
        &mut self,
        value: f32,
        sample_rate: f32,
        shape: impl Fn(f32) -> Option<Shape>,
    ) -> bool {
        let active = shape(value);
        if value != self.value {
            match active {
                Some(shape) => self.filter.set(shape, sample_rate),
                None => self.filter.reset(),
            }
            self.value = value;
        }
        active.is_some()
    }
}

/// Bass / middle / treble of the NAM plugin: knobs 0…10, flat at 5
#[derive(Debug)]
pub struct ToneStack {
    sample_rate: f32,
    bands: [Band; 3],
}

impl ToneStack {
    pub const FLAT: f32 = 5.0;

    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            bands: [Band::new(), Band::new(), Band::new()],
        }
    }

    /// Clear the filter state, when the stack moves in the signal path
    pub fn reset(&mut self) {
        for band in &mut self.bands {
            band.filter.reset();
        }
    }

    pub fn process(&mut self, buffer: &mut [f32], bass: f32, middle: f32, treble: f32) {
        let sr = self.sample_rate;
        let [b, m, t] = &mut self.bands;
        // Flat bands are skipped: a centred knob costs nothing
        let flat = |v: f32| (v - Self::FLAT).abs() < 1e-3;
        let bands = [
            b.update(bass, sr, |v| {
                (!flat(v)).then_some(Shape::LowShelf {
                    freq: 150.0,
                    q: 0.707,
                    gain_db: 4.0 * (v - Self::FLAT),
                })
            }),
            m.update(middle, sr, |v| {
                let gain_db = 3.0 * (v - Self::FLAT);
                (!flat(v)).then_some(Shape::Peak {
                    freq: 425.0,
                    // Narrower cut than boost, as in the NAM plugin
                    q: if gain_db < 0.0 { 1.5 } else { 0.7 },
                    gain_db,
                })
            }),
            t.update(treble, sr, |v| {
                (!flat(v)).then_some(Shape::HighShelf {
                    freq: 1800.0,
                    q: 0.707,
                    gain_db: 2.0 * (v - Self::FLAT),
                })
            }),
        ];
        for (band, active) in self.bands.iter_mut().zip(bands) {
            if active {
                band.filter.process(buffer);
            }
        }
    }
}

/// Low and high cut of the impulse response blocks; a cut at the end of
/// its range is off
#[derive(Debug)]
pub struct Cuts {
    sample_rate: f32,
    low: Band,
    high: Band,
}

impl Cuts {
    pub const LOW_OFF: f32 = 20.0;
    pub const HIGH_OFF: f32 = 20000.0;

    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            low: Band::new(),
            high: Band::new(),
        }
    }

    pub fn process(&mut self, buffer: &mut [f32], low_cut: f32, high_cut: f32) {
        let sr = self.sample_rate;
        if self.low.update(low_cut, sr, |f| {
            (f > Self::LOW_OFF).then_some(Shape::HighPass { freq: f })
        }) {
            self.low.filter.process(buffer);
        }
        if self.high.update(high_cut, sr, |f| {
            (f < Self::HIGH_OFF).then_some(Shape::LowPass { freq: f })
        }) {
            self.high.filter.process(buffer);
        }
    }
}

/// Noise gate of the NAM plugin: the level is measured on the clean input of
/// the block and the gain applied after the model, so the gate follows the
/// playing, not the hiss the model adds. Below the threshold the gain drops
/// by `RATIO * (dB below)^2`.
#[derive(Debug)]
pub struct NoiseGate {
    /// Mean square of the input over ~10 ms
    level: f32,
    level_coef: f32,
    /// Current gain in dB (0 = open)
    gain_db: f32,
    open_step: f32,
    close_step: f32,
    hold: usize,
    hold_samples: usize,
}

impl NoiseGate {
    const AVERAGE_SECONDS: f32 = 0.01;
    const RATIO: f32 = 0.1;
    const MAX_REDUCTION_DB: f32 = -120.0;
    const OPEN_SECONDS: f32 = 0.005;
    const HOLD_SECONDS: f32 = 0.01;
    const CLOSE_SECONDS: f32 = 0.05;

    pub fn new(sample_rate: f32) -> Self {
        let range = -Self::MAX_REDUCTION_DB;
        Self {
            level: 0.0,
            level_coef: 1.0 - (-1.0 / (Self::AVERAGE_SECONDS * sample_rate)).exp(),
            gain_db: 0.0,
            open_step: range / (Self::OPEN_SECONDS * sample_rate),
            close_step: range / (Self::CLOSE_SECONDS * sample_rate),
            hold: 0,
            hold_samples: (Self::HOLD_SECONDS * sample_rate) as usize,
        }
    }

    /// Open the gate at once (when it is switched off)
    pub fn reset(&mut self) {
        self.gain_db = 0.0;
        self.hold = 0;
    }

    /// Gate `output` according to the level of `detector` (same length)
    pub fn process(&mut self, detector: &[f32], output: &mut [f32], threshold_db: f32) {
        // Logs and powers only when needed: the level is compared as power
        let threshold = 10f32.powf(threshold_db / 10.0);
        let (mut gain_db, mut gain) = (0.0f32, 1.0f32);
        for (x, out) in detector.iter().zip(output) {
            self.level += self.level_coef * (x * x - self.level);
            let target = if self.level < threshold {
                let below = 10.0 * (self.level + 1e-20).log10() - threshold_db;
                (-Self::RATIO * below * below).max(Self::MAX_REDUCTION_DB)
            } else {
                0.0
            };
            if target >= self.gain_db {
                self.hold = self.hold_samples;
                self.gain_db = (self.gain_db + self.open_step).min(target);
            } else if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.gain_db = (self.gain_db - self.close_step).max(target);
            }
            if self.gain_db < 0.0 {
                if self.gain_db != gain_db {
                    gain_db = self.gain_db;
                    gain = 10f32.powf(gain_db / 20.0);
                }
                *out *= gain;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn sine(freq: f32, amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / SR).sin())
            .collect()
    }

    /// Peak of the second half (after the filter settled)
    fn peak(buffer: &[f32]) -> f32 {
        buffer[buffer.len() / 2..]
            .iter()
            .fold(0.0, |m, x| m.max(x.abs()))
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    #[test]
    fn tone_stack_is_flat_at_noon_and_shapes_bands() {
        let mut stack = ToneStack::new(SR);
        let mut flat = sine(1000.0, 0.5, 4800);
        let reference = flat.clone();
        stack.process(&mut flat, 5.0, 5.0, 5.0);
        assert_eq!(flat, reference);

        let gain_at = |freq: f32, bass: f32, middle: f32, treble: f32| {
            let mut stack = ToneStack::new(SR);
            let mut buffer = sine(freq, 0.5, 9600);
            stack.process(&mut buffer, bass, middle, treble);
            db(peak(&buffer) / 0.5)
        };
        // Bass at 10: +20 dB shelf, well below its corner
        assert!((gain_at(40.0, 10.0, 5.0, 5.0) - 20.0).abs() < 1.5);
        assert!(gain_at(5000.0, 10.0, 5.0, 5.0).abs() < 1.0);
        // Middle at 0: -15 dB at 425 Hz
        assert!((gain_at(425.0, 5.0, 0.0, 5.0) + 15.0).abs() < 1.0);
        // Treble at 10: +10 dB high up
        assert!((gain_at(12000.0, 5.0, 5.0, 10.0) - 10.0).abs() < 1.0);
    }

    #[test]
    fn cuts_filter_the_ends() {
        let gain_at = |freq: f32, low: f32, high: f32| {
            let mut cuts = Cuts::new(SR);
            let mut buffer = sine(freq, 0.5, 9600);
            cuts.process(&mut buffer, low, high);
            db(peak(&buffer) / 0.5)
        };
        assert!(gain_at(50.0, Cuts::LOW_OFF, Cuts::HIGH_OFF).abs() < 0.01);
        assert!(gain_at(50.0, 200.0, Cuts::HIGH_OFF) < -20.0);
        assert!(gain_at(1000.0, 200.0, 5000.0).abs() < 0.5);
        assert!(gain_at(12000.0, 20.0, 5000.0) < -12.0);
        // -3 dB at the corner (Butterworth)
        assert!((gain_at(5000.0, 20.0, 5000.0) + 3.0).abs() < 0.5);
    }

    #[test]
    fn gate_closes_on_noise_and_opens_on_playing() {
        let mut gate = NoiseGate::new(SR);
        // Hiss at -70 dB with a -50 dB threshold: closed
        let noise = sine(3000.0, 10f32.powf(-70.0 / 20.0), 9600);
        let mut out = vec![1.0; noise.len()];
        gate.process(&noise, &mut out, -50.0);
        // ~23 dB below: -0.1 * 23^2 = -53 dB
        assert!(db(out[out.len() - 1]) < -50.0);
        // A note at -20 dB: open within a few ms
        let note = sine(200.0, 0.1, 960);
        let mut out = vec![1.0; note.len()];
        gate.process(&note, &mut out, -50.0);
        assert!((out[out.len() - 1] - 1.0).abs() < 1e-6);
        // Off, as with a threshold far below the signal
        let mut out = vec![1.0; noise.len()];
        gate.process(&noise, &mut out, -150.0);
        assert!(out.iter().all(|&x| x == 1.0));
    }
}
