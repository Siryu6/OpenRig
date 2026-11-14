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

/// A leaky count of bad periods. Each bad period adds its weight in
/// seconds, each good one takes its own length off, and the plugin is
/// switched off once the count reaches `limit` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// Weight of a bad period; None: its own length
    weight: Option<f32>,
    pub limit: f32,
}

impl Rule {
    /// Bad periods adding up to `seconds` of audio, net of the good ones
    pub const fn sustained(seconds: f32) -> Self {
        Self {
            weight: None,
            limit: seconds,
        }
    }

    /// `count` bad periods, each one forgiven after `forgive` seconds
    pub const fn events(count: f32, forgive: f32) -> Self {
        Self {
            weight: Some(forgive),
            limit: count * forgive,
        }
    }

    /// Count a period of `period` seconds, bad or not; true once the
    /// limit is reached
    fn count(&self, level: &mut f32, bad: bool, period: f32) -> bool {
        *level = if bad {
            *level + self.weight.unwrap_or(period)
        } else {
            (*level - period).max(0.0)
        };
        *level >= self.limit
    }
}

/// Load checks: a period in which the plugin takes at least this share of
/// it is bad for the rule.
/// - Half of it, for 0.3 s net: a sustained overload.
/// - Three quarters, three times, each forgiven after 0.25 s: with the
///   rest of the chain, the deadline is missed. A spike now and then (a
///   preset load, a burst of system activity) is harmless, several in a
///   row are not.
/// - The whole period, five times, each forgiven after 3 s: the deadline
///   is missed whatever the rest of the chain does, so a dropout the user
///   hears. Spikes far apart never add up, so a plugin that overruns one
///   period every half second (a buffer rebuilt in one go) is cut by this
///   rule only.
pub const LOAD_RULES: [(f32, Rule); 3] = [
    (0.5, Rule::sustained(0.3)),
    (0.75, Rule::events(3.0, 0.25)),
    (1.0, Rule::events(5.0, 3.0)),
];
/// Periods with a non-finite output: three, each forgiven after 0.25 s
pub const OUTPUT_RULE: Rule = Rule::events(3.0, 0.25);
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
    /// Count of each of `LOAD_RULES`, and of `OUTPUT_RULE`
    load: [f32; LOAD_RULES.len()],
    output: f32,
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
            load: [0.0; LOAD_RULES.len()],
            output: 0.0,
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

        // The output is checked from the start
        let trip = if OUTPUT_RULE.count(&mut self.output, non_finite, period) {
            Some(Trip::NonFinite)
        } else if self.grace > 0.0 {
            self.grace -= period;
            None
        } else {
            // Every rule counts the period, whichever trips
            let mut over = false;
            for ((share, rule), level) in LOAD_RULES.iter().zip(&mut self.load) {
                over |= rule.count(level, load >= *share, period);
            }
            over.then_some(Trip::Cpu)
        };
        self.tripped = trip.is_some();
        trip
    }
}

/// True if every sample of `buffers` is finite; else silences them all
/// (real-time safe). A non-finite sample would spread through what comes
/// after (filters and reverbs keep it in their state) and could reach the
/// speakers at full scale.
pub fn silence_non_finite(buffers: &mut [&mut [f32]]) -> bool {
    let finite = buffers.iter().all(|b| b.iter().all(|s| s.is_finite()));
    if !finite {
        for buffer in buffers.iter_mut() {
            buffer.fill(0.0);
        }
    }
    finite
}

/// The guard of one plugin of the chain: its watchdog, and its slot on the
/// [`GuardBoard`]
#[derive(Debug)]
pub struct Guard {
    watchdog: Watchdog,
    slot: Arc<AtomicU8>,
}

impl Guard {
    /// Guard of block `instance_id` (not real-time safe: called when
    /// building the chain)
    pub fn new(instance_id: usize, sample_rate: f32, window: usize) -> Self {
        Self {
            watchdog: Watchdog::new(sample_rate, window),
            slot: GuardBoard::global().slot(instance_id),
        }
    }

    /// After a run that took `nanos` and put out `outputs`: silences a
    /// non-finite output, and returns true when the plugin must be switched
    /// off now (the UI is told). Real-time safe.
    pub fn check(&mut self, nanos: u64, outputs: &mut [&mut [f32]]) -> bool {
        let frames = outputs.first().map_or(0, |b| b.len());
        let finite = silence_non_finite(outputs);
        let Some(trip) = self.watchdog.record(frames, nanos, finite) else {
            return false;
        };
        GuardBoard::global().report(&self.slot, trip);
        true
    }

