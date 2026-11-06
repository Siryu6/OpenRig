//! Looper: record a loop, play it back and overdub layers on top.
//!
//! Loops are recorded in stereo. `process` runs on the audio thread and
//! never allocates. The control methods run on the UI thread through
//! [`Looper::control`], which allocates and frees the layers (11.5 MB each)
//! outside the lock the audio thread shares.
//!
//! With the tempo of the preset and a bar length, the looper can count one
//! bar in before recording and round the first recording to whole bars
//! (see [`LooperSettings`]). The bar grid starts where the recording starts;
//! its tempo is fixed for the loop when recording starts.

use std::f32::consts::TAU;
use std::sync::Mutex;

/// Maximum loop length
const MAX_LOOP_SECONDS: f32 = 30.0;
/// Overdubbed layers are recorded slightly lower to leave headroom
const OVERDUB_GAIN: f32 = 0.8;
/// A stop this close after a bar line ends the loop on that bar line instead
/// of adding an almost empty bar: an eighth of a beat, at most this long
const SNAP_MAX_SECONDS: f32 = 0.1;
/// Click: a decaying sine, higher on the first beat of the bar
const CLICK_SECONDS: f32 = 0.04;
const CLICK_DECAY_SECONDS: f32 = 0.006;
const CLICK_HZ: f32 = 1000.0;
const ACCENT_HZ: f32 = 1600.0;
/// Level of the other beats, relative to the first one
const CLICK_BEAT_GAIN: f32 = 0.6;

pub const MIN_BEATS_PER_BAR: u8 = 2;
pub const MAX_BEATS_PER_BAR: u8 = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperState {
    Stopped,
    /// Clicking one bar before recording
    CountIn,
    Recording,
    Playing,
    Overdubbing,
}

/// Count-in and bar quantization, kept with the session
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LooperSettings {
    /// Beats in a bar (4 for 4/4, 6 for 6/8 with the tempo in eighths)
    pub beats_per_bar: u8,
    /// Click one bar before recording; recording starts on the next downbeat
    pub count_in: bool,
    /// The first recording ends at the end of a bar
    pub quantize: bool,
    /// Click level, 0.0 to 1.0
    pub click_level: f32,
}

impl Default for LooperSettings {
    fn default() -> Self {
        Self {
            beats_per_bar: 4,
            count_in: false,
            quantize: false,
            click_level: 0.5,
        }
    }
}

impl LooperSettings {
    /// The settings within their ranges
    fn clamped(mut self) -> Self {
        self.beats_per_bar = self
            .beats_per_bar
            .clamp(MIN_BEATS_PER_BAR, MAX_BEATS_PER_BAR);
        self.click_level = if self.click_level.is_finite() {
            self.click_level.clamp(0.0, 1.0)
        } else {
            LooperSettings::default().click_level
        };
        self
    }
}

/// Samples in a beat at `bpm` (at least one)
pub fn beat_samples(sample_rate: f32, bpm: f32) -> usize {
    let bpm = crate::state::tempo::clamp_bpm(bpm);
    ((sample_rate * 60.0 / bpm).round() as usize).max(1)
}

/// Samples a recording lasts once rounded to whole bars of `bar` samples:
/// up to the end of the bar it stopped in, or back to the bar line it just
/// crossed when it stopped at most `snap` samples after it. One bar at least.
pub fn quantized_length(recorded: usize, bar: usize, snap: usize) -> usize {
    if bar == 0 {
        return recorded;
    }
    let bars = recorded.div_ceil(bar).max(1);
    let into_last = recorded.saturating_sub((bars - 1) * bar);
    if bars > 1 && into_last <= snap {
        (bars - 1) * bar
    } else {
        bars * bar
    }
}

/// Click generator: a short decaying sine. No allocation, no state but the
/// running click.
#[derive(Debug, Clone, Copy)]
struct Click {
    /// Samples left in the running click (0 = silent)
    remaining: usize,
    length: usize,
    phase: f32,
    step: f32,
    envelope: f32,
    decay: f32,
    gain: f32,
}

