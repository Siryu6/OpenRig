//! What to show above the parameters of a delay, reverb, drive, modulation or
//! cabinet block (see view_infer): mostly what the block does to the sound,
//! measured (its level over the last seconds, its spectrum, its waves in and
//! out), and the echoes of a delay whose time is known.

/// Kind of block, and so what is shown of it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// Echoes, and the level in and out over the last seconds
    Delay,
    /// The level in and out over the last seconds: the tail after a note
    Reverb,
    /// The waves in and out (how it clips), and the spectrum in and out (the
    /// harmonics)
    Drive,
    /// The spectrum in and out
    Modulation,
    /// The spectrum in and out: the cabinet's response
    Cabinet,
}

/// How to draw a block: its kind, and its echoes if it is a delay
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    pub kind: ScopeKind,
    pub echoes: Option<Echoes>,
}

/// A delay's controls, by symbol, as far as they are understood
#[derive(Debug, Clone, PartialEq)]
pub struct Echoes {
    /// Time between echoes
    pub time: String,
    /// Seconds per unit of the time control (0.001 for ms)
    pub seconds: f32,
    /// Share of each echo in the next, and the control's range
    pub feedback: Option<Level>,
    /// Level of the echoes against the dry sound
    pub mix: Option<Level>,
    /// The mix fades the dry sound out as the echoes come in (a "dry/wet"
    /// control); otherwise the dry sound stays
    pub crossfade: bool,
    /// Toggle that makes the time follow the tempo instead (its meaning
    /// varies: no echoes are drawn while it is on)
    pub sync: Option<String>,
}

/// A 0 to 1 amount read from a control: its range, or dB
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub symbol: String,
    pub min: f32,
    pub max: f32,
    pub db: bool,
}

impl Level {
    /// Control value for an amount, 0 to 1 (the reverse of `read`)
    pub fn value(&self, amount: f32) -> f32 {
        if self.db {
            20.0 * amount.max(1e-6).log10()
        } else if self.max > self.min {
            self.min + amount.clamp(0.0, 1.0) * (self.max - self.min)
        } else {
            amount.clamp(0.0, 1.0)
        }
    }

    fn read(&self, value: &impl Fn(&str) -> Option<f32>) -> Option<f32> {
        let v = value(&self.symbol)?;
        Some(if self.db {
            10f32.powf(v / 20.0)
        } else if self.max > self.min {
            ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            v.clamp(0.0, 1.0)
        })
    }
}

/// Echoes drawn at most, and the quietest (amplitude, -60 dB)
const MAX_ECHOES: usize = 12;
const QUIETEST: f32 = 0.001;

impl Echoes {
    /// Delay between echoes, seconds (none while synced to the tempo)
    pub fn seconds(&self, value: &impl Fn(&str) -> Option<f32>) -> Option<f32> {
        let time = value(&self.time)? * self.seconds;
        (!self.synced(value) && time > 0.0).then_some(time)
    }

    /// The time comes from the tempo
    pub fn synced(&self, value: &impl Fn(&str) -> Option<f32>) -> bool {
        self.sync
            .as_deref()
            .and_then(value)
            .is_some_and(|v| v >= 0.5)
    }

    /// Feedback, 0 to 1 (none: a single echo)
    pub fn feedback(&self, value: &impl Fn(&str) -> Option<f32>) -> f32 {
        self.feedback
            .as_ref()
            .and_then(|f| f.read(value))
            .unwrap_or(0.0)
            .min(0.99)
    }

    /// The dry sound and each echo: (seconds, amplitude), the dry first
    pub fn taps(&self, value: &impl Fn(&str) -> Option<f32>) -> Option<Vec<(f32, f32)>> {
        let time = self.seconds(value)?;
        let feedback = self.feedback(value);
        let wet = self.mix.as_ref().and_then(|m| m.read(value)).unwrap_or(1.0);
        // A mix in dB can go over unity: the dry sound is then gone
        let dry = if self.crossfade {
            (1.0 - wet).max(0.0)
        } else {
            1.0
        };
        let mut taps = vec![(0.0, dry)];
        let mut level = wet;
        for n in 1..=MAX_ECHOES {
            if level < QUIETEST {
                break;
            }
            taps.push((n as f32 * time, level));
            level *= feedback;
        }
        Some(taps)
    }

    /// Value of the time control for a delay of `seconds`
    pub fn time_value(&self, seconds: f32) -> f32 {
        seconds / self.seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::control_map::values;

    fn delay() -> Echoes {
        let level = |symbol: &str| Level {
            symbol: symbol.into(),
            min: 0.0,
            max: 1.0,
            db: false,
        };
        Echoes {
            time: "time".into(),
            seconds: 0.001,
            feedback: Some(level("feedb")),
            mix: Some(level("drywet")),
            crossfade: true,
            sync: Some("sync".into()),
        }
    }

    #[test]
    fn echoes_fade_by_the_feedback() {
        let v = values(&[
            ("time", 250.0),
            ("feedb", 0.5),
            ("drywet", 0.4),
            ("sync", 0.0),
        ]);
        let taps = delay().taps(&v).unwrap();
        assert_eq!(taps[0], (0.0, 0.6));
        assert_eq!(taps[1], (0.25, 0.4));
        assert_eq!(taps[2], (0.5, 0.2));
        // Down to -60 dB: 0.4 / 2^n
        assert_eq!(taps.len(), 1 + 9);
    }

    #[test]
    fn no_feedback_is_one_echo_and_a_synced_time_none() {
        let v = values(&[("time", 100.0), ("feedb", 0.0), ("drywet", 1.0)]);
        assert_eq!(delay().taps(&v).unwrap(), [(0.0, 0.0), (0.1, 1.0)]);
        let synced = values(&[("time", 100.0), ("sync", 1.0)]);
        assert_eq!(delay().taps(&synced), None);
    }

    #[test]
    fn amounts_go_back_to_control_values() {
        let d = delay();
        assert!((d.time_value(0.25) - 250.0).abs() < 1e-3);
        let feedback = Level {
            symbol: "fb".into(),
            min: 0.0,
            max: 100.0,
            db: false,
        };
        assert!((feedback.value(0.3) - 30.0).abs() < 1e-3);
        let v = |_: &str| Some(30.0);
        assert!((feedback.read(&v).unwrap() - 0.3).abs() < 1e-6);
        let db = Level {
            db: true,
            ..feedback
        };
        assert!((db.value(0.5) + 6.02).abs() < 0.01);
    }
}