    /// True once the plugin was switched off, until `rearm`
    pub fn tripped(&self) -> bool {
        self.watchdog.tripped()
    }

    /// The plugin is switched back on: check it from scratch
    pub fn rearm(&mut self) {
        self.watchdog.rearm();
        self.slot.store(0, Ordering::Release);
    }

    /// What the slot holds (tests)
    #[cfg(test)]
    pub fn reported(&self) -> u8 {
        self.slot.load(Ordering::Acquire)
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
        // Then a spike every half second (within the period), and short
        // heavy bursts
        for _ in 0..40 {
            assert_eq!(feed(&mut dog, 1, 0.9), None);
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
    fn periodic_overruns_are_cut_even_when_far_apart() {
        // Dragonfly Plate (Tank) or Room 3.2.10 on a Pi 5, 64 frames at
        // 48 kHz: about 1 % on average, but one block of about 1.8
        // periods every 32768 samples (0.68 s), a dropout each time
        const SMALL: usize = 64;
        let mut dog = Watchdog::new(RATE, SMALL);
        let small = |share: f32| (share * SMALL as f32 / RATE * 1e9) as u64;
        let mut blocks = 0usize;
        let cut = (0..60).find_map(|_| {
            let spike = dog.record(SMALL, small(1.8), true);
            let rest = (1..512).find_map(|_| dog.record(SMALL, small(0.01), true));
            blocks += 512;
            spike.or(rest)
        });
        assert_eq!(cut, Some(Trip::Cpu));
        let seconds = blocks as f32 * SMALL as f32 / RATE;
        assert!(seconds <= 6.0, "cut after {seconds} s");
    }

    #[test]
    fn a_rare_overrun_is_tolerated() {
        // One missed deadline every 4 s (a burst of system activity)
        let mut dog = Watchdog::new(RATE, PERIOD);
        for _ in 0..30 {
            assert_eq!(feed(&mut dog, 1, 2.0), None);
            assert_eq!(feed(&mut dog, periods(4.0), 0.2), None);
        }
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
    fn rules_count_in_seconds() {
        // Three events forgiven after 0.25 s: the third in a row trips,
        // one every 0.25 s never adds up
        let rule = Rule::events(3.0, 0.25);
        let mut level = 0.0;
        assert!(!rule.count(&mut level, true, 0.01));
        assert!(!rule.count(&mut level, true, 0.01));
        assert!(rule.count(&mut level, true, 0.01));
        let mut level = 0.0;
        for _ in 0..100 {
            assert!(!rule.count(&mut level, true, 0.01));
            for _ in 0..25 {
                assert!(!rule.count(&mut level, false, 0.01));
            }
        }
        // Sustained: bad periods add their length, net of the good ones
        let rule = Rule::sustained(0.3);
        let mut level = 0.0;
        for _ in 0..1000 {
            assert!(!rule.count(&mut level, true, 0.01));
            assert!(!rule.count(&mut level, false, 0.01));
        }
        assert!((0..32).any(|_| rule.count(&mut level, true, 0.01)));
    }

    #[test]
    fn non_finite_buffers_are_silenced_together() {
        let (mut l, mut r) = ([0.5, 0.25], [0.5, f32::INFINITY]);
        assert!(silence_non_finite(&mut [&mut l[..]]));
        assert_eq!(l, [0.5, 0.25]);
        assert!(!silence_non_finite(&mut [&mut l[..], &mut r[..]]));
        assert_eq!((l, r), ([0.0; 2], [0.0; 2]));
    }

    #[test]
    fn a_guard_silences_reports_and_rearms() {
        const ID: usize = 9301;
        let mut guard = Guard::new(ID, RATE, PERIOD);
        let nan = || vec![f32::NAN; PERIOD];
        let mut out = nan();
        assert!(!guard.check(0, &mut [&mut out]));
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(!guard.check(0, &mut [&mut nan()]));
        assert_eq!(guard.reported(), 0);
        assert!(guard.check(0, &mut [&mut nan()]));
        assert!(guard.tripped());
        assert_eq!(guard.reported(), Trip::NonFinite as u8);
        // Once
        assert!(!guard.check(0, &mut [&mut nan()]));
        guard.rearm();
        assert!(!guard.tripped());
        assert_eq!(guard.reported(), 0);
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