impl Click {
    fn new(sample_rate: f32) -> Self {
        Self {
            remaining: 0,
            length: (sample_rate * CLICK_SECONDS) as usize,
            phase: 0.0,
            step: 0.0,
            envelope: 0.0,
            decay: (-1.0 / (CLICK_DECAY_SECONDS * sample_rate)).exp(),
            gain: 0.0,
        }
    }

    fn trigger(&mut self, sample_rate: f32, accent: bool, level: f32) {
        let hz = if accent { ACCENT_HZ } else { CLICK_HZ };
        self.remaining = self.length;
        self.phase = 0.0;
        self.step = TAU * hz / sample_rate;
        self.envelope = 1.0;
        self.gain = level * if accent { 1.0 } else { CLICK_BEAT_GAIN };
    }

    fn next(&mut self) -> f32 {
        if self.remaining == 0 {
            return 0.0;
        }
        self.remaining -= 1;
        let sample = self.phase.sin() * self.envelope * self.gain;
        self.phase = (self.phase + self.step) % TAU;
        self.envelope *= self.decay;
        sample
    }
}

pub struct Looper {
    sample_rate: f32,
    max_samples: usize,
    /// Layer 0 is the first recording, the others are overdubs. Frames are
    /// interleaved (left, right).
    layers: Vec<Box<[f32]>>,
    max_layers: usize,
    /// Zeroed layer ready for the next recording or overdub
    spare: Option<Box<[f32]>>,
    /// Layers removed, to free outside the lock
    garbage: Vec<Box<[f32]>>,
    state: LooperState,
    /// Loop length in samples (0 = no loop)
    loop_length: usize,
    playhead: usize,
    level: f32,
    settings: LooperSettings,
    /// Tempo of the next recording
    bpm: f32,
    /// Beat and bar of the loop, in samples, fixed when recording starts
    beat_length: usize,
    bar_length: usize,
    /// Samples into the count-in
    count: usize,
    /// Quantized end of the recording, and the state that follows it
    end_at: Option<usize>,
    after_end: LooperState,
    click: Click,
}

impl Looper {
    pub fn new(sample_rate: f32) -> Self {
        let max_samples = (sample_rate * MAX_LOOP_SECONDS) as usize;
        let max_layers = 8;
        let settings = LooperSettings::default();
        let bpm = crate::presets::DEFAULT_BPM;
        let beat_length = beat_samples(sample_rate, bpm);
        Self {
            sample_rate,
            max_samples,
            layers: Vec::with_capacity(max_layers),
            max_layers,
            spare: None,
            garbage: Vec::with_capacity(max_layers),
            state: LooperState::Stopped,
            loop_length: 0,
            playhead: 0,
            level: 1.0,
            settings,
            bpm,
            beat_length,
            bar_length: beat_length * settings.beats_per_bar as usize,
            count: 0,
            end_at: None,
            after_end: LooperState::Playing,
            click: Click::new(sample_rate),
        }
    }

    /// An empty looper at `sample_rate` with the settings, tempo and level of
    /// this one
    pub fn with_sample_rate(&self, sample_rate: f32) -> Self {
        let mut looper = Self::new(sample_rate);
        looper.settings = self.settings;
        looper.bpm = self.bpm;
        looper.level = self.level;
        looper
    }

    /// (left, right) of the loop at frame `position`
    fn loop_frame(&self, position: usize) -> (f32, f32) {
        let (l, r) = self.layers.iter().fold((0.0, 0.0), |(l, r), layer| {
            match layer.get(2 * position..2 * position + 2) {
                Some(&[left, right]) => (l + left, r + right),
                _ => (l, r),
            }
        });
        (l * self.level, r * self.level)
    }

