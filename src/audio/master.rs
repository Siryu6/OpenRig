//! Output EQ and limiter: the last stage of each output (or stereo pair),
//! after its level, so the player can fit the room or the front of house
//! without touching a preset, and never send clipping to the PA.
//!
//! Set per strip in `routing.json` ([`OutputEq`], [`Limiter`]). Off the
//! audio thread, [`target`] turns them into filter parameters and limiter
//! gains ([`MasterTarget`], plain numbers sent whole with the routing); on
//! the audio thread, [`MasterProc`] runs them without allocating or locking.
//!
//! The EQ bands are second-order filters with the responses of the RBJ
//! cookbook biquads (the curve drawn in the Routing view), run as
//! trapezoidal state-variable filters: their parameters are smoothed
//! sample by sample on a change, and stay stable while they move. A band at
//! 0 dB, or switched off, costs nothing.
//!
//! The limiter is a lookahead peak limiter: the signal is delayed by
//! [`LOOKAHEAD_MS`] while the gain it needs is computed, held over that
//! time, and ramped down in time for the loudest sample. No sample leaves
//! above the ceiling.

use serde::{Deserialize, Serialize};

/// Delay of a limited output: the time the gain has to come down
pub const LOOKAHEAD_MS: f32 = 1.0;
/// Ceiling range, dBFS
pub const CEILING_RANGE_DB: (f32, f32) = (-12.0, 0.0);
/// Release time range, ms
pub const RELEASE_RANGE_MS: (f32, f32) = (10.0, 1000.0);
/// Band gain range, dB
pub const EQ_GAIN_RANGE_DB: (f32, f32) = (-12.0, 12.0);
/// Q range of the mid bands
pub const Q_RANGE: (f32, f32) = (0.3, 8.0);
/// Frequency range of every band, Hz
pub const FREQ_RANGE: (f32, f32) = (20.0, 20_000.0);
/// Time constant of the filter parameters following a change
const SMOOTH_MS: f32 = 5.0;
/// Limiter on/off crossfade
const FADE_MS: f32 = 10.0;
/// Q of the cuts and the shelves: Butterworth, and shelves without overshoot
const FLAT_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The bands, low to high
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    /// High-pass, 12 dB per octave
    LowCut,
    LowShelf,
    Mid,
    HighShelf,
    /// Low-pass, 12 dB per octave
    HighCut,
}

impl BandKind {
    /// Gain and Q apply to it
    pub fn has_gain(self) -> bool {
        !matches!(self, BandKind::LowCut | BandKind::HighCut)
    }

    pub fn has_q(self) -> bool {
        self == BandKind::Mid
    }
}

/// Bands of an output EQ
pub const BANDS: usize = 6;
/// Kind and name of each band, in [`OutputEq::bands`] order
pub const BAND_KINDS: [(BandKind, &str); BANDS] = [
    (BandKind::LowCut, "Low cut"),
    (BandKind::LowShelf, "Low"),
    (BandKind::Mid, "Mid 1"),
    (BandKind::Mid, "Mid 2"),
    (BandKind::HighShelf, "High"),
    (BandKind::HighCut, "High cut"),
];

/// One band of the output EQ
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub on: bool,
    pub freq: f32,
    /// Shelves and mids
    pub gain_db: f32,
    /// Mids
    pub q: f32,
}

