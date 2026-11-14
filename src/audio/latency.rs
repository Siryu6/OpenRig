//! Latency of the effects, and its compensation.
//!
//! A block can be late: an LV2 plugin reports how many frames on its
//! `lv2:reportsLatency` control output (a pitch shifter, a lookahead
//! compressor), and the output limiter looks 1 ms ahead. Wherever two paths
//! meet again (the branches of a split, a return mixing an input into the
//! chain, the dry signal of a block's bypass), the earlier path goes
//! through a [`DelayLine`] so both arrive together: no comb filtering. A
//! change of delay crossfades between the old and the new delay over
//! [`FADE_MS`], so it never clicks.
//!
//! [`LatencyReport`] carries what the audio thread finds to the JACK
//! latency callback (so a recorder aligns what it records from the
//! pedalboard) and to the UI; [`BlockLatency`] gives the latency of each
//! block to its editor.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::routing::{MAX_PORTS, MAX_SENDS};
use crate::sync::MutexExt;

/// Longest delay a path can be compensated by; a plugin reporting more is
/// taken at this
pub const MAX_LATENCY_MS: f32 = 100.0;
/// Crossfade between the old and the new delay when it changes
pub const FADE_MS: f32 = 10.0;

/// Longest compensation at `sample_rate`, frames
pub fn max_latency(sample_rate: f32) -> usize {
    (sample_rate.max(1.0) * MAX_LATENCY_MS / 1000.0).round() as usize
}

/// Frames to milliseconds
pub fn frames_to_ms(frames: usize, sample_rate: f32) -> f32 {
    frames as f32 * 1000.0 / sample_rate.max(1.0)
}

/// Delay of a whole number of frames, up to [`max_latency`]. Allocated when
/// built, then real-time safe. A new delay is reached by crossfading the
/// old and the new read positions; one asked for during a crossfade waits
/// for it to end. A line that has only held silence (just built or
/// cleared) takes the first delay asked for at once.
pub struct DelayLine {
    buffer: Box<[f32]>,
    mask: usize,
    /// Where the next frame is written
    pos: usize,
    max: usize,
    /// Delay played, delay faded to, and delay asked for
    delay: usize,
    next: usize,
    wanted: usize,
    /// Crossfade from `delay` to `next`, 0 to 1 (0: none running)
    fade: f32,
    step: f32,
    /// Nothing went through since the line was built or cleared
    fresh: bool,
}

impl DelayLine {
    /// Allocates: build off the audio thread
    pub fn new(sample_rate: f32) -> Self {
        let max = max_latency(sample_rate);
        let size = (max + 1).next_power_of_two();
        Self {
            buffer: vec![0.0; size].into_boxed_slice(),
            mask: size - 1,
            pos: 0,
            max,
            delay: 0,
            next: 0,
            wanted: 0,
            fade: 0.0,
            step: 1.0 / (sample_rate * FADE_MS / 1000.0).max(1.0),
            fresh: true,
        }
    }

    /// Longest delay, frames
    pub fn max(&self) -> usize {
        self.max
    }

    /// Delay heard once any crossfade is over, frames
    pub fn target(&self) -> usize {
        self.wanted
    }

    /// Go to `delay` frames (clamped to the longest), with a crossfade,
    /// or at once on a fresh line
    pub fn set(&mut self, delay: usize) {
        self.wanted = delay.min(self.max);
        if self.fresh {
            (self.delay, self.next, self.fade) = (self.wanted, self.wanted, 0.0);
        }
    }

