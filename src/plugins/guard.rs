//! Plugin guard: switches off a plugin that takes too much of the audio
//! period, or keeps putting out non-finite samples, so one bad plugin
//! cannot freeze the board.
//!
//! The audio thread runs at a real-time priority: a plugin that suddenly
//! costs more than the period (a bug triggered by a setting) makes it miss
//! every deadline and hog a core. Each plugin of the chain has a
//! [`Watchdog`], fed with the time the plugin took, at no cost beyond the
//! clock the chain already reads. When it trips, the chain bypasses the
//! plugin at once (it stops running) and tells the UI through a
//! [`GuardBoard`] slot (atomics only on the audio side). The UI then shows
//! the block as bypassed with a toast; switching it back on re-arms the
//! watchdog.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::sync::MutexExt;

/// A period in which the plugin takes at least this share of it is heavy
pub const HEAVY_SHARE: f32 = 0.5;
/// Heavy periods (in seconds of audio, net of the light ones) that switch
/// the plugin off: a sustained overload is cut in about this time
pub const HEAVY_SECONDS: f32 = 0.3;
/// A period in which the plugin alone takes at least this share of it is a
/// spike: with the rest of the chain, the deadline is missed
pub const SPIKE_SHARE: f32 = 0.75;
/// Spikes that switch the plugin off...
pub const SPIKE_LIMIT: f32 = 3.0;
/// ...each one being forgiven after this many seconds: a spike now and
/// then (a preset load, a burst of system activity) is harmless, several
/// in a row are not
pub const SPIKE_FORGIVE_SECONDS: f32 = 0.25;
/// Periods with a non-finite output that switch the plugin off, each one
/// forgiven after `SPIKE_FORGIVE_SECONDS` too
pub const NON_FINITE_LIMIT: f32 = 3.0;
/// Time not checked for load after the plugin starts (or is switched back
/// on): first periods often cost more (buffers, caches, initial state)
pub const GRACE_SECONDS: f32 = 0.2;

/// Why the guard switched a plugin off
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Trip {
    /// It took too much of the audio period
    Cpu = 1,
    /// Its output was not finite (NaN, infinity)
    NonFinite = 2,
}

impl Trip {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Trip::Cpu),
            2 => Some(Trip::NonFinite),
            _ => None,
        }
    }

    /// End of the sentence "<Plugin> was switched off: …"
    pub fn reason(self) -> &'static str {
        match self {
            Trip::Cpu => "it used too much CPU",
            Trip::NonFinite => "its output was invalid",
        }
    }
}

/// Load and output checks of one plugin, fed by the audio thread after
/// each run. Real-time safe: arithmetic only.
#[derive(Debug, Clone)]
pub struct Watchdog {
    sample_rate: f32,
    /// Frames checked at once: one audio period
    window: usize,
    /// Frames, nanoseconds and output state gathered for the window
    frames: usize,
    nanos: u64,
    non_finite: bool,
    /// Seconds of audio left before the load is checked
    grace: f32,
    /// Net seconds of heavy periods
    heavy: f32,
    /// Spikes and non-finite periods not forgiven yet
    spikes: f32,
    bad_output: f32,
    tripped: bool,
}