impl EqBand {
    const fn new(on: bool, freq: f32) -> Self {
        Self {
            on,
            freq,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

impl Default for EqBand {
    fn default() -> Self {
        Self::new(true, 1000.0)
    }
}

impl EqBand {
    /// The band as the RBJ cookbook biquad of its shape, for drawing (see
    /// [`SvfParams::of`]: the same response)
    pub fn curve(&self, kind: BandKind) -> crate::plugins::eq_curve::BandState {
        use crate::plugins::eq_curve::{BandState, QUnit, Shape};
        BandState {
            shape: match kind {
                BandKind::LowCut => Shape::Highpass,
                BandKind::HighCut => Shape::Lowpass,
                BandKind::LowShelf => Shape::Lowshelf,
                BandKind::HighShelf => Shape::Highshelf,
                BandKind::Mid => Shape::Peak,
            },
            on: self.on,
            freq: self.freq,
            gain: if kind.has_gain() { self.gain_db } else { 0.0 },
            q: if kind.has_q() { self.q } else { FLAT_Q },
            q_unit: QUnit::Q,
        }
    }
}

/// Global EQ of an output or pair: low cut, low shelf, two mids, high
/// shelf, high cut. Off and flat by default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputEq {
    pub on: bool,
    pub low_cut: EqBand,
    pub low_shelf: EqBand,
    pub mid_1: EqBand,
    pub mid_2: EqBand,
    pub high_shelf: EqBand,
    pub high_cut: EqBand,
}

impl Default for OutputEq {
    fn default() -> Self {
        Self {
            on: false,
            low_cut: EqBand::new(false, 80.0),
            low_shelf: EqBand::new(true, 120.0),
            mid_1: EqBand::new(true, 400.0),
            mid_2: EqBand::new(true, 2500.0),
            high_shelf: EqBand::new(true, 6000.0),
            high_cut: EqBand::new(false, 10_000.0),
        }
    }
}

impl OutputEq {
    /// The bands, in [`BAND_KINDS`] order
    pub fn bands(&self) -> [EqBand; BANDS] {
        [
            self.low_cut,
            self.low_shelf,
            self.mid_1,
            self.mid_2,
            self.high_shelf,
            self.high_cut,
        ]
    }

    pub fn band_mut(&mut self, band: usize) -> Option<&mut EqBand> {
        Some(match band {
            0 => &mut self.low_cut,
            1 => &mut self.low_shelf,
            2 => &mut self.mid_1,
            3 => &mut self.mid_2,
            4 => &mut self.high_shelf,
            5 => &mut self.high_cut,
            _ => return None,
        })
    }

    /// Every band back to flat, the cuts off; on/off kept
    pub fn flatten(&mut self) {
        *self = Self {
            on: self.on,
            ..Self::default()
        };
    }

    /// Every value finite
    pub fn is_valid(&self) -> bool {
        self.bands()
            .iter()
            .all(|b| b.freq.is_finite() && b.gain_db.is_finite() && b.q.is_finite())
    }

    /// Values within their ranges
    pub fn clamp(&mut self) {
        for band in 0..BANDS {
            if let Some(b) = self.band_mut(band) {
                b.freq = b.freq.clamp(FREQ_RANGE.0, FREQ_RANGE.1);
                b.gain_db = b.gain_db.clamp(EQ_GAIN_RANGE_DB.0, EQ_GAIN_RANGE_DB.1);
                b.q = b.q.clamp(Q_RANGE.0, Q_RANGE.1);
            }
        }
    }
}

/// Safety limiter of an output or pair. On by default, at a ceiling normal
/// levels never reach.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limiter {
    pub on: bool,
    /// dBFS
    pub ceiling_db: f32,
    pub release_ms: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            on: true,
            ceiling_db: -1.0,
            release_ms: 100.0,
        }
    }
}

impl Limiter {
    pub fn is_valid(&self) -> bool {
        self.ceiling_db.is_finite() && self.release_ms.is_finite()
    }

    pub fn clamp(&mut self) {
        self.ceiling_db = self
            .ceiling_db
            .clamp(CEILING_RANGE_DB.0, CEILING_RANGE_DB.1);
        self.release_ms = self
            .release_ms
            .clamp(RELEASE_RANGE_MS.0, RELEASE_RANGE_MS.1);
    }
}

/// Parameters of a trapezoidal state-variable filter (A. Simper, Cytomic):
/// cutoff `g`, damping `k`, and the mix of its input, band-pass and
/// low-pass outputs. `m = (1, 0, 0)` passes the input as it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvfParams {
    pub g: f32,
    pub k: f32,
    pub m: [f32; 3],
}