    fn advance(&mut self) {
        self.playhead += 1;
        if self.playhead >= self.loop_length {
            self.playhead = 0;
        }
    }

    /// Longest recording: the buffer, rounded down to whole bars when
    /// quantizing
    fn record_limit(&self) -> usize {
        if self.settings.quantize && self.bar_length <= self.max_samples {
            self.max_samples - self.max_samples % self.bar_length
        } else {
            self.max_samples
        }
    }

    /// A stop this many samples after a bar line ends the loop on it
    fn snap_samples(&self) -> usize {
        (self.beat_length / 8).min((self.sample_rate * SNAP_MAX_SECONDS) as usize)
    }

    fn click(&mut self, accent: bool) {
        self.click
            .trigger(self.sample_rate, accent, self.settings.click_level);
    }

    /// State changes due at the next frame: the clicks of the count-in, its
    /// end, the recording at its end
    fn on_frame(&mut self) {
        if self.state == LooperState::CountIn {
            if self.count >= self.bar_length {
                self.state = LooperState::Recording;
                self.playhead = 0;
            } else if self.count.is_multiple_of(self.beat_length) {
                self.click(self.count == 0);
            }
        }
        if self.state == LooperState::Recording {
            let end = self.end_at.unwrap_or(usize::MAX).min(self.record_limit());
            if self.playhead >= end {
                // Buffer full without an end asked: the loop goes on playing
                let next = if self.end_at.is_some() {
                    self.after_end
                } else {
                    LooperState::Playing
                };
                self.close_loop(end, next);
            }
        }
    }