    /// Forget the past: the next frames come after silence, and the next
    /// delay is taken at once
    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.fresh = true;
        self.set(self.wanted);
    }

    #[inline]
    fn read(&self, delay: usize) -> f32 {
        self.buffer[self.pos.wrapping_sub(delay) & self.mask]
    }

    /// Push one frame, return the one `delay` frames old
    #[inline]
    pub fn tick(&mut self, input: f32) -> f32 {
        self.fresh = false;
        self.buffer[self.pos] = input;
        if self.fade == 0.0 && self.wanted != self.delay {
            self.next = self.wanted;
            self.fade = self.step;
        }
        let output = if self.fade > 0.0 {
            let (from, to) = (self.read(self.delay), self.read(self.next));
            let output = from + (to - from) * self.fade;
            self.fade += self.step;
            if self.fade >= 1.0 {
                self.delay = self.next;
                self.fade = 0.0;
            }
            output
        } else {
            self.read(self.delay)
        };
        self.pos = (self.pos + 1) & self.mask;
        output
    }

    /// Delay `buffer` in place
    pub fn process(&mut self, buffer: &mut [f32]) {
        if self.delay == 0 && self.wanted == 0 && self.fade == 0.0 {
            // No delay: only keep the past, for when one comes
            self.fresh &= buffer.is_empty();
            for &sample in buffer.iter() {
                self.buffer[self.pos] = sample;
                self.pos = (self.pos + 1) & self.mask;
            }
            return;
        }
        for sample in buffer.iter_mut() {
            *sample = self.tick(*sample);
        }
    }
}

/// Latency of each block, by instance id, for its editor. The chain builder
/// hands a block the slot of its id when the block can be late; the audio
/// thread writes it, the UI reads it.
#[derive(Debug, Default)]
pub struct BlockLatency {
    slots: Mutex<HashMap<usize, Arc<AtomicU32>>>,
}

impl BlockLatency {
    pub fn global() -> &'static BlockLatency {
        static SLOTS: OnceLock<BlockLatency> = OnceLock::new();
        SLOTS.get_or_init(BlockLatency::default)
    }

    /// Slot of block `instance_id` (not real-time safe: when building)
    pub fn slot(&self, instance_id: usize) -> Arc<AtomicU32> {
        self.slots.locked().entry(instance_id).or_default().clone()
    }

    /// Frames block `instance_id` is late by; None for a block that is never
    /// late
    pub fn get(&self, instance_id: usize) -> Option<u32> {
        let slots = self.slots.locked();
        Some(slots.get(&instance_id)?.load(Ordering::Relaxed))
    }
}

/// Frames the effects put the signal out late by: the main mix, and the
/// shortest and longest of each send bus (None for a bus no send feeds)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectsLatency {
    pub chain: usize,
    pub sends: [Option<(usize, usize)>; MAX_SENDS],
}

impl EffectsLatency {
    /// Shortest and longest frames bus `bus` is late by: 0 is the main
    /// mix, then the sends; nothing on a bus no send feeds
    pub fn bus(&self, bus: usize) -> (usize, usize) {
        match bus.checked_sub(1) {
            None => (self.chain, self.chain),
            Some(send) => self.sends.get(send).copied().flatten().unwrap_or((0, 0)),
        }
    }
}

/// What an output plays, for its latency
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSource {
    Silent,
    /// The main mix or a send, fed by the effects input
    Effects,
    /// An input passed straight through
    Input,
}

/// Latency of the pedalboard's paths, written by the audio thread when it
/// changes, read by the JACK latency callback and the UI
#[derive(Debug, Default)]
pub struct LatencyReport {
    /// Effects (the main mix), frames
    chain: AtomicU32,
    /// Each output: shortest and longest frames from its input, the input
    /// (the effects input or the one passed through), and what it plays
    outputs: [[AtomicU32; 4]; MAX_PORTS],
    /// Bumped on every change
    version: AtomicU64,
}

/// One output's latency, as [`LatencyReport`] keeps it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLatency {
    pub min: u32,
    pub max: u32,
    pub input: usize,
    pub source: OutputSource,
}