impl SvfParams {
    pub const NEUTRAL: Self = Self {
        g: 0.1,
        k: 1.0,
        m: [1.0, 0.0, 0.0],
    };

    fn is_neutral(&self) -> bool {
        self.m == [1.0, 0.0, 0.0]
    }

    /// The band at `sample_rate`. Same response as the RBJ cookbook
    /// biquad of its shape (both are bilinear transforms of the same analog
    /// filter, warped at its frequency).
    pub fn of(kind: BandKind, band: &EqBand, sample_rate: f32) -> Self {
        let gain = band.gain_db.clamp(EQ_GAIN_RANGE_DB.0, EQ_GAIN_RANGE_DB.1);
        if !band.on || (kind.has_gain() && gain == 0.0) {
            return Self::NEUTRAL;
        }
        let rate = sample_rate.max(1.0) as f64;
        let freq = (band.freq as f64).clamp(FREQ_RANGE.0 as f64, rate * 0.45);
        let g = (std::f64::consts::PI * freq / rate).tan();
        let a = 10f64.powf(gain as f64 / 40.0);
        let q = match kind {
            BandKind::Mid => (band.q as f64).clamp(Q_RANGE.0 as f64, Q_RANGE.1 as f64),
            _ => FLAT_Q as f64,
        };
        let k = 1.0 / q;
        let (g, k, m) = match kind {
            BandKind::LowCut => (g, k, [1.0, -k, -1.0]),
            BandKind::HighCut => (g, k, [0.0, 0.0, 1.0]),
            BandKind::Mid => {
                let k = 1.0 / (q * a);
                (g, k, [1.0, k * (a * a - 1.0), 0.0])
            }
            BandKind::LowShelf => (g / a.sqrt(), k, [1.0, k * (a - 1.0), a * a - 1.0]),
            BandKind::HighShelf => (g * a.sqrt(), k, [a * a, k * (1.0 - a) * a, 1.0 - a * a]),
        };
        Self {
            g: g as f32,
            k: k as f32,
            m: m.map(|v| v as f32),
        }
    }
}

/// What the audio thread runs on an output or pair, computed off it
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MasterTarget {
    pub eq: [SvfParams; BANDS],
    pub limiter: bool,
    /// Linear
    pub ceiling: f32,
    /// Release time, ms
    pub release_ms: f32,
}

impl MasterTarget {
    /// Nothing: the output as it is
    pub const NONE: Self = Self {
        eq: [SvfParams::NEUTRAL; BANDS],
        limiter: false,
        ceiling: 1.0,
        release_ms: 100.0,
    };
}

/// The EQ and limiter of a strip, for the audio thread
pub fn target(eq: &OutputEq, limiter: &Limiter, sample_rate: f32) -> MasterTarget {
    let bands = eq.bands();
    let mut target = MasterTarget::NONE;
    if eq.on {
        for (slot, (band, (kind, _))) in target.eq.iter_mut().zip(bands.iter().zip(BAND_KINDS)) {
            *slot = SvfParams::of(kind, band, sample_rate);
        }
    }
    let mut l = *limiter;
    l.clamp();
    target.limiter = l.on;
    target.ceiling = 10f32.powf(l.ceiling_db / 20.0);
    target.release_ms = l.release_ms;
    target
}

/// One band running on up to two channels
#[derive(Debug, Clone, Copy)]
struct Band {
    current: SvfParams,
    /// a1, a2, a3 of `current`
    a: [f32; 3],
    /// ic1eq, ic2eq of each channel
    state: [[f32; 2]; 2],
    /// Neutral and steady: skipped, its state cleared
    idle: bool,
}

impl Band {
    const IDLE: Self = Self {
        current: SvfParams::NEUTRAL,
        a: [0.0; 3],
        state: [[0.0; 2]; 2],
        idle: true,
    };

    fn coefficients(p: &SvfParams) -> [f32; 3] {
        let a1 = 1.0 / (1.0 + p.g * (p.g + p.k));
        let a2 = p.g * a1;
        [a1, a2, p.g * a2]
    }