    /// Record the signal and mix the loop into it, in place (`left` and
    /// `right` have the same length). The click is added after recording, so
    /// it is heard but never recorded.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.on_frame();
            match self.state {
                LooperState::Stopped => {}
                LooperState::CountIn => self.count += 1,
                LooperState::Recording => {
                    let at = 2 * self.playhead;
                    if let Some(frame) = self.layers.first_mut().and_then(|f| f.get_mut(at..at + 2))
                    {
                        frame.copy_from_slice(&[*l, *r]);
                    }
                    self.playhead += 1;
                }
                LooperState::Playing => {
                    let (loop_l, loop_r) = self.loop_frame(self.playhead);
                    *l += loop_l;
                    *r += loop_r;
                    self.advance();
                }
                LooperState::Overdubbing => {
                    let (loop_l, loop_r) = self.loop_frame(self.playhead);
                    let at = 2 * self.playhead;
                    if let Some([left, right]) = self
                        .layers
                        .last_mut()
                        .and_then(|layer| layer.get_mut(at..at + 2))
                    {
                        *left += *l * OVERDUB_GAIN;
                        *right += *r * OVERDUB_GAIN;
                    }
                    *l += loop_l;
                    *r += loop_r;
                    self.advance();
                }
            }
            let click = self.click.next();
            *l += click;
            *r += click;
        }
    }

    /// Run control methods from a thread other than the audio one: the next
    /// layer is allocated before taking the lock, removed ones are freed
    /// after, so the audio thread never waits on them.
    pub fn control<R>(looper: &Mutex<Looper>, f: impl FnOnce(&mut Looper) -> R) -> R {
        let lock = || looper.lock().unwrap_or_else(|e| e.into_inner());
        let needed = {
            let looper = lock();
            looper.spare.is_none().then_some(looper.max_samples)
        };
        let mut spare = needed.map(new_layer);
        let (result, garbage) = {
            let mut looper = lock();
            if looper.spare.is_none() {
                looper.spare = spare.take();
            }
            let result = f(&mut looper);
            (result, std::mem::take(&mut looper.garbage))
        };
        drop(garbage);
        result
    }

    fn take_layer(&mut self) -> Box<[f32]> {
        self.spare
            .take()
            .unwrap_or_else(|| new_layer(self.max_samples))
    }

    /// Start recording a new loop (erases the current one), after a bar of
    /// clicks with the count-in. During the count-in, cancels it.
    pub fn record(&mut self) {
        if self.state == LooperState::CountIn {
            self.clear();
            return;
        }
        self.garbage.append(&mut self.layers);
        let layer = self.take_layer();
        self.layers.push(layer);
        self.loop_length = 0;
        self.playhead = 0;
        self.end_at = None;
        self.beat_length = beat_samples(self.sample_rate, self.bpm);
        self.bar_length = self.beat_length * self.settings.beats_per_bar as usize;
        self.count = 0;
        self.state = if self.settings.count_in {
            LooperState::CountIn
        } else {
            LooperState::Recording
        };
    }

    /// End the first recording, then go to `next` (playing, stopped or
    /// overdubbing). Quantized, it ends at the end of the bar: now if it
    /// just crossed a bar line, else when the bar is over.
    fn end_recording(&mut self, next: LooperState) {
        if next == LooperState::Overdubbing && self.spare.is_none() {
            // Normally prepared by `control`: the audio thread takes it
            self.spare = Some(new_layer(self.max_samples));
        }
        if self.end_at.is_some() {
            self.after_end = next;
            return;
        }
        let recorded = self.playhead;
        let end = if self.settings.quantize {
            quantized_length(recorded, self.bar_length, self.snap_samples())
                .min(self.record_limit())
        } else {
            recorded
        };
        if end <= recorded {
            self.close_loop(end, next);
        } else {
            self.end_at = Some(end);
            self.after_end = next;
        }
    }

    /// The first recording becomes a loop of `length` samples. Runs on the
    /// audio thread too: no allocation.
    fn close_loop(&mut self, length: usize, next: LooperState) {
        self.end_at = None;
        self.loop_length = length;
        // Ended on a bar line already behind: the loop goes on from there
        let over = self.playhead.saturating_sub(length);
        self.playhead = if over < length { over } else { 0 };
        self.state = match next {
            _ if length == 0 => LooperState::Stopped,
            LooperState::Overdubbing => {
                if self.layers.len() < self.max_layers {
                    self.layers.extend(self.spare.take());
                }
                LooperState::Overdubbing
            }
            LooperState::Stopped => LooperState::Stopped,
            _ => LooperState::Playing,
        };
        if self.state == LooperState::Stopped {
            self.playhead = 0;
        }
    }

    /// Play the loop. While recording, this closes the loop (at the end of
    /// the bar when quantizing); while overdubbing, it keeps the overdub and
    /// continues. During the count-in, cancels it.
    pub fn play(&mut self) {
        match self.state {
            LooperState::CountIn => self.clear(),
            LooperState::Recording => self.end_recording(LooperState::Playing),
            LooperState::Overdubbing => self.state = LooperState::Playing,
            LooperState::Stopped if self.loop_length > 0 => {
                self.playhead = 0;
                self.state = LooperState::Playing;
            }
            _ => {}
        }
    }

    pub fn stop(&mut self) {
        match self.state {
            LooperState::CountIn => self.clear(),
            LooperState::Recording => self.end_recording(LooperState::Stopped),
            _ => {
                self.state = LooperState::Stopped;
                self.playhead = 0;
            }
        }
    }

    /// Record a new layer on top of the loop, without moving the playhead
    pub fn overdub(&mut self) {
        match self.state {
            LooperState::CountIn => self.clear(),
            LooperState::Recording => self.end_recording(LooperState::Overdubbing),
            LooperState::Overdubbing => {}
            _ if self.loop_length == 0 => {}
            _ => {
                if self.layers.len() < self.max_layers {
                    let layer = self.take_layer();
                    self.layers.push(layer);
                }
                // With all layers used, the last one keeps accumulating
                self.state = LooperState::Overdubbing;
            }
        }
    }

    /// Single-button control (footswitch):
    /// stopped → (count-in →) record → play → overdub → play → ...
    pub fn cycle(&mut self) {
        match self.state {
            LooperState::Stopped if self.loop_length == 0 => self.record(),
            LooperState::CountIn => self.record(),
            LooperState::Stopped | LooperState::Recording | LooperState::Overdubbing => self.play(),
            LooperState::Playing => self.overdub(),
        }
    }

    /// Remove the last overdub layer
    pub fn undo(&mut self) {
        if self.state == LooperState::Overdubbing {
            self.state = LooperState::Playing;
        }
        if self.layers.len() > 1 {
            self.garbage.extend(self.layers.pop());
        }
    }

    /// Erase the loop
    pub fn clear(&mut self) {
        self.garbage.append(&mut self.layers);
        self.loop_length = 0;
        self.playhead = 0;
        self.count = 0;
        self.end_at = None;
        self.state = LooperState::Stopped;
    }

    pub fn set_level(&mut self, level: f32) {
        if level.is_finite() {
            self.level = level.clamp(0.0, 2.0);
        }
    }

    /// Tempo of the next recording (the current loop keeps its own)
    pub fn set_tempo(&mut self, bpm: f32) {
        self.bpm = crate::state::tempo::clamp_bpm(bpm);
    }

    pub fn settings(&self) -> LooperSettings {
        self.settings
    }

    /// Settings for the next recording (and the click, at once)
    pub fn set_settings(&mut self, settings: LooperSettings) {
        self.settings = settings.clamped();
    }

    pub fn state(&self) -> LooperState {
        self.state
    }

    /// Loop length in seconds (while recording: the length so far)
    pub fn loop_length_seconds(&self) -> f32 {
        let length = match self.state {
            LooperState::Recording => self.playhead,
            LooperState::CountIn => 0,
            _ => self.loop_length,
        };
        length as f32 / self.sample_rate
    }

    pub fn position_seconds(&self) -> f32 {
        self.playhead as f32 / self.sample_rate
    }

    /// Bar (from 1; 0 during the count-in) and beat (from 1) at the playhead,
    /// while counting in, recording, or playing a loop of whole bars
    pub fn beat(&self) -> Option<(usize, usize)> {
        let at = match self.state {
            LooperState::CountIn => {
                let count = self.count.min(self.bar_length.saturating_sub(1));
                return Some((0, count / self.beat_length + 1));
            }
            LooperState::Recording => self.playhead,
            LooperState::Playing | LooperState::Overdubbing if self.loop_bars() > 0 => {
                self.playhead
            }
            _ => return None,
        };
        Some((
            at / self.bar_length + 1,
            at % self.bar_length / self.beat_length + 1,
        ))
    }

    /// Beats in a bar of the loop being recorded or played
    pub fn beats_per_bar(&self) -> usize {
        self.bar_length / self.beat_length
    }

    /// Bars in the loop when it is made of whole bars, else 0
    pub fn loop_bars(&self) -> usize {
        if self.loop_length > 0 && self.loop_length.is_multiple_of(self.bar_length) {
            self.loop_length / self.bar_length
        } else {
            0
        }
    }

    /// The recording goes on to the end of the bar
    pub fn ending(&self) -> bool {
        self.end_at.is_some()
    }

    /// Number of overdubs on top of the first recording
    #[cfg(test)]
    pub fn overdub_count(&self) -> usize {
        self.layers.len().saturating_sub(1)
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    pub fn level(&self) -> f32 {
        self.level
    }
}

