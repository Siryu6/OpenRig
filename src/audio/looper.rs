//! Looper: record a loop, play it back and overdub layers on top.
//!
//! Loops are recorded in stereo. `process` runs on the audio thread and
//! never allocates. The control methods run on the UI thread through
//! [`Looper::control`], which allocates and frees the layers (11.5 MB each)
//! outside the lock the audio thread shares.

use std::sync::Mutex;

/// Maximum loop length
const MAX_LOOP_SECONDS: f32 = 30.0;
/// Overdubbed layers are recorded slightly lower to leave headroom
const OVERDUB_GAIN: f32 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperState {
    Stopped,
    Recording,
    Playing,
    Overdubbing,
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
}

impl Looper {
    pub fn new(sample_rate: f32) -> Self {
        let max_samples = (sample_rate * MAX_LOOP_SECONDS) as usize;
        let max_layers = 8;
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
        }
    }

    /// (left, right) of the loop at frame `position`
    fn loop_frame(&self, position: usize) -> (f32, f32) {
        let (l, r) = self.layers.iter().fold((0.0, 0.0), |(l, r), layer| {
            (l + layer[2 * position], r + layer[2 * position + 1])
        });
        (l * self.level, r * self.level)
    }

    fn advance(&mut self) {
        self.playhead += 1;
        if self.playhead >= self.loop_length {
            self.playhead = 0;
        }
    }

    /// Record the signal and mix the loop into it, in place (`left` and
    /// `right` have the same length)
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            match self.state {
                LooperState::Stopped => {}
                LooperState::Recording => {
                    if self.playhead < self.max_samples {
                        let frame = &mut self.layers[0][2 * self.playhead..][..2];
                        frame.copy_from_slice(&[*l, *r]);
                        self.playhead += 1;
                    } else {
                        // Buffer full: the loop is as long as it can be
                        self.loop_length = self.max_samples;
                        self.playhead = 0;
                        self.state = LooperState::Playing;
                    }
                }
                LooperState::Playing => {
                    let (loop_l, loop_r) = self.loop_frame(self.playhead);
                    *l += loop_l;
                    *r += loop_r;
                    self.advance();
                }
                LooperState::Overdubbing => {
                    let (loop_l, loop_r) = self.loop_frame(self.playhead);
                    if let Some(layer) = self.layers.last_mut() {
                        layer[2 * self.playhead] += *l * OVERDUB_GAIN;
                        layer[2 * self.playhead + 1] += *r * OVERDUB_GAIN;
                    }
                    *l += loop_l;
                    *r += loop_r;
                    self.advance();
                }
            }
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

    /// Start recording a new loop (erases the current one)
    pub fn record(&mut self) {
        self.garbage.append(&mut self.layers);
        let layer = self.take_layer();
        self.layers.push(layer);
        self.loop_length = 0;
        self.playhead = 0;
        self.state = LooperState::Recording;
    }

    /// Play the loop. While recording, this closes the loop at the current
    /// position; while overdubbing, it keeps the overdub and continues.
    pub fn play(&mut self) {
        match self.state {
            LooperState::Recording => {
                self.loop_length = self.playhead;
                self.playhead = 0;
                self.state = if self.loop_length > 0 {
                    LooperState::Playing
                } else {
                    LooperState::Stopped
                };
            }
            LooperState::Overdubbing => self.state = LooperState::Playing,
            LooperState::Stopped if self.loop_length > 0 => {
                self.playhead = 0;
                self.state = LooperState::Playing;
            }
            _ => {}
        }
    }

    pub fn stop(&mut self) {
        if self.state == LooperState::Recording {
            self.play();
        }
        self.state = LooperState::Stopped;
        self.playhead = 0;
    }

    /// Record a new layer on top of the loop, without moving the playhead
    pub fn overdub(&mut self) {
        if self.state == LooperState::Recording {
            self.play();
        }
        if self.loop_length == 0 || self.state == LooperState::Overdubbing {
            return;
        }
        if self.layers.len() < self.max_layers {
            let layer = self.take_layer();
            self.layers.push(layer);
        }
        // With all layers used, the last one keeps accumulating
        self.state = LooperState::Overdubbing;
    }

    /// Single-button control (footswitch):
    /// stopped → record → play → overdub → play → ...
    pub fn cycle(&mut self) {
        match self.state {
            LooperState::Stopped if self.loop_length == 0 => self.record(),
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
        self.state = LooperState::Stopped;
    }

    pub fn set_level(&mut self, level: f32) {
        self.level = level.clamp(0.0, 2.0);
    }

    pub fn state(&self) -> LooperState {
        self.state
    }

    /// Loop length in seconds (while recording: the length so far)
    pub fn loop_length_seconds(&self) -> f32 {
        let length = if self.state == LooperState::Recording {
            self.playhead
        } else {
            self.loop_length
        };
        length as f32 / self.sample_rate
    }

    pub fn position_seconds(&self) -> f32 {
        self.playhead as f32 / self.sample_rate
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
        // SAFETY: a valid, aligned element of the slice
        unsafe { std::ptr::write_volatile(sample, 0.0) };
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(looper.lock().unwrap().spare.is_none());
        Looper::control(&looper, |l| {
            assert!(l.spare.is_some());
            l.play();
            l.overdub();
        });
        Looper::control(&looper, |l| l.undo());
        let l = looper.lock().unwrap();
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
}