    #[inline]
    fn tick(a: [f32; 3], m: [f32; 3], s: &mut [f32; 2], v0: f32) -> f32 {
        let v3 = v0 - s[1];
        let v1 = a[0] * s[0] + a[1] * v3;
        let v2 = s[1] + a[1] * s[0] + a[2] * v3;
        s[0] = 2.0 * v1 - s[0];
        s[1] = 2.0 * v2 - s[1];
        m[0] * v0 + m[1] * v1 + m[2] * v2
    }

    /// Filter `channels` in place, moving towards `target` by `smooth` of
    /// the way each sample
    fn process(&mut self, target: &SvfParams, smooth: f32, channels: &mut [&mut [f32]]) {
        // Switched off: fade the band out where it is, without sweeping it
        let target = &if target.is_neutral() {
            SvfParams {
                m: SvfParams::NEUTRAL.m,
                ..self.current
            }
        } else {
            *target
        };
        if self.idle {
            if target.is_neutral() {
                return;
            }
            // Start from silence, at the new cutoff, fading the filter in
            *self = Self {
                current: SvfParams {
                    m: SvfParams::NEUTRAL.m,
                    ..*target
                },
                idle: false,
                ..Self::IDLE
            };
            self.a = Self::coefficients(&self.current);
        }
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if self.current == *target {
            if target.is_neutral() {
                *self = Self::IDLE;
                return;
            }
            for (c, samples) in channels.iter_mut().enumerate().take(2) {
                let s = &mut self.state[c];
                for x in samples[..n].iter_mut() {
                    *x = Self::tick(self.a, target.m, s, *x);
                }
            }
            return;
        }
        for i in 0..n {
            let p = &mut self.current;
            p.g += (target.g - p.g) * smooth;
            p.k += (target.k - p.k) * smooth;
            for (m, t) in p.m.iter_mut().zip(target.m) {
                *m += (t - *m) * smooth;
            }
            self.a = Self::coefficients(p);
            for (c, samples) in channels.iter_mut().enumerate().take(2) {
                samples[i] = Self::tick(self.a, p.m, &mut self.state[c], samples[i]);
            }
        }
        // Close enough: settle on the target exactly
        let p = &self.current;
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-5 * b.abs().max(1e-3);
        if close(p.g, target.g)
            && close(p.k, target.k)
            && p.m.iter().zip(target.m).all(|(a, b)| (a - b).abs() < 1e-5)
        {
            self.current = *target;
            self.a = Self::coefficients(target);
        }
    }
}

/// Lookahead peak limiter on up to two linked channels (one gain for both,
/// so a pair keeps its image)
struct PeakLimiter {
    /// Lookahead, samples
    delay: usize,
    /// Delayed input of each channel (ring of `delay`)
    lines: [Vec<f32>; 2],
    pos: usize,
    /// Running minimum of the gain needed over the last `delay + 1`
    /// samples: a monotonic queue of (sample count, gain) in a ring
    queue: Vec<(u64, f32)>,
    head: usize,
    len: usize,
    count: u64,
    /// After release
    envelope: f32,
    /// Moving average of the envelope over `delay + 1` samples
    average: Vec<f32>,
    average_pos: usize,
    sum: f64,
    /// 1 = limited (delayed), 0 = straight through; ramped on/off
    wet: f32,
    fade_step: f32,
    sample_rate: f32,
}

impl PeakLimiter {
    fn new(sample_rate: f32) -> Self {
        let delay = ((sample_rate * LOOKAHEAD_MS / 1000.0).round() as usize).max(1);
        let mut limiter = Self {
            delay,
            lines: [vec![0.0; delay], vec![0.0; delay]],
            pos: 0,
            queue: vec![(0, 1.0); delay + 1],
            head: 0,
            len: 0,
            count: 0,
            envelope: 1.0,
            average: vec![1.0; delay + 1],
            average_pos: 0,
            sum: 0.0,
            wet: 0.0,
            fade_step: 1.0 / (sample_rate * FADE_MS / 1000.0).max(1.0),
            sample_rate,
        };
        limiter.reset();
        limiter
    }