/// A zeroed layer of `max_samples` stereo frames, with its memory mapped:
/// fresh zeroed allocations are mapped lazily by the kernel, and the page
/// faults would otherwise happen on the audio thread as it records.
fn new_layer(max_samples: usize) -> Box<[f32]> {
    let mut layer = vec![0.0f32; 2 * max_samples].into_boxed_slice();
    // One write per page (4 KiB is the smallest page size in use)
    for sample in layer.iter_mut().step_by(1024) {
        // An opaque zero: the compiler cannot drop the store as useless
        *sample = std::hint::black_box(0.0);
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::MutexExt;

    /// Left output for `value` on the left and `value / 2` on the right;
    /// checks that the right side follows
    fn run(looper: &mut Looper, value: f32, n: usize) -> Vec<f32> {
        let mut left = vec![value; n];
        let mut right = vec![value / 2.0; n];
        looper.process(&mut left, &mut right);
        for (l, r) in left.iter().zip(&right) {
            assert!((l / 2.0 - r).abs() < 1e-6);
        }
        left
    }

    #[test]
    fn records_then_plays_the_loop() {
        let mut looper = Looper::new(1000.0);
        looper.record();
        assert_eq!(looper.state(), LooperState::Recording);
        run(&mut looper, 0.5, 100);

        looper.play();
        assert_eq!(looper.state(), LooperState::Playing);
        assert_eq!(looper.loop_length_seconds(), 0.1);
        // Silence in, loop out
        assert!(run(&mut looper, 0.0, 250).iter().all(|&s| s == 0.5));
    }

    #[test]
    fn overdub_adds_a_layer_and_undo_removes_it() {
        let mut looper = Looper::new(1000.0);
        looper.record();
        run(&mut looper, 0.5, 100);
        looper.play();
        run(&mut looper, 0.0, 30);

        looper.overdub();
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert_eq!(looper.overdub_count(), 1);
        assert_eq!(looper.position_seconds(), 0.03); // playhead kept
        run(&mut looper, 0.25, 100);

        looper.play();
        let out = run(&mut looper, 0.0, 100);
        assert!(out
            .iter()
            .all(|&s| (s - (0.5 + 0.25 * OVERDUB_GAIN)).abs() < 1e-6));

        looper.undo();
        assert!(run(&mut looper, 0.0, 100).iter().all(|&s| s == 0.5));
    }

    #[test]
    fn recording_stops_when_the_buffer_is_full() {
        let mut looper = Looper::new(10.0); // 300 samples max
        looper.record();
        let out = run(&mut looper, 1.0, 400);
        assert_eq!(looper.state(), LooperState::Playing);
        assert_eq!(looper.loop_length_seconds(), MAX_LOOP_SECONDS);
        assert!(out.iter().all(|&s| s >= 1.0)); // every sample written
    }

    #[test]
    fn control_prepares_and_frees_layers_outside_the_lock() {
        let looper = Mutex::new(Looper::new(1000.0));
        Looper::control(&looper, |l| l.record());
        // The next layer is ready, the recording has its own
        assert!(looper.locked().spare.is_none());
        Looper::control(&looper, |l| {
            assert!(l.spare.is_some());
            l.play();
            l.overdub();
        });
        Looper::control(&looper, |l| l.undo());
        let l = looper.locked();
        assert_eq!(l.layer_count(), 1);
        assert!(l.garbage.is_empty());
        assert!(l.spare.is_some());
    }

    #[test]
    fn cycle_walks_through_the_states() {
        let mut looper = Looper::new(1000.0);
        looper.cycle();
        assert_eq!(looper.state(), LooperState::Recording);
        run(&mut looper, 0.1, 10);
        looper.cycle();
        assert_eq!(looper.state(), LooperState::Playing);
        looper.cycle();
        assert_eq!(looper.state(), LooperState::Overdubbing);
        looper.cycle();
        assert_eq!(looper.state(), LooperState::Playing);
    }

    /// 1000 Hz, 120 BPM in 4/4: beats of 500 samples, bars of 2000, a
    /// snap of 62 samples
    fn timed(settings: LooperSettings) -> Looper {
        let mut looper = Looper::new(1000.0);
        looper.set_tempo(120.0);
        looper.set_settings(settings);
        looper
    }

    /// 48 kHz, 120 BPM in 4/4: beats of 24000 samples, bars of 96000 (the
    /// click is a 1–1.6 kHz sine: too high for 1000 Hz)
    fn clicking(settings: LooperSettings) -> Looper {
        let mut looper = Looper::new(48000.0);
        looper.set_tempo(120.0);
        looper.set_settings(settings);
        looper
    }

    fn quantized() -> LooperSettings {
        LooperSettings {
            quantize: true,
            click_level: 0.0,
            ..LooperSettings::default()
        }
    }

    /// Output (left) for `input` on both sides
    fn feed(looper: &mut Looper, input: impl Iterator<Item = f32>) -> Vec<f32> {
        let mut left: Vec<f32> = input.collect();
        let mut right = left.clone();
        looper.process(&mut left, &mut right);
        left
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn beats_and_bars_in_samples() {
        assert_eq!(beat_samples(48000.0, 100.0), 28800);
        assert_eq!(beat_samples(44100.0, 120.0), 22050);
        // Out of range tempos are clamped
        assert_eq!(beat_samples(48000.0, 1000.0), 12000);
        let mut looper = Looper::new(48000.0);
        looper.set_tempo(100.0);
        looper.set_settings(LooperSettings {
            count_in: true,
            ..LooperSettings::default()
        });
        looper.record();
        // The count-in is one bar: 4 beats of 0.6 s
        assert_eq!(looper.bar_length, 4 * 28800);
        assert_eq!(looper.beats_per_bar(), 4);
    }

    #[test]
    fn recordings_round_to_whole_bars() {
        // Up to the end of the bar
        assert_eq!(quantized_length(2500, 2000, 62), 4000);
        assert_eq!(quantized_length(4063, 2000, 62), 6000);
        assert_eq!(quantized_length(4000, 2000, 62), 4000);
        // Just after a bar line: back to it
        assert_eq!(quantized_length(4030, 2000, 62), 4000);
        assert_eq!(quantized_length(4062, 2000, 62), 4000);
        // One bar at least
        assert_eq!(quantized_length(30, 2000, 62), 2000);
        assert_eq!(quantized_length(0, 2000, 62), 2000);
        let looper = timed(quantized());
        assert_eq!(looper.snap_samples(), 62);
        // Slow tempos: the snap stays short
        let mut slow = Looper::new(48000.0);
        slow.set_tempo(40.0);
        slow.record();
        assert_eq!(slow.snap_samples(), 4800);
    }

    #[test]
    fn count_in_clicks_one_bar_then_records_on_the_downbeat() {
        let mut looper = clicking(LooperSettings {
            count_in: true,
            click_level: 1.0,
            ..LooperSettings::default()
        });
        looper.record();
        assert_eq!(looper.state(), LooperState::CountIn);
        assert_eq!(looper.beat(), Some((0, 1)));
        assert_eq!(looper.loop_length_seconds(), 0.0);
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 96000));
        assert_eq!(looper.state(), LooperState::CountIn);
        assert_eq!(looper.beat(), Some((0, 4)));
        // A click on each beat, the first one louder
        let beats: Vec<f32> = out.chunks(24000).map(|beat| peak(&beat[..2000])).collect();
        assert_eq!(beats.len(), 4);
        assert!(beats.iter().all(|&p| p > 0.1));
        assert!(beats[0] > beats[1] * 1.2);
        assert_eq!(peak(&out[2000..24000]), 0.0);

        // The next frame is the first one recorded
        feed(&mut looper, (0..100).map(|i| 1.0 + i as f32));
        assert_eq!(looper.state(), LooperState::Recording);
        assert_eq!(looper.beat(), Some((1, 1)));
        looper.play();
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 100));
        assert_eq!(out[0], 1.0);
        assert_eq!(out[99], 100.0);
    }

    #[test]
    fn record_again_during_the_count_in_cancels_it() {
        let mut looper = timed(LooperSettings {
            count_in: true,
            ..LooperSettings::default()
        });
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.5, 700));
        assert_eq!(looper.beat(), Some((0, 2)));
        looper.record();
        assert_eq!(looper.state(), LooperState::Stopped);
        assert_eq!(looper.layer_count(), 0);
        // Nothing gets recorded afterwards
        feed(&mut looper, std::iter::repeat_n(0.5, 3000));
        assert_eq!(looper.loop_length_seconds(), 0.0);

        // The footswitch too
        looper.cycle();
        assert_eq!(looper.state(), LooperState::CountIn);
        looper.cycle();
        assert_eq!(looper.state(), LooperState::Stopped);
    }

    #[test]
    fn quantized_stop_records_to_the_end_of_the_bar() {
        let mut looper = timed(quantized());
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.5, 2500));
        looper.play();
        assert_eq!(looper.state(), LooperState::Recording);
        assert!(looper.ending());
        feed(&mut looper, std::iter::repeat_n(0.25, 1500));
        assert_eq!(looper.state(), LooperState::Recording);
        // The frame after the bar is the loop's first one
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 10));
        assert_eq!(looper.state(), LooperState::Playing);
        assert_eq!(out[0], 0.5);
        assert_eq!(looper.loop_length_seconds(), 4.0);
        assert_eq!(looper.loop_bars(), 2);
        assert_eq!(looper.beat(), Some((1, 1)));
        assert!(!looper.ending());

        // Overdubs keep the length
        looper.overdub();
        feed(&mut looper, std::iter::repeat_n(0.1, 5000));
        looper.play();
        assert_eq!(looper.loop_length_seconds(), 4.0);
    }

    #[test]
    fn quantized_stop_just_after_a_bar_line_snaps_back_to_it() {
        let mut looper = timed(quantized());
        looper.record();
        let input = (0..4030).map(|i| i as f32);
        feed(&mut looper, input);
        looper.play();
        assert_eq!(looper.state(), LooperState::Playing);
        assert_eq!(looper.loop_length_seconds(), 4.0);
        // Already 30 frames into the next pass: playing goes on from there
        assert_eq!(looper.position_seconds(), 0.03);
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 1));
        assert_eq!(out[0], 30.0);
    }

    #[test]
    fn quantized_stop_and_overdub_wait_for_the_bar() {
        let mut looper = timed(quantized());
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.5, 300));
        looper.stop();
        assert_eq!(looper.state(), LooperState::Recording);
        feed(&mut looper, std::iter::repeat_n(0.5, 1701));
        assert_eq!(looper.state(), LooperState::Stopped);
        assert_eq!(looper.loop_length_seconds(), 2.0);

        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.5, 300));
        looper.overdub();
        feed(&mut looper, std::iter::repeat_n(0.5, 1701));
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert_eq!(looper.overdub_count(), 1);
    }

    #[test]
    fn the_click_is_heard_but_not_recorded() {
        let mut looper = clicking(LooperSettings {
            click_level: 1.0,
            ..LooperSettings::default()
        });
        looper.record();
        looper.click(true);
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 4000));
        assert!(peak(&out[..2000]) > 0.1);
        looper.play();
        assert!(feed(&mut looper, std::iter::repeat_n(0.0, 4000))
            .iter()
            .all(|&s| s == 0.0));
    }

    #[test]
    fn settings_stay_in_range_and_survive_a_sample_rate_change() {
        let mut looper = Looper::new(1000.0);
        looper.set_settings(LooperSettings {
            beats_per_bar: 40,
            click_level: f32::NAN,
            quantize: true,
            ..LooperSettings::default()
        });
        assert_eq!(looper.settings().beats_per_bar, MAX_BEATS_PER_BAR);
        assert_eq!(looper.settings().click_level, 0.5);
        looper.set_tempo(90.0);
        let looper = looper.with_sample_rate(48000.0);
        assert!(looper.settings().quantize);
        assert_eq!(looper.bpm, 90.0);
        // Older sessions have no looper settings
        let old: LooperSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old, LooperSettings::default());
    }
}
