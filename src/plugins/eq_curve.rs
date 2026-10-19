//! EQ curve of a plugin, computed from its controls: which control is the
//! frequency, the gain, the Q of each band (see view_infer, which reads that
//! from the plugin's ports). Each band is drawn as the textbook biquad of its
//! shape (RBJ cookbook), so the curve is close to, not exactly, what a plugin
//! with other filters does.

use std::f64::consts::{LN_2, PI};

use super::control_map::interpolate;

/// Sample rate the curve is drawn at
const RATE: f64 = 48_000.0;

/// How to draw a plugin as an EQ
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// The plugin's own on/off control, if it has one
    pub enable: Option<String>,
    /// Output gain, added to the whole curve
    pub gain: Option<String>,
    pub bands: Vec<Band>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Highpass,
    Lowpass,
    Lowshelf,
    Highshelf,
    Peak,
    Notch,
    Bandpass,
}

/// How a band's `q` control is read
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QUnit {
    #[default]
    Q,
    /// Bandwidth in octaves
    Octaves,
    /// Shelf slope (1 is the steepest without overshoot)
    Slope,
}

/// A value read from a control, or fixed by the plugin (the frequency of a
/// graphic EQ band)
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Fixed(f32),
    Control(String),
}

impl Value {
    fn read(&self, value: &impl Fn(&str) -> Option<f32>) -> Option<f32> {
        match self {
            Value::Fixed(v) => Some(*v),
            Value::Control(symbol) => value(symbol),
        }
    }

    /// The control, when it is one
    pub fn control(&self) -> Option<&str> {
        match self {
            Value::Fixed(_) => None,
            Value::Control(symbol) => Some(symbol),
        }
    }
}