    /// Back to silence and no reduction (allocation-free)
    fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.pos = 0;
        self.head = 0;
        self.len = 0;
        self.envelope = 1.0;
        self.average.fill(1.0);
        self.average_pos = 0;
        self.sum = self.average.len() as f64;
    }

    /// Minimum of the gains pushed over the last `delay + 1` samples
    #[inline]
    fn push_min(&mut self, gain: f32) -> f32 {
        let size = self.queue.len();
        let window = size as u64;
        // Drop from the front those out of the window
        while self.len > 0 && self.queue[self.head].0 + window <= self.count {
            self.head = (self.head + 1) % size;
            self.len -= 1;
        }
        // And from the back the larger ones: they can never be the minimum
        while self.len > 0 {
            let back = (self.head + self.len - 1) % size;
            if self.queue[back].1 < gain {
                break;
            }
            self.len -= 1;
        }
        // At most `window - 1` left, so there is room
        self.queue[(self.head + self.len) % size] = (self.count, gain);
        self.len += 1;
        self.count += 1;
        self.queue[self.head].1
    }

    /// Limit `channels` in place to `ceiling`; the lowest gain applied
    fn process(&mut self, target: &MasterTarget, channels: &mut [&mut [f32]]) -> f32 {
        let on = target.limiter;
        if !on && self.wet == 0.0 {
            return 1.0;
        }
        if on && self.wet == 0.0 {
            // Starting: no stale sound or reduction
            self.reset();
        }
        let ceiling = target.ceiling.clamp(1e-6, 1.0);
        let release =
            1.0 - (-1.0 / (self.sample_rate * target.release_ms.max(1.0) / 1000.0).max(1.0)).exp();
        let width = self.average.len() as f64;
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        let used = channels.len().min(2);
        let channels = &mut channels[..used];
        let mut lowest = 1.0f32;
        for i in 0..n {
            let peak = channels.iter().fold(0.0f32, |p, c| p.max(c[i].abs()));
            let needed = if peak > ceiling { ceiling / peak } else { 1.0 };
            let held = self.push_min(needed);
            self.envelope = if held < self.envelope {
                held
            } else {
                self.envelope + (held - self.envelope) * release
            };
            self.sum += (self.envelope - self.average[self.average_pos]) as f64;
            self.average[self.average_pos] = self.envelope;
            self.average_pos = (self.average_pos + 1) % self.average.len();
            let gain = ((self.sum / width) as f32).min(1.0);
            lowest = lowest.min(gain);

            crate::plugins::chain::ramp(&mut self.wet, if on { 1.0 } else { 0.0 }, self.fade_step);
            for (c, samples) in channels.iter_mut().enumerate() {
                let line = &mut self.lines[c];
                let input = samples[i];
                let delayed = std::mem::replace(&mut line[self.pos], input);
                // Belt and braces: rounding in the running sum
                let limited = (delayed * gain).clamp(-ceiling, ceiling);
                samples[i] = self.wet * limited + (1.0 - self.wet) * input;
            }
            self.pos = (self.pos + 1) % self.delay;
        }
        // The running sum drifts by rounding: recomputed once per period
        self.sum = self.average.iter().map(|&v| v as f64).sum();
        lowest
    }
}

/// EQ and limiter of one output or pair, on the audio thread
pub struct MasterProc {
    bands: [Band; BANDS],
    limiter: PeakLimiter,
    smooth: f32,
    /// Processed last period: a strip that comes back starts clean
    active: bool,
}

impl MasterProc {
    /// Allocates: build off the audio thread
    pub fn new(sample_rate: f32) -> Self {
        Self {
            bands: [Band::IDLE; BANDS],
            limiter: PeakLimiter::new(sample_rate),
            smooth: 1.0 - (-1.0 / (sample_rate * SMOOTH_MS / 1000.0).max(1.0)).exp(),
            active: false,
        }
    }

