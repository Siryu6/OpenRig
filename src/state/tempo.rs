//! Global tempo and tap tempo. The tempo belongs to the preset; blocks with a
//! BPM parameter (LV2 `time:beatsPerMinute`) follow it.

use std::time::{Duration, Instant};

pub const MIN_BPM: f32 = 40.0;
pub const MAX_BPM: f32 = 240.0;
/// Taps further apart than this start a new count
const TAP_TIMEOUT: Duration = Duration::from_secs(2);
/// Intervals averaged (the last few taps: the tempo follows the player)
const TAP_HISTORY: usize = 4;

pub fn clamp_bpm(bpm: f32) -> f32 {
    if bpm.is_finite() {
        bpm.clamp(MIN_BPM, MAX_BPM)
    } else {
        crate::presets::DEFAULT_BPM
    }
}

/// Turns taps on a key or footswitch into a tempo
#[derive(Debug, Default)]
pub struct TapTempo {
    last: Option<Instant>,
    intervals: Vec<Duration>,
}

impl TapTempo {
    /// Record a tap at `now`; the new tempo once two taps are in
    pub fn tap(&mut self, now: Instant) -> Option<f32> {
        let last = self.last.replace(now);
        let interval = now.saturating_duration_since(last?);
        if interval > TAP_TIMEOUT || interval.is_zero() {
            self.intervals.clear();
            return None;
        }
        if self.intervals.len() == TAP_HISTORY {
            self.intervals.remove(0);
        }
        self.intervals.push(interval);
        let average = self.intervals.iter().sum::<Duration>() / self.intervals.len() as u32;
        Some(clamp_bpm(60.0 / average.as_secs_f32()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_average_and_restart_after_a_pause() {
        let mut tap = TapTempo::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(tap.tap(at(0)), None);
        assert_eq!(tap.tap(at(500)), Some(120.0));
        // 500 then 600 ms: 550 on average
        let bpm = tap.tap(at(1100)).unwrap();
        assert!((bpm - 60.0 / 0.55).abs() < 0.01);
        // A pause starts over
        assert_eq!(tap.tap(at(4000)), None);
        assert_eq!(tap.tap(at(4250)), Some(240.0));
    }

    #[test]
    fn tempo_stays_in_range() {
        assert_eq!(clamp_bpm(10.0), MIN_BPM);
        assert_eq!(clamp_bpm(f32::NAN), 120.0);
        assert_eq!(clamp_bpm(98.5), 98.5);
    }
}
