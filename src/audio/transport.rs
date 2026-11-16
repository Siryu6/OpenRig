//! The pedalboard's musical clock: where the music is, in bars and beats,
//! for the plugins that follow the transport (`time:Position`).
//!
//! The pedalboard always plays: the clock runs from the start of the audio
//! at the preset tempo, in 4/4. A tempo change keeps the position (only the
//! speed of the beats changes); a tap of the tap tempo lands on a beat, so
//! the beats move to the foot; a MIDI Start goes back to the first beat of
//! a bar. The audio thread owns the clock (`ChainSwitcher`), so every block
//! of every chain, a chain fading in included, reads the same position.

/// Beats in a bar, and the note value of a beat (4/4)
pub const BEATS_PER_BAR: u32 = 4;
pub const BEAT_UNIT: u32 = 4;

/// One point of the music, as sent to the plugins
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// Frames since the clock started
    pub frame: u64,
    /// Beats since the clock started
    pub beat: f64,
    pub bpm: f32,
    pub sample_rate: f32,
}

impl Position {
    /// Bar, from 0
    pub fn bar(&self) -> i64 {
        (self.beat / f64::from(BEATS_PER_BAR)).floor() as i64
    }

    /// Beats since the start of the bar, 0 to 4
    pub fn bar_beat(&self) -> f32 {
        self.beat.rem_euclid(f64::from(BEATS_PER_BAR)) as f32
    }

    /// Playback speed: always playing
    pub fn speed(&self) -> f32 {
        1.0
    }

    /// The position `frames` later
    pub fn after(&self, frames: usize) -> Self {
        Self {
            frame: self.frame.wrapping_add(frames as u64),
            beat: self.beat + beats(frames, self.bpm, self.sample_rate),
            ..*self
        }
    }
}

/// Beats in `frames` at `bpm`
fn beats(frames: usize, bpm: f32, sample_rate: f32) -> f64 {
    if sample_rate > 0.0 {
        frames as f64 * f64::from(bpm) / 60.0 / f64::from(sample_rate)
    } else {
        0.0
    }
}

/// What moves the clock besides time
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportEvent {
    /// A tap of the tap tempo: a beat is now
    Beat,
    /// MIDI Start: the first beat of a bar is now
    Start,
}

/// The running clock (audio thread; no allocation)
#[derive(Debug, Clone)]
pub struct Transport {
    now: Position,
}

impl Transport {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            now: Position {
                frame: 0,
                beat: 0.0,
                bpm: 120.0,
                sample_rate,
            },
        }
    }

    /// Where the music is now, at the start of the period
    pub fn position(&self) -> Position {
        self.now
    }

    /// The period of `frames` was played
    pub fn advance(&mut self, frames: usize) {
        self.now = self.now.after(frames);
    }

    /// Follow `bpm` from now on; the position stays where it is (0 or less:
    /// no tempo, kept)
    pub fn set_bpm(&mut self, bpm: f32) {
        if bpm.is_finite() && bpm > 0.0 {
            self.now.bpm = bpm;
        }
    }

    pub fn event(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::Beat => self.now.beat = self.now.beat.round(),
            TransportEvent::Start => self.now.beat = 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_counts_beats_and_bars() {
        let mut clock = Transport::new(48_000.0);
        clock.set_bpm(120.0);
        // Half a second is one beat at 120
        clock.advance(24_000);
        let p = clock.position();
        assert_eq!(p.frame, 24_000);
        assert!((p.beat - 1.0).abs() < 1e-9);
        assert_eq!(p.bar(), 0);
        assert!((p.bar_beat() - 1.0).abs() < 1e-6);
        // Four more beats: one bar later, same place in the bar
        clock.advance(4 * 24_000);
        let p = clock.position();
        assert_eq!(p.bar(), 1);
        assert!((p.bar_beat() - 1.0).abs() < 1e-6);
        assert!((p.after(12_000).bar_beat() - 1.5).abs() < 1e-6);
    }

    #[test]
    fn a_tempo_change_keeps_the_position() {
        let mut clock = Transport::new(48_000.0);
        clock.set_bpm(120.0);
        clock.advance(24_000);
        clock.set_bpm(60.0);
        assert!((clock.position().beat - 1.0).abs() < 1e-9);
        // One beat at 60 is a second
        clock.advance(48_000);
        assert!((clock.position().beat - 2.0).abs() < 1e-9);
        // No tempo, or a broken one, is ignored
        clock.set_bpm(0.0);
        clock.set_bpm(f32::NAN);
        assert_eq!(clock.position().bpm, 60.0);
    }

    #[test]
    fn a_tap_lands_on_a_beat_and_start_on_a_bar() {
        let mut clock = Transport::new(48_000.0);
        clock.set_bpm(120.0);
        // 5.3 beats: the tap pulls back to beat 5, bar 1
        clock.advance(24_000 * 53 / 10);
        clock.event(TransportEvent::Beat);
        let p = clock.position();
        assert!((p.beat - 5.0).abs() < 1e-9);
        assert_eq!((p.bar(), p.bar_beat()), (1, 1.0));
        // 5.7 beats: on to beat 6
        clock.advance(24_000 * 7 / 10);
        clock.event(TransportEvent::Beat);
        assert!((clock.position().beat - 6.0).abs() < 1e-9);
        clock.event(TransportEvent::Start);
        let p = clock.position();
        assert_eq!((p.bar(), p.bar_beat()), (0, 0.0));
        // Frames keep counting
        assert_eq!(p.frame, 24_000 * 6);
    }
}