impl Watchdog {
    /// Watchdog of a plugin run at `sample_rate`, checked every `window`
    /// frames (the audio period)
    pub fn new(sample_rate: f32, window: usize) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            window: window.max(1),
            frames: 0,
            nanos: 0,
            non_finite: false,
            grace: GRACE_SECONDS,
            heavy: 0.0,
            spikes: 0.0,
            bad_output: 0.0,
            tripped: false,
        }
    }

    /// Start over (the plugin is switched back on)
    pub fn rearm(&mut self) {
        *self = Self::new(self.sample_rate, self.window);
    }

    /// True once tripped (until `rearm`)
    pub fn tripped(&self) -> bool {
        self.tripped
    }

    /// The plugin processed `frames` frames in `nanos` nanoseconds, with
    /// a finite output or not; returns why to switch it off, once
    pub fn record(&mut self, frames: usize, nanos: u64, finite: bool) -> Option<Trip> {
        if self.tripped {
            return None;
        }
        self.frames += frames;
        self.nanos = self.nanos.saturating_add(nanos);
        self.non_finite |= !finite;
        if self.frames < self.window {
            return None;
        }
        let period = self.frames as f32 / self.sample_rate;
        let load = self.nanos as f32 * 1e-9 / period;
        let non_finite = self.non_finite;
        (self.frames, self.nanos, self.non_finite) = (0, 0, false);

        // Spikes and bad outputs are forgiven over the periods without
        let forgiven = period / SPIKE_FORGIVE_SECONDS;
        if non_finite {
            self.bad_output += 1.0;
            if self.bad_output >= NON_FINITE_LIMIT {
                self.tripped = true;
                return Some(Trip::NonFinite);
            }
        } else {
            self.bad_output = (self.bad_output - forgiven).max(0.0);
        }

        if self.grace > 0.0 {
            self.grace -= period;
            return None;
        }
        self.heavy = if load >= HEAVY_SHARE {
            self.heavy + period
        } else {
            (self.heavy - period).max(0.0)
        };
        if load >= SPIKE_SHARE {
            self.spikes += 1.0;
        } else {
            self.spikes = (self.spikes - forgiven).max(0.0);
        }
        if self.heavy >= HEAVY_SECONDS || self.spikes >= SPIKE_LIMIT {
            self.tripped = true;
            return Some(Trip::Cpu);
        }
        None
    }
}

/// Plugins the guard switched off, for the UI. The audio thread only
/// stores to atomics; the UI takes the trips with `take`.
#[derive(Debug, Default)]
pub struct GuardBoard {
    slots: Mutex<HashMap<usize, Arc<AtomicU8>>>,
    /// A slot was set since the last `take`
    pending: AtomicBool,
}