/// A control choosing the shape of a band (its "Filter type" list)
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeChoice {
    pub control: String,
    /// Value of each choice and the shape it gives (None: the band is off)
    pub options: Vec<(f32, Option<Shape>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub shape: Shape,
    /// Control choosing the shape, when the plugin has one per band
    pub shape_choice: Option<ShapeChoice>,
    /// Control that turns the band on
    pub switch: Option<String>,
    pub freq: Value,
    /// Hz per unit of the frequency control (1000 for one in kHz)
    pub freq_scale: f32,
    pub gain: Option<String>,
    pub q: Option<Value>,
    pub q_unit: QUnit,
    /// (value, Q) points the `q` control is mapped through
    pub q_map: Vec<(f32, f32)>,
}

/// A band with its current values
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandState {
    pub shape: Shape,
    pub on: bool,
    pub freq: f32,
    /// dB
    pub gain: f32,
    /// In the band's `q_unit`, after `q_map`
    pub q: f32,
    pub q_unit: QUnit,
}

impl Band {
    /// Its values, `value` giving the value of a control by symbol
    pub fn state(&self, value: &impl Fn(&str) -> Option<f32>) -> BandState {
        let read = |symbol: &Option<String>| symbol.as_deref().and_then(value);
        let q = self.q.as_ref().and_then(|q| q.read(value));
        let q = q.unwrap_or(match self.q_unit {
            QUnit::Octaves => 1.0,
            QUnit::Q | QUnit::Slope => std::f32::consts::FRAC_1_SQRT_2,
        });
        // A shape list picks the nearest choice; "off" turns the band off
        let chosen = self.shape_choice.as_ref().map(|c| {
            let v = value(&c.control).unwrap_or(0.0);
            c.options
                .iter()
                .min_by(|a, b| (a.0 - v).abs().total_cmp(&(b.0 - v).abs()))
                .and_then(|o| o.1)
        });
        BandState {
            shape: chosen.flatten().unwrap_or(self.shape),
            on: read(&self.switch).is_none_or(|v| v >= 0.5) && chosen.is_none_or(|s| s.is_some()),
            freq: match &self.freq {
                Value::Fixed(hz) => *hz,
                Value::Control(_) => self
                    .freq
                    .read(value)
                    .map_or(1000.0, |f| f * self.freq_scale),
            },
            gain: read(&self.gain).unwrap_or(0.0),
            q: interpolate(&self.q_map, q),
            q_unit: self.q_unit,
        }
    }
}

impl BandState {
    /// Biquad coefficients b0, b1, b2, a0, a1, a2
    fn coefficients(&self) -> [f64; 6] {
        let w0 = 2.0 * PI * (self.freq as f64).clamp(1.0, RATE * 0.49) / RATE;
        let (sin, cos) = w0.sin_cos();
        let a = 10f64.powf(self.gain as f64 / 40.0);
        let q = (self.q as f64).max(1e-3);
        let alpha = match self.q_unit {
            QUnit::Q => sin / (2.0 * q),
            QUnit::Octaves => sin * (LN_2 / 2.0 * q * w0 / sin).sinh(),
            QUnit::Slope => sin / 2.0 * ((a + 1.0 / a) * (1.0 / q - 1.0) + 2.0).max(1e-4).sqrt(),
        };
        let sq = 2.0 * a.sqrt() * alpha;
        match self.shape {
            Shape::Lowpass => [
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ],
            Shape::Highpass => [
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ],
            Shape::Peak => [
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ],
            Shape::Notch => [1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            Shape::Bandpass => [alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            Shape::Lowshelf => [
                a * ((a + 1.0) - (a - 1.0) * cos + sq),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sq),
                (a + 1.0) + (a - 1.0) * cos + sq,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sq,
            ],
            Shape::Highshelf => [
                a * ((a + 1.0) + (a - 1.0) * cos + sq),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sq),
                (a + 1.0) - (a - 1.0) * cos + sq,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sq,
            ],
        }
    }

    /// Gain of the band at `freq` Hz, in dB
    pub fn gain_at(&self, freq: f32) -> f32 {
        if !self.on {
            return 0.0;
        }
        let [b0, b1, b2, a0, a1, a2] = self.coefficients();
        let w = 2.0 * PI * freq as f64 / RATE;
        // |b0 + b1 z⁻¹ + b2 z⁻²|² at z = e^jw
        let power = |c0: f64, c1: f64, c2: f64| {
            let re = c0 + c1 * w.cos() + c2 * (2.0 * w).cos();
            let im = c1 * w.sin() + c2 * (2.0 * w).sin();
            re * re + im * im
        };
        (10.0 * (power(b0, b1, b2) / power(a0, a1, a2)).max(1e-12).log10()) as f32
    }
}

impl Display {
    /// Gain of the whole plugin at each of `freqs`, in dB
    pub fn response(&self, value: &impl Fn(&str) -> Option<f32>, freqs: &[f32]) -> Vec<f32> {
        let enabled = self
            .enable
            .as_deref()
            .and_then(value)
            .is_none_or(|v| v >= 0.5);
        if !enabled {
            return vec![0.0; freqs.len()];
        }
        let gain = self.gain.as_deref().and_then(value).unwrap_or(0.0);
        let bands: Vec<BandState> = self.bands.iter().map(|b| b.state(value)).collect();
        freqs
            .iter()
            .map(|&f| gain + bands.iter().map(|b| b.gain_at(f)).sum::<f32>())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(shape: Shape, freq: f32, gain: f32, q: f32, q_unit: QUnit) -> BandState {
        BandState {
            shape,
            on: true,
            freq,
            gain,
            q,
            q_unit,
        }
    }

    #[test]
    fn peak_has_its_gain_at_its_frequency() {
        let b = band(Shape::Peak, 1000.0, 6.0, 1.0, QUnit::Octaves);
        assert!((b.gain_at(1000.0) - 6.0).abs() < 0.01);
        assert!(b.gain_at(50.0).abs() < 0.1);
    }

    #[test]
    fn shelves_reach_their_gain_away_from_the_corner() {
        let low = band(Shape::Lowshelf, 200.0, -9.0, 1.0, QUnit::Slope);
        assert!((low.gain_at(20.0) + 9.0).abs() < 0.2);
        assert!(low.gain_at(10_000.0).abs() < 0.1);
        let high = band(Shape::Highshelf, 4000.0, 6.0, 1.0, QUnit::Slope);
        assert!((high.gain_at(20_000.0) - 6.0).abs() < 0.3);
    }

    #[test]
    fn butterworth_cut_is_3_db_down_at_its_frequency() {
        let hp = band(Shape::Highpass, 100.0, 0.0, 0.707, QUnit::Q);
        assert!((hp.gain_at(100.0) + 3.0).abs() < 0.05);
        assert!(hp.gain_at(25.0) < -20.0);
    }

    #[test]
    fn notch_removes_its_frequency() {
        let notch = band(Shape::Notch, 1000.0, 0.0, 2.0, QUnit::Q);
        assert!(notch.gain_at(1000.0) < -40.0);
        assert!(notch.gain_at(100.0).abs() < 0.1);
    }

    #[test]
    fn shape_choice_switches_the_band() {
        let b = Band {
            shape: Shape::Peak,
            shape_choice: Some(ShapeChoice {
                control: "type".into(),
                options: vec![
                    (0.0, None),
                    (1.0, Some(Shape::Peak)),
                    (2.0, Some(Shape::Highpass)),
                ],
            }),
            switch: None,
            freq: Value::Fixed(100.0),
            freq_scale: 1.0,
            gain: None,
            q: None,
            q_unit: QUnit::Q,
            q_map: Vec::new(),
        };
        let at = |t: f32| b.state(&move |s: &str| (s == "type").then_some(t));
        assert!(!at(0.0).on);
        assert_eq!(at(2.0).shape, Shape::Highpass);
    }
}
