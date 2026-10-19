//! Level curve of a compressor, limiter or gate, computed from its controls
//! (see view_infer, which finds them in the plugin's ports): the output level
//! for each input level, as the textbook curve of its kind, so it is close
//! to, not exactly, what a plugin with its own detector does.

use super::control_map::interpolate;

/// What the plugin does to loud or quiet sounds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Lowers what goes over the threshold, by the ratio
    Compressor,
    /// Keeps the output under the threshold
    Limiter,
    /// Lowers what stays under the threshold
    Gate,
}

/// How to draw a plugin as a dynamics processor: its controls, by symbol
#[derive(Debug, Clone, PartialEq)]
pub struct Dynamics {
    pub kind: Kind,
    pub threshold: String,
    pub ratio: Option<String>,
    /// Ratio control read through its named values ("1:4" at 0.75…): (value,
    /// 1 / ratio), by value. Empty when the control is the ratio itself
    pub ratio_scale: Vec<(f32, f32)>,
    /// Width of the soft knee, dB
    pub knee: Option<String>,
    /// Gain added after the reduction
    pub makeup: Option<String>,
    /// Gain added before (a limiter's drive)
    pub input_gain: Option<String>,
    /// How far a gate lowers the sound, dB (negative)
    pub range: Option<String>,
    pub enable: Option<String>,
    /// Output port reporting the reduction, dB
    pub reduction: Option<String>,
}

/// A gate without a range closes this far, dB
const GATE_CLOSED: f32 = -80.0;

impl Dynamics {
    /// Output level for an input level, dB; `value` gives a control's value
    pub fn output(&self, value: &impl Fn(&str) -> Option<f32>, input: f32) -> f32 {
        let read = |symbol: &Option<String>| symbol.as_deref().and_then(value);
        let enabled = read(&self.enable).is_none_or(|v| v >= 0.5);
        if !enabled {
            return input;
        }
        let threshold = value(&self.threshold).unwrap_or(0.0);
        let x = input + read(&self.input_gain).unwrap_or(0.0);
        let y = match self.kind {
            Kind::Compressor => {
                let ratio = self.ratio(value).unwrap_or(4.0).max(1.0);
                let knee = read(&self.knee).unwrap_or(0.0).max(0.0);
                compress(x, threshold, ratio, knee)
            }
            Kind::Limiter => compress(x, threshold, f32::INFINITY, 0.0),
            Kind::Gate => {
                let range = read(&self.range).unwrap_or(GATE_CLOSED).min(0.0);
                if x < threshold {
                    x + range
                } else {
                    x
                }
            }
        };
        y + read(&self.makeup).unwrap_or(0.0)
    }

    /// The compression ratio (infinite for a limiter's setting)
    pub fn ratio(&self, value: &impl Fn(&str) -> Option<f32>) -> Option<f32> {
        let raw = self.ratio.as_deref().and_then(value)?;
        if self.ratio_scale.len() < 2 {
            return Some(raw);
        }
        let slope = interpolate(&self.ratio_scale, raw).clamp(0.0, 1.0);
        Some(if slope > 0.0 {
            1.0 / slope
        } else {
            f32::INFINITY
        })
    }

    /// Value of the ratio control that gives `ratio`
    pub fn ratio_value(&self, ratio: f32) -> f32 {
        if self.ratio_scale.len() < 2 {
            return ratio;
        }
        let mut by_slope: Vec<(f32, f32)> = self.ratio_scale.iter().map(|&(v, s)| (s, v)).collect();
        by_slope.sort_by(|a, b| a.0.total_cmp(&b.0));
        interpolate(&by_slope, 1.0 / ratio.max(1.0))
    }

    /// Ratio that puts the output of a 0 dB input at `output` dB (dragging
    /// the end of a compressor's curve), the gains added before and after
    /// kept
    pub fn ratio_for(&self, value: &impl Fn(&str) -> Option<f32>, output: f32) -> Option<f32> {
        let read = |symbol: &Option<String>| symbol.as_deref().and_then(value);
        let threshold = value(&self.threshold)?;
        let over = read(&self.input_gain).unwrap_or(0.0) - threshold;
        let above = output - read(&self.makeup).unwrap_or(0.0) - threshold;
        (self.kind == Kind::Compressor && over > 0.0).then(|| {
            if above <= 0.0 {
                f32::INFINITY
            } else {
                over / above
            }
        })
    }
}