impl GuardBoard {
    /// The board of the running chain
    pub fn global() -> &'static GuardBoard {
        static BOARD: OnceLock<GuardBoard> = OnceLock::new();
        BOARD.get_or_init(GuardBoard::default)
    }

    /// Slot of the block `instance_id` (not real-time safe: called when
    /// building the chain)
    pub fn slot(&self, instance_id: usize) -> Arc<AtomicU8> {
        self.slots.locked().entry(instance_id).or_default().clone()
    }

    /// Report a trip in `slot` (real-time safe)
    pub fn report(&self, slot: &AtomicU8, trip: Trip) {
        slot.store(trip as u8, Ordering::Release);
        self.pending.store(true, Ordering::Release);
    }

    /// Trips since the last call, by instance_id; locks only when there
    /// are some
    pub fn take(&self) -> Vec<(usize, Trip)> {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }
        let mut trips: Vec<(usize, Trip)> = self
            .slots
            .locked()
            .iter()
            .filter_map(|(&id, slot)| Some((id, Trip::from_u8(slot.swap(0, Ordering::AcqRel))?)))
            .collect();
        trips.sort_by_key(|&(id, _)| id);
        trips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;
    const PERIOD: usize = 128;

    /// Nanoseconds for `share` of a period
    fn nanos(share: f32) -> u64 {
        (share * PERIOD as f32 / RATE * 1e9) as u64
    }

    /// Feed `periods` periods at `share` of the period; the period of the
    /// first trip, if any
    fn feed(dog: &mut Watchdog, periods: usize, share: f32) -> Option<(usize, Trip)> {
        (0..periods).find_map(|p| Some((p, dog.record(PERIOD, nanos(share), true)?)))
    }

    /// Periods in `seconds`
    fn periods(seconds: f32) -> usize {
        (seconds * RATE / PERIOD as f32).ceil() as usize
    }

    #[test]
    fn a_light_plugin_is_never_cut() {
        let mut dog = Watchdog::new(RATE, PERIOD);
        assert_eq!(feed(&mut dog, periods(60.0), 0.3), None);
        assert!(!dog.tripped());
    }

    #[test]
    fn a_sustained_overload_is_cut_within_half_a_second() {
        let mut dog = Watchdog::new(RATE, PERIOD);
        let (at, trip) = feed(&mut dog, periods(5.0), 0.6).expect("cut");
        assert_eq!(trip, Trip::Cpu);
        let seconds = at as f32 * PERIOD as f32 / RATE;
        assert!((0.4..=0.55).contains(&seconds), "cut after {seconds} s");
        // Reported once
        assert_eq!(feed(&mut dog, 100, 2.0), None);
        assert!(dog.tripped());
    }

    #[test]
    fn a_plugin_overloaded_from_its_start_is_cut_too() {
        // Missing every deadline: cut on the spikes, soon after the grace
        let mut dog = Watchdog::new(RATE, PERIOD);
        let (at, trip) = feed(&mut dog, periods(5.0), 3.0).expect("cut");
        assert_eq!(trip, Trip::Cpu);
        assert!(at <= periods(GRACE_SECONDS) + 3, "cut after {at} periods");
    }

    #[test]
    fn single_spikes_and_a_slow_start_are_tolerated() {
        let mut dog = Watchdog::new(RATE, PERIOD);
        // A preset load: the first periods take far more than the period
        assert_eq!(feed(&mut dog, periods(0.15), 20.0), None);
        // Then a spike every half second, and short heavy bursts
        for _ in 0..40 {
            assert_eq!(feed(&mut dog, 1, 4.0), None);
            assert_eq!(feed(&mut dog, periods(0.1), 0.6), None);
            assert_eq!(feed(&mut dog, periods(0.4), 0.1), None);
        }
    }

    #[test]
    fn frequent_spikes_are_cut_even_when_the_average_is_low() {
        // The GlitchShifter case on a Pi 5: about 20 % on average, with a
        // block of three periods about every 100 ms
        let mut dog = Watchdog::new(RATE, PERIOD);
        assert_eq!(feed(&mut dog, periods(GRACE_SECONDS), 0.1), None);
        let mut elapsed = 0;
        let cut = (0..100).find_map(|_| {
            let spike = dog.record(PERIOD, nanos(3.0), true);
            let rest = feed(&mut dog, periods(0.1) - 1, 0.15).map(|(_, t)| t);
            elapsed += periods(0.1);
            spike.or(rest)
        });
        assert_eq!(cut, Some(Trip::Cpu));
        let seconds = elapsed as f32 * PERIOD as f32 / RATE;
        assert!(seconds <= 0.6, "cut after {seconds} s");
    }

    #[test]
    fn parts_of_a_period_add_up() {
        // The chain may run a period in sub-blocks (several cores)
        let mut dog = Watchdog::new(RATE, PERIOD);
        let cut = (0..periods(2.0))
            .find_map(|_| (0..8).find_map(|_| dog.record(PERIOD / 8, nanos(0.6) / 8, true)));
        assert_eq!(cut, Some(Trip::Cpu));
    }

    #[test]
    fn rearming_starts_over() {
        let mut dog = Watchdog::new(RATE, PERIOD);
        assert!(feed(&mut dog, periods(1.0), 0.9).is_some());
        dog.rearm();
        assert!(!dog.tripped());
        // In grace again
        assert_eq!(feed(&mut dog, periods(0.15), 0.9), None);
    }

    #[test]
    fn repeated_non_finite_output_is_cut() {
        let mut dog = Watchdog::new(RATE, PERIOD);
        // Now and then: tolerated
        for _ in 0..20 {
            assert_eq!(dog.record(PERIOD, nanos(0.1), false), None);
            assert_eq!(feed(&mut dog, periods(0.3), 0.1), None);
        }
        // Every period, even in the grace time: cut on the third
        let mut dog = Watchdog::new(RATE, PERIOD);
        assert_eq!(dog.record(PERIOD, 0, false), None);
        assert_eq!(dog.record(PERIOD, 0, false), None);
        assert_eq!(dog.record(PERIOD, 0, false), Some(Trip::NonFinite));
    }

    #[test]
    fn the_board_hands_each_trip_once() {
        let board = GuardBoard::default();
        let (a, b) = (board.slot(7), board.slot(9));
        assert!(board.take().is_empty());
        board.report(&b, Trip::NonFinite);
        board.report(&a, Trip::Cpu);
        assert_eq!(board.take(), vec![(7, Trip::Cpu), (9, Trip::NonFinite)]);
        assert!(board.take().is_empty());
        // The same block keeps its slot across chain builds
        assert!(Arc::ptr_eq(&a, &board.slot(7)));
    }
}