    /// Lookahead of the limiter, samples
    pub fn latency(&self) -> usize {
        self.limiter.delay
    }

    /// Not played this period
    pub fn idle(&mut self) {
        if self.active {
            self.bands = [Band::IDLE; BANDS];
            self.limiter.wet = 0.0;
            self.active = false;
        }
    }

    /// Run on `channels` (1 or 2, the outputs of a strip) in place
    /// (real-time safe). Returns the gain reduction applied, 0 (none) to 1.
    pub fn process(&mut self, target: &MasterTarget, channels: &mut [&mut [f32]]) -> f32 {
        if !self.active {
            self.active = true;
            // Straight to the limiter's state: no fade-in that would let a
            // loud start through
            self.limiter.wet = if target.limiter { 1.0 } else { 0.0 };
            self.limiter.reset();
        }
        for (band, params) in self.bands.iter_mut().zip(&target.eq) {
            band.process(params, self.smooth, channels);
        }
        1.0 - self.limiter.process(target, channels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const RATE: f32 = 48_000.0;

    /// Gain at `freq` of `target` measured with a sine, dB
    fn measured(target: &MasterTarget, freq: f32) -> f32 {
        let mut proc = MasterProc::new(RATE);
        let n = 48_000;
        let mut signal: Vec<f32> = (0..n)
            .map(|i| 0.1 * (2.0 * std::f32::consts::PI * freq * i as f32 / RATE).sin())
            .collect();
        for chunk in signal.chunks_mut(128) {
            proc.process(target, &mut [chunk]);
        }
        // RMS of the last half, after the filters settled
        let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
        20.0 * (rms(&signal[n / 2..]) / (0.1 / 2f32.sqrt())).log10()
    }

    fn eq_with(band: usize, set: impl FnOnce(&mut EqBand)) -> MasterTarget {
        let mut eq = OutputEq {
            on: true,
            ..OutputEq::default()
        };
        set(eq.band_mut(band).unwrap());
        target(
            &eq,
            &Limiter {
                on: false,
                ..Limiter::default()
            },
            RATE,
        )
    }

    /// The RBJ cookbook response, as the Routing view draws it
    fn drawn(kind: BandKind, band: &EqBand, freq: f32) -> f32 {
        band.curve(kind).gain_at(freq)
    }

    #[test]
    fn every_band_has_the_response_drawn() {
        let cases = [
            (0, EqBand::new(true, 100.0)),
            (
                1,
                EqBand {
                    gain_db: 6.0,
                    ..EqBand::new(true, 200.0)
                },
            ),
            (
                2,
                EqBand {
                    gain_db: -9.0,
                    q: 2.0,
                    ..EqBand::new(true, 800.0)
                },
            ),
            (
                3,
                EqBand {
                    gain_db: 4.0,
                    q: 0.7,
                    ..EqBand::new(true, 3000.0)
                },
            ),
            (
                4,
                EqBand {
                    gain_db: -6.0,
                    ..EqBand::new(true, 5000.0)
                },
            ),
            (5, EqBand::new(true, 6000.0)),
        ];
        for (band, values) in cases {
            let target = eq_with(band, |b| *b = values);
            let kind = BAND_KINDS[band].0;
            for freq in [40.0, 100.0, 250.0, 800.0, 2000.0, 5000.0, 12_000.0] {
                let (got, want) = (measured(&target, freq), drawn(kind, &values, freq));
                // Below -40 dB, the sine's own leftovers count
                if want > -40.0 {
                    assert!(
                        (got - want).abs() < 0.15,
                        "band {band} at {freq} Hz: {got:.2} dB, drawn {want:.2} dB"
                    );
                }
            }
        }
    }

    #[test]
    fn cuts_and_bells_land_where_expected() {
        let low_cut = eq_with(0, |b| *b = EqBand::new(true, 100.0));
        assert!((measured(&low_cut, 100.0) + 3.0).abs() < 0.1);
        assert!(measured(&low_cut, 25.0) < -20.0);
        assert!(measured(&low_cut, 2000.0).abs() < 0.05);
        let high_cut = eq_with(5, |b| *b = EqBand::new(true, 5000.0));
        assert!((measured(&high_cut, 5000.0) + 3.0).abs() < 0.1);
        assert!(measured(&high_cut, 100.0).abs() < 0.05);
        let bell = eq_with(2, |b| {
            *b = EqBand {
                gain_db: 6.0,
                ..EqBand::new(true, 1000.0)
            }
        });
        assert!((measured(&bell, 1000.0) - 6.0).abs() < 0.1);
        let low = eq_with(1, |b| {
            *b = EqBand {
                gain_db: -9.0,
                ..EqBand::new(true, 200.0)
            }
        });
        assert!((measured(&low, 30.0) + 9.0).abs() < 0.3);
        assert!(measured(&low, 5000.0).abs() < 0.1);
    }

    #[test]
    fn flat_or_off_eq_and_idle_limiter_pass_the_signal_untouched() {
        let mut proc = MasterProc::new(RATE);
        let input: Vec<f32> = (0..512)
            .map(|i| ((i * 37) % 101) as f32 / 101.0 - 0.5)
            .collect();
        let mut out = input.clone();
        let off = target(
            &OutputEq::default(),
            &Limiter {
                on: false,
                ..Limiter::default()
            },
            RATE,
        );
        proc.process(&off, &mut [&mut out]);
        assert_eq!(out, input);
        // A flat EQ switched on, a limiter that is not reached: only the
        // lookahead delay
        let flat = target(
            &OutputEq {
                on: true,
                ..OutputEq::default()
            },
            &Limiter::default(),
            RATE,
        );
        let mut proc = MasterProc::new(RATE);
        let mut out = input.clone();
        proc.process(&flat, &mut [&mut out]);
        let d = proc.latency();
        assert_eq!(d, 48);
        assert_eq!(&out[d..], &input[..input.len() - d]);
    }

    /// Signals that push a limiter: full-scale square, single-sample spikes,
    /// a step from silence, an alternating ±2 (the fastest swing there is),
    /// a loud sine sweep
    fn hard_signals() -> Vec<Vec<f32>> {
        let n = 24_000;
        let square = (0..n)
            .map(|i| if (i / 50) % 2 == 0 { 1.5 } else { -1.5 })
            .collect();
        let spikes = (0..n)
            .map(|i| if i % 997 == 0 { 8.0 } else { 0.01 })
            .collect();
        let step = (0..n).map(|i| if i < 1000 { 0.0 } else { 4.0 }).collect();
        let alternating = (0..n)
            .map(|i| if i % 2 == 0 { 2.0 } else { -2.0 })
            .collect();
        let sweep = (0..n)
            .map(|i| {
                let t = i as f32 / RATE;
                3.0 * (2.0 * std::f32::consts::PI * (50.0 + 8000.0 * t) * t).sin()
            })
            .collect();
        vec![square, spikes, step, alternating, sweep]
    }

    #[test]
    fn limiter_never_exceeds_the_ceiling() {
        for ceiling_db in [-1.0, -6.0, 0.0] {
            let ceiling = 10f32.powf(ceiling_db / 20.0);
            for release_ms in [10.0, 100.0, 1000.0] {
                let limiter = Limiter {
                    on: true,
                    ceiling_db,
                    release_ms,
                };
                // With the EQ boosting too
                let mut eq = OutputEq {
                    on: true,
                    ..OutputEq::default()
                };
                eq.low_shelf.gain_db = 12.0;
                eq.mid_1.gain_db = 12.0;
                let target = target(&eq, &limiter, RATE);
                for (s, signal) in hard_signals().into_iter().enumerate() {
                    let mut proc = MasterProc::new(RATE);
                    let mut left = signal.clone();
                    let mut right: Vec<f32> = signal.iter().map(|x| -0.5 * x).collect();
                    // Odd period sizes on purpose
                    let mut start = 0;
                    for size in [64, 128, 1, 333, 1024].iter().cycle() {
                        if start >= left.len() {
                            break;
                        }
                        let end = (start + size).min(left.len());
                        let reduction = proc.process(
                            &target,
                            &mut [&mut left[start..end], &mut right[start..end]],
                        );
                        assert!((0.0..=1.0).contains(&reduction));
                        start = end;
                    }
                    for x in left.iter().chain(&right) {
                        assert!(x.is_finite(), "signal {s}: not a number");
                        assert!(
                            x.abs() <= ceiling * (1.0 + 1e-6),
                            "signal {s}, {ceiling_db} dB, {release_ms} ms: {x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn limiter_reaches_the_ceiling_without_pumping_below_it() {
        // A steady 0 dBFS sine with a -6 dB ceiling sits at -6 dB
        let target = target(
            &OutputEq::default(),
            &Limiter {
                on: true,
                ceiling_db: -6.0,
                release_ms: 100.0,
            },
            RATE,
        );
        let mut proc = MasterProc::new(RATE);
        let mut s: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / RATE).sin())
            .collect();
        let mut reduction = 0.0;
        for chunk in s.chunks_mut(256) {
            reduction = proc.process(&target, &mut [chunk]);
        }
        let peak = s[24_000..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let peak_db = 20.0 * peak.log10();
        assert!((-6.5..=-6.0).contains(&peak_db), "peak {peak_db} dB");
        assert!((0.45..0.55).contains(&reduction), "reduction {reduction}");
    }

    #[test]
    fn moving_parameters_stay_finite() {
        let mut proc = MasterProc::new(RATE);
        let mut signal: Vec<f32> = (0..96_000)
            .map(|i| (2.0 * std::f32::consts::PI * 100.0 * i as f32 / RATE).sin())
            .collect();
        for (i, chunk) in signal.chunks_mut(64).enumerate() {
            // Every band swept and switched at every period
            let mut eq = OutputEq {
                on: i % 7 != 0,
                ..OutputEq::default()
            };
            let f = FREQ_RANGE.0 * 1000f32.powf((i % 50) as f32 / 49.0);
            for b in 0..BANDS {
                let band = eq.band_mut(b).unwrap();
                *band = EqBand {
                    on: (i + b) % 3 != 0,
                    freq: f,
                    gain_db: if i % 2 == 0 { 12.0 } else { -12.0 },
                    q: if i % 4 == 0 { Q_RANGE.1 } else { Q_RANGE.0 },
                };
            }
            let limiter = Limiter {
                on: i % 11 != 0,
                ..Limiter::default()
            };
            proc.process(&target(&eq, &limiter, RATE), &mut [chunk]);
        }
        assert!(signal.iter().all(|x| x.is_finite() && x.abs() < 4.0));
        // At the top of the range of rates and frequencies
        let mut proc = MasterProc::new(192_000.0);
        let mut eq = OutputEq {
            on: true,
            ..OutputEq::default()
        };
        eq.high_cut = EqBand::new(true, 20_000.0);
        eq.mid_2 = EqBand {
            gain_db: 12.0,
            q: Q_RANGE.1,
            ..EqBand::new(true, 20_000.0)
        };
        let t = target(&eq, &Limiter::default(), 192_000.0);
        let mut noise: Vec<f32> = (0..4096)
            .map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0)
            .collect();
        proc.process(&t, &mut [&mut noise]);
        assert!(noise.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn old_and_partial_settings_load() {
        let eq: OutputEq =
            serde_json::from_str(r#"{ "on": true, "mid_1": { "gain_db": 3.0 } }"#).unwrap();
        assert!(eq.on);
        assert_eq!(eq.mid_1.gain_db, 3.0);
        assert_eq!(eq.mid_1.freq, EqBand::default().freq);
        assert_eq!(eq.low_cut, OutputEq::default().low_cut);
        let limiter: Limiter = serde_json::from_str("{}").unwrap();
        assert_eq!(limiter, Limiter::default());
        assert!(limiter.on);
    }
}