/// Hard or soft knee compression of `x` dB (Giannoulis, Massberg & Reiss)
fn compress(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    let slope = 1.0 / ratio - 1.0;
    if 2.0 * over < -knee {
        x
    } else if knee > 0.0 && 2.0 * over.abs() <= knee {
        x + slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        x + slope * over
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::control_map::values;

    fn plugin(kind: Kind) -> Dynamics {
        Dynamics {
            kind,
            threshold: "thr".into(),
            ratio: Some("rat".into()),
            ratio_scale: Vec::new(),
            knee: Some("kn".into()),
            makeup: Some("mak".into()),
            input_gain: None,
            range: None,
            enable: None,
            reduction: None,
        }
    }

    #[test]
    fn compressor_follows_its_ratio_over_the_threshold() {
        let c = plugin(Kind::Compressor);
        let v = values(&[("thr", -20.0), ("rat", 4.0), ("kn", 0.0), ("mak", 0.0)]);
        assert_eq!(c.output(&v, -40.0), -40.0);
        assert_eq!(c.output(&v, 0.0), -15.0);
    }

    #[test]
    fn soft_knee_meets_both_lines() {
        let c = plugin(Kind::Compressor);
        let v = values(&[("thr", -20.0), ("rat", 4.0), ("kn", 8.0), ("mak", 3.0)]);
        assert!((c.output(&v, -24.0) - -21.0).abs() < 1e-4);
        assert!((c.output(&v, -16.0) - (-20.0 + 4.0 / 4.0 + 3.0)).abs() < 1e-4);
        // Inside the knee, below both lines' corner
        assert!(c.output(&v, -20.0) < -17.0);
    }

    #[test]
    fn limiter_never_goes_over() {
        let mut l = plugin(Kind::Limiter);
        l.input_gain = Some("gain".into());
        let v = values(&[("thr", -1.0), ("gain", 10.0)]);
        assert_eq!(l.output(&v, -30.0), -20.0);
        assert_eq!(l.output(&v, -3.0), -1.0);
    }

    #[test]
    fn gate_lowers_what_stays_under() {
        let mut g = plugin(Kind::Gate);
        g.range = Some("close".into());
        let v = values(&[("thr", -40.0), ("close", -50.0)]);
        assert_eq!(g.output(&v, -60.0), -110.0);
        assert_eq!(g.output(&v, -20.0), -20.0);
    }

    #[test]
    fn a_ratio_given_as_named_values_is_read_through_them() {
        // x42-darc: 0 to 1, "1:4" at 0.75, "Lim" at 1
        let mut c = plugin(Kind::Compressor);
        c.ratio_scale = vec![(0.5, 0.5), (0.75, 0.25), (1.0, 0.0)];
        let ratio = |v: f32| c.ratio(&move |s: &str| (s == "rat").then_some(v)).unwrap();
        assert!((ratio(0.75) - 4.0).abs() < 1e-4);
        assert!((ratio(0.0) - 1.0).abs() < 1e-4);
        assert_eq!(ratio(1.0), f32::INFINITY);
        assert!((c.ratio_value(4.0) - 0.75).abs() < 1e-4);
        assert!((c.ratio_value(10.0) - 0.9).abs() < 1e-4);
    }

    #[test]
    fn dragging_the_end_sets_the_ratio() {
        let c = plugin(Kind::Compressor);
        let v = values(&[("thr", -20.0), ("mak", 0.0)]);
        assert_eq!(c.ratio_for(&v, -15.0), Some(4.0));
        assert_eq!(c.ratio_for(&v, -25.0), Some(f32::INFINITY));
        // 6 dB in: 0 dB reaches the compressor at +6, 26 dB over
        let driven = Dynamics {
            input_gain: Some("in".into()),
            ..c
        };
        let v = values(&[("thr", -20.0), ("mak", 0.0), ("in", 6.0)]);
        assert_eq!(driven.ratio_for(&v, -13.5), Some(4.0));
    }
}