impl LatencyReport {
    fn store(&self, slot: &AtomicU32, value: u32) {
        if slot.swap(value, Ordering::Relaxed) != value {
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    /// Latency of the effects, frames (audio thread)
    pub fn set_chain(&self, frames: usize) {
        self.store(&self.chain, frames as u32);
    }

    pub fn chain(&self) -> u32 {
        self.chain.load(Ordering::Relaxed)
    }

    /// Latency of output `port` (audio thread)
    pub fn set_output(&self, port: usize, latency: OutputLatency) {
        let Some(slots) = self.outputs.get(port) else {
            return;
        };
        let source = match latency.source {
            OutputSource::Silent => 0,
            OutputSource::Effects => 1,
            OutputSource::Input => 2,
        };
        for (slot, value) in
            slots
                .iter()
                .zip([latency.min, latency.max, latency.input as u32, source])
        {
            self.store(slot, value);
        }
    }

    pub fn output(&self, port: usize) -> OutputLatency {
        let value = |i: usize| {
            self.outputs
                .get(port)
                .map_or(0, |slots| slots[i].load(Ordering::Relaxed))
        };
        OutputLatency {
            min: value(0),
            max: value(1),
            input: value(2) as usize,
            source: match value(3) {
                1 => OutputSource::Effects,
                2 => OutputSource::Input,
                _ => OutputSource::Silent,
            },
        }
    }

    /// Changes so far: a new value means the latencies changed
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_by_whole_frames() {
        let mut line = DelayLine::new(48_000.0);
        // Fresh: at once
        line.set(3);
        let mut buffer = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        line.process(&mut buffer);
        assert_eq!(buffer, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);
        // Past the longest delay: clamped
        line.clear();
        line.set(1_000_000);
        assert_eq!(line.target(), max_latency(48_000.0));
    }

    #[test]
    fn a_fresh_line_takes_its_first_delay_at_once() {
        let mut line = DelayLine::new(48_000.0);
        line.set(2);
        let mut buffer = [1.0, 0.0, 0.0, 0.0];
        line.process(&mut buffer);
        assert_eq!(buffer, [0.0, 0.0, 1.0, 0.0]);
        // Once used, a new delay crossfades: still mostly the old one
        line.set(1);
        let mut buffer = [1.0, 0.0, 0.0, 0.0];
        line.process(&mut buffer);
        assert!(buffer[1] < 0.1 && buffer[2] > 0.9, "{buffer:?}");
        // Cleared: silence, then the next delay at once
        line.clear();
        line.set(3);
        let mut buffer = [1.0, 0.0, 0.0, 0.0, 0.0];
        line.process(&mut buffer);
        assert_eq!(buffer, [0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_new_delay_fades_in_without_a_click() {
        let rate = 48_000.0;
        let mut line = DelayLine::new(rate);
        let sine = |i: usize| 0.5 * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / rate).sin();
        let mut last = 0.0f32;
        let mut jump = 0.0f32;
        for i in 0..9600 {
            if i == 2000 {
                line.set(200);
            }
            if i == 2100 {
                // Asked for during the crossfade: follows it
                line.set(37);
            }
            let out = line.tick(sine(i));
            if i > 0 {
                jump = jump.max((out - last).abs());
            }
            last = out;
        }
        // A clean 440 Hz sine at 0.5 moves by 0.029 per frame at most; a
        // jump of the read position would move by up to 1
        assert!(jump < 0.04, "largest step {jump}");
        // Settled on the last delay asked for
        let out: Vec<f32> = (0..64)
            .map(|i| line.tick(if i == 0 { 1.0 } else { 0.0 }))
            .collect();
        assert_eq!(line.target(), 37);
        assert_eq!(out[37], 1.0);
        assert!(out[38..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn report_counts_changes_only() {
        let report = LatencyReport::default();
        let before = report.version();
        report.set_chain(64);
        let after = report.version();
        assert!(after > before);
        report.set_chain(64);
        assert_eq!(report.version(), after);
        let out = OutputLatency {
            min: 64,
            max: 112,
            input: 1,
            source: OutputSource::Effects,
        };
        report.set_output(3, out);
        assert_eq!(report.output(3), out);
        assert_eq!(report.output(99).source, OutputSource::Silent);
    }
}
