//! Looper: record a loop, play it back and overdub layers on top.
//!
//! Loops are recorded in stereo. `process` runs on the audio thread and
//! never allocates. The control methods run on the UI thread through
//! [`Looper::control`], which allocates and frees the layers (11.5 MB each)
//! outside the lock the audio thread shares.
//!
//! With the tempo of the preset and a bar length, the looper can click the
//! beats (a metronome), count one bar in before recording (or overdubbing
//! from a stop), and round the first recording to whole bars
//! (see [`LooperSettings`]). The bar grid starts where the recording starts;
//! its tempo is fixed for the loop when recording starts.
//!
//! The loop can be multiplied (repeated n times: each layer keeps its own
//! length and repeats, so this is instant), played at half speed (an octave
//! down, like the half speed of hardware loopers), and keeps a small peak
//! overview of itself for the waveform display, updated as it plays.

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
/// Points of the waveform overview of the loop
pub const WAVE_POINTS: usize = 256;
/// Points of the overview of the recording in progress, across the whole
/// buffer (15 ms each at 30 s): the loop's overview is made from them when
/// the recording ends
const RECORD_POINTS: usize = 2048;

pub const MIN_BEATS_PER_BAR: u8 = 2;
pub const MAX_BEATS_PER_BAR: u8 = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperState {
    Stopped,
    /// Clicking one bar before recording or overdubbing
    CountIn,
    Recording,
    Playing,
    Overdubbing,
}

/// Count-in, bar quantization and click, kept with the session
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LooperSettings {
    /// Beats in a bar (4 for 4/4, 6 for 6/8 with the tempo in eighths)
    pub beats_per_bar: u8,
    /// Click one bar before recording, or before overdubbing from a stop;
    /// recording starts on the next downbeat
    pub count_in: bool,
    /// The first recording ends at the end of a bar
    pub quantize: bool,
    /// Click every beat while recording, playing and overdubbing (the
    /// metronome). The first recording then always ends on a bar line, so the
    /// click stays in time with the loop.
    pub click: bool,
    /// Click level, 0.0 to 1.0
    pub click_level: f32,
}

impl Default for LooperSettings {
    fn default() -> Self {
        Self {
            beats_per_bar: 4,
            count_in: false,
            quantize: false,
            click: false,
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

/// What the UI shows, read in one go
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LooperStatus {
    pub state: LooperState,
    /// Seconds (while recording: so far)
    pub length: f32,
    pub position: f32,
    pub layers: usize,
    /// Bar (from 1; 0 during the count-in) and beat (from 1), when on the
    /// bar grid
    pub beat: Option<(usize, usize)>,
    pub beats_per_bar: usize,
    /// Bars in the loop, 0 when not whole bars
    pub bars: usize,
    pub ending: bool,
    pub half_speed: bool,
    /// The loop can be doubled (it fits the buffer twice)
    pub can_double: bool,
    /// Changes whenever the waveform overview does
    pub wave_version: u64,
}

/// A loop read from a file, to put in the looper (see [`Looper::load`]):
/// interleaved stereo frames at `sample_rate`
#[derive(Debug, Clone, PartialEq)]
pub struct LoopAudio {
    pub frames: Vec<f32>,
    pub sample_rate: u32,
    /// Bar grid of the loop when it was saved (0 bars: free length)
    pub bpm: f32,
    pub beats_per_bar: u8,
    pub bars: usize,
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

/// Peaks of `from` regrouped into `to`: each point of `to` is the highest of
/// the points of `from` it covers
fn regroup(from: &[f32], to: &mut [f32]) {
    let (n, m) = (from.len(), to.len());
    for (i, point) in to.iter_mut().enumerate() {
        let start = i * n / m;
        let end = ((i + 1) * n / m).max(start + 1).min(n);
        *point = from
            .get(start..end)
            .map_or(0.0, |s| s.iter().fold(0.0f32, |a, &b| a.max(b)));
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

/// A recording or overdub: interleaved (left, right) frames, as long as the
/// buffer. It repeats every `length` frames: a layer recorded before the
/// loop was multiplied plays once per repeat.
struct Layer {
    frames: Box<[f32]>,
    /// Frames in one pass of the layer (0 while the first one records)
    length: usize,
}

impl Layer {
    /// (left, right) at `position` of the loop
    fn at(&self, position: usize) -> (f32, f32) {
        let position = if position < self.length || self.length == 0 {
            position
        } else {
            position % self.length
        };
        match self.frames.get(2 * position..2 * position + 2) {
            Some(&[left, right]) => (left, right),
            _ => (0.0, 0.0),
        }
    }

    /// Write its repeats out, up to `length` frames, so it can be recorded
    /// on across the whole loop
    fn unfold(&mut self, length: usize) {
        let length = length.min(self.frames.len() / 2);
        if self.length == 0 || self.length >= length {
            return;
        }
        let mut filled = self.length;
        while filled < length {
            let n = (length - filled).min(filled);
            self.frames.copy_within(0..2 * n, 2 * filled);
            filled += n;
        }
        self.length = length;
    }
}

pub struct Looper {
    sample_rate: f32,
    max_samples: usize,
    /// Layer 0 is the first recording, the others are overdubs
    layers: Vec<Layer>,
    max_layers: usize,
    /// Zeroed layer ready for the next recording or overdub
    spare: Option<Box<[f32]>>,
    /// Layers removed, to free outside the lock
    garbage: Vec<Layer>,
    state: LooperState,
    /// Loop length in samples (0 = no loop)
    loop_length: usize,
    playhead: usize,
    /// Between `playhead` and the next frame, at half speed
    fraction: f32,
    half_speed: bool,
    level: f32,
    settings: LooperSettings,
    /// Tempo of the next recording
    bpm: f32,
    /// Beat and bar of the loop, in samples, fixed when recording starts
    beat_length: usize,
    bar_length: usize,
    /// Samples into the count-in, and the state that follows it
    count: usize,
    after_count: LooperState,
    /// Beat of the grid the last click was for (`usize::MAX`: none yet), so
    /// the metronome clicks once at the start of each beat
    click_beat: usize,
    /// Quantized end of the recording, and the state that follows it
    end_at: Option<usize>,
    after_end: LooperState,
    click: Click,
    /// Peaks of the loop (layers summed, before the level), refreshed as it
    /// plays: each point is rewritten on each pass
    wave: [f32; WAVE_POINTS],
    /// Point of `wave` being refreshed, and its peak so far on this pass
    wave_point: usize,
    wave_peak: f32,
    /// Peaks of the recording in progress, over the whole buffer
    record_wave: [f32; RECORD_POINTS],
    /// Bumped when the overview changes
    wave_version: u64,
    /// Bumped when the loop is replaced or erased
    loop_id: u64,
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
            fraction: 0.0,
            half_speed: false,
            level: 1.0,
            settings,
            bpm,
            beat_length,
            bar_length: beat_length * settings.beats_per_bar as usize,
            count: 0,
            after_count: LooperState::Recording,
            click_beat: usize::MAX,
            end_at: None,
            after_end: LooperState::Playing,
            click: Click::new(sample_rate),
            wave: [0.0; WAVE_POINTS],
            wave_point: usize::MAX,
            wave_peak: 0.0,
            record_wave: [0.0; RECORD_POINTS],
            wave_version: 0,
            loop_id: 0,
        }
    }

    /// An empty looper at `sample_rate` with the settings, tempo and level of
    /// this one
    pub fn with_sample_rate(&self, sample_rate: f32) -> Self {
        let mut looper = Self::new(sample_rate);
        looper.settings = self.settings;
        looper.bpm = self.bpm;
        looper.level = self.level;
        // A new overview, even if the old one was empty too
        looper.wave_version = self.wave_version + 1;
        looper
    }

    /// (left, right) of the layers summed at frame `position`
    fn mix(&self, position: usize) -> (f32, f32) {
        self.layers.iter().fold((0.0, 0.0), |(l, r), layer| {
            let (left, right) = layer.at(position);
            (l + left, r + right)
        })
    }

    /// (left, right) of the loop at the playhead, between two frames at
    /// half speed, after the level. Notes the peak for the overview, with
    /// `overdub` (what is being added to the loop there) so that an overdub
    /// shows as it is played rather than on the next pass.
    fn loop_frame(&mut self, overdub: (f32, f32)) -> (f32, f32) {
        let (mut l, mut r) = self.mix(self.playhead);
        if self.fraction == 0.0 {
            self.note_peak(l + overdub.0, r + overdub.1);
        } else {
            let next = if self.playhead + 1 < self.loop_length {
                self.playhead + 1
            } else {
                0
            };
            let (nl, nr) = self.mix(next);
            l += (nl - l) * self.fraction;
            r += (nr - r) * self.fraction;
        }
        (l * self.level, r * self.level)
    }

    /// Point of the overview at `position` of the loop
    fn wave_point_at(&self, position: usize) -> usize {
        (position * WAVE_POINTS / self.loop_length.max(1)).min(WAVE_POINTS - 1)
    }

    /// Note the loop's peak at the playhead in the overview: a point is
    /// cleared when the playhead enters it, so each pass rewrites it
    fn note_peak(&mut self, l: f32, r: f32) {
        let peak = l.abs().max(r.abs());
        let point = self.wave_point_at(self.playhead);
        if point != self.wave_point {
            if self.wave_point < WAVE_POINTS && self.wave[self.wave_point] != self.wave_peak {
                self.wave[self.wave_point] = self.wave_peak;
                self.wave_version += 1;
            }
            self.wave_point = point;
            self.wave_peak = 0.0;
        }
        self.wave_peak = self.wave_peak.max(peak);
    }

    /// Move the playhead one frame of output on (half a frame at half speed)
    fn advance(&mut self) {
        if self.half_speed {
            self.fraction += 0.5;
            if self.fraction < 1.0 {
                return;
            }
            self.fraction = 0.0;
        }
        self.playhead += 1;
        if self.playhead >= self.loop_length {
            self.playhead = 0;
            // A loop shorter than a beat still clicks on each pass
            self.click_beat = usize::MAX;
        }
    }

    /// The first recording is rounded to whole bars: asked for, or implied
    /// by the metronome (a loop off the bar grid would drift from it)
    fn quantizing(&self) -> bool {
        self.settings.quantize || self.settings.click
    }

    /// Longest recording: the buffer, rounded down to whole bars when
    /// quantizing
    fn record_limit(&self) -> usize {
        if self.quantizing() && self.bar_length <= self.max_samples {
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
    /// end, the recording at its end, the metronome
    fn on_frame(&mut self) {
        if self.state == LooperState::CountIn {
            if self.count >= self.bar_length {
                self.end_count_in();
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
        if matches!(
            self.state,
            LooperState::Recording | LooperState::Playing | LooperState::Overdubbing
        ) {
            // Followed even when the metronome is off: switched on, it
            // waits for the next beat
            let beat = self.playhead / self.beat_length;
            if beat != self.click_beat {
                self.click_beat = beat;
                if self.settings.click {
                    self.click(beat.is_multiple_of(self.beats_per_bar()));
                }
            }
        }
    }

    /// Start a bar of clicks, then go to `then`
    fn start_count_in(&mut self, then: LooperState) {
        self.count = 0;
        self.after_count = then;
        self.state = LooperState::CountIn;
    }

    /// The count-in is over: the loop starts on this downbeat. Runs on the
    /// audio thread: no allocation (the overdub layer was prepared).
    fn end_count_in(&mut self) {
        self.playhead = 0;
        self.click_beat = usize::MAX;
        self.state = self.after_count;
        if self.state == LooperState::Overdubbing {
            self.push_overdub_layer();
        }
    }

    /// Any transport key during the count-in cancels it: a recording to be
    /// leaves nothing, a loop to be overdubbed stays stopped
    fn cancel_count_in(&mut self) {
        if self.after_count == LooperState::Recording {
            self.clear();
        } else {
            self.count = 0;
            self.playhead = 0;
            self.state = LooperState::Stopped;
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
                    if let Some(frame) = self
                        .layers
                        .first_mut()
                        .and_then(|layer| layer.frames.get_mut(at..at + 2))
                    {
                        frame.copy_from_slice(&[*l, *r]);
                    }
                    let point = self.playhead * RECORD_POINTS / self.max_samples.max(1);
                    if let Some(peak) = self.record_wave.get_mut(point) {
                        let new = peak.max(l.abs()).max(r.abs());
                        if new != *peak {
                            *peak = new;
                            self.wave_version += 1;
                        }
                    }
                    self.playhead += 1;
                }
                LooperState::Playing => {
                    let (loop_l, loop_r) = self.loop_frame((0.0, 0.0));
                    *l += loop_l;
                    *r += loop_r;
                    self.advance();
                }
                LooperState::Overdubbing => {
                    // Always at full speed (see `overdub`)
                    let (loop_l, loop_r) = self.loop_frame((*l * OVERDUB_GAIN, *r * OVERDUB_GAIN));
                    let at = 2 * self.playhead;
                    if let Some([left, right]) = self
                        .layers
                        .last_mut()
                        .and_then(|layer| layer.frames.get_mut(at..at + 2))
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

    /// A new overdub layer over the whole loop, from the spare one; with all
    /// layers used, the last one keeps accumulating. On the audio thread: no
    /// allocation (`overdub` unfolded the last layer already). Overdubs are
    /// recorded at full speed, even when half speed was asked for during a
    /// count-in or a recording.
    fn push_overdub_layer(&mut self) {
        self.half_speed = false;
        self.fraction = 0.0;
        if self.layers.len() < self.max_layers {
            if let Some(frames) = self.spare.take() {
                self.layers.push(Layer {
                    frames,
                    length: self.loop_length,
                });
            }
        }
    }

    /// Start recording a new loop (erases the current one), after a bar of
    /// clicks with the count-in. During the count-in, cancels it.
    pub fn record(&mut self) {
        if self.state == LooperState::CountIn {
            self.cancel_count_in();
            return;
        }
        self.garbage.append(&mut self.layers);
        let frames = self.take_layer();
        self.layers.push(Layer { frames, length: 0 });
        self.loop_length = 0;
        self.playhead = 0;
        self.fraction = 0.0;
        self.half_speed = false;
        self.end_at = None;
        self.beat_length = beat_samples(self.sample_rate, self.bpm);
        self.bar_length = self.beat_length * self.settings.beats_per_bar as usize;
        self.click_beat = usize::MAX;
        self.record_wave = [0.0; RECORD_POINTS];
        self.wave = [0.0; WAVE_POINTS];
        self.wave_version += 1;
        self.loop_id += 1;
        if self.settings.count_in {
            self.start_count_in(LooperState::Recording);
        } else {
            self.count = 0;
            self.state = LooperState::Recording;
        }
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
        let end = if self.quantizing() {
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
        if let Some(first) = self.layers.first_mut() {
            first.length = length;
        }
        // The overview, from the recording's
        let points = (length * RECORD_POINTS)
            .div_ceil(self.max_samples.max(1))
            .clamp(1, RECORD_POINTS);
        regroup(&self.record_wave[..points], &mut self.wave);
        self.wave_point = usize::MAX;
        self.wave_version += 1;
        // Ended on a bar line already behind: the loop goes on from there
        let over = self.playhead.saturating_sub(length);
        self.playhead = if over < length { over } else { 0 };
        self.state = match next {
            _ if length == 0 => LooperState::Stopped,
            LooperState::Overdubbing => {
                self.push_overdub_layer();
                LooperState::Overdubbing
            }
            LooperState::Stopped => LooperState::Stopped,
            _ => LooperState::Playing,
        };
        if self.state == LooperState::Stopped {
            self.playhead = 0;
        }
        self.click_beat = usize::MAX;
    }

    /// Play the loop (at once: no count-in, the loop is its own guide).
    /// While recording, this closes the loop (at the end of the bar when
    /// quantizing); while overdubbing, it keeps the overdub and continues.
    /// During the count-in, cancels it.
    pub fn play(&mut self) {
        match self.state {
            LooperState::CountIn => self.cancel_count_in(),
            LooperState::Recording => self.end_recording(LooperState::Playing),
            LooperState::Overdubbing => self.state = LooperState::Playing,
            LooperState::Stopped if self.loop_length > 0 => {
                self.playhead = 0;
                self.fraction = 0.0;
                self.click_beat = usize::MAX;
                self.state = LooperState::Playing;
            }
            _ => {}
        }
    }

    pub fn stop(&mut self) {
        match self.state {
            LooperState::CountIn => self.cancel_count_in(),
            LooperState::Recording => self.end_recording(LooperState::Stopped),
            _ => {
                self.state = LooperState::Stopped;
                self.playhead = 0;
                self.fraction = 0.0;
            }
        }
    }

    /// Record a new layer on top of the loop, without moving the playhead
    /// (from a stop, after a bar of clicks with the count-in). Overdubs are
    /// recorded at full speed: at half speed, the loop goes back to it.
    pub fn overdub(&mut self) {
        match self.state {
            LooperState::CountIn => self.cancel_count_in(),
            LooperState::Recording => self.end_recording(LooperState::Overdubbing),
            LooperState::Overdubbing => {}
            _ if self.loop_length == 0 => {}
            _ => {
                self.set_half_speed(false);
                // The audio thread takes the layer: it must be ready
                if self.spare.is_none() {
                    self.spare = Some(new_layer(self.max_samples));
                }
                if self.layers.len() == self.max_layers {
                    // Its repeats are recorded on one by one
                    let length = self.loop_length;
                    if let Some(last) = self.layers.last_mut() {
                        last.unfold(length);
                    }
                }
                if self.state == LooperState::Stopped && self.settings.count_in {
                    self.playhead = 0;
                    self.start_count_in(LooperState::Overdubbing);
                    return;
                }
                if self.state == LooperState::Stopped {
                    self.click_beat = usize::MAX;
                }
                self.push_overdub_layer();
                self.state = LooperState::Overdubbing;
            }
        }
    }

    /// Single-button control (footswitch):
    /// stopped → (count-in →) record → play → overdub → play → ...
    /// During a count-in, cancels it.
    pub fn cycle(&mut self) {
        match self.state {
            LooperState::Stopped if self.loop_length == 0 => self.record(),
            LooperState::CountIn => self.cancel_count_in(),
            LooperState::Stopped | LooperState::Recording | LooperState::Overdubbing => self.play(),
            LooperState::Playing => self.overdub(),
        }
    }

    /// Remove the last overdub layer. Nothing during a count-in: nothing
    /// has been played yet, and the layers there were made before it (Stop
    /// cancels a count-in, as on a Boss RC)
    pub fn undo(&mut self) {
        if self.state == LooperState::CountIn {
            return;
        }
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
        self.fraction = 0.0;
        self.half_speed = false;
        self.count = 0;
        self.end_at = None;
        self.click_beat = usize::MAX;
        self.wave = [0.0; WAVE_POINTS];
        self.wave_version += 1;
        self.loop_id += 1;
        self.state = LooperState::Stopped;
    }

    /// Repeat the loop `times` times: overdubs then span the longer loop.
    /// Instant (the layers repeat, nothing is copied); an overdub running
    /// is kept and the loop plays on. False when there is no loop yet or the
    /// result would not fit the buffer.
    pub fn multiply(&mut self, times: usize) -> bool {
        if times < 2
            || self.loop_length == 0
            || matches!(self.state, LooperState::CountIn | LooperState::Recording)
        {
            return false;
        }
        let Some(length) = self
            .loop_length
            .checked_mul(times)
            .filter(|&length| length <= self.max_samples)
        else {
            return false;
        };
        if self.state == LooperState::Overdubbing {
            self.state = LooperState::Playing;
        }
        self.loop_length = length;
        // The overview repeats too
        let tiled: Vec<f32> = (0..times).flat_map(|_| self.wave).collect();
        regroup(&tiled, &mut self.wave);
        self.wave_point = usize::MAX;
        self.wave_version += 1;
        true
    }

    /// Play the loop at half speed (an octave down) or back at full speed.
    /// Overdubbing goes on playing: overdubs are recorded at full speed.
    pub fn set_half_speed(&mut self, half: bool) {
        if half && self.state == LooperState::Overdubbing {
            self.state = LooperState::Playing;
        }
        self.half_speed = half;
        if !half {
            self.fraction = 0.0;
        }
    }

    pub fn half_speed(&self) -> bool {
        self.half_speed
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

    /// Everything the UI shows, in one lock
    pub fn status(&self) -> LooperStatus {
        LooperStatus {
            state: self.state,
            length: self.loop_length_seconds(),
            position: self.position_seconds(),
            layers: self.layer_count(),
            beat: self.beat(),
            beats_per_bar: self.beats_per_bar(),
            bars: self.loop_bars(),
            ending: self.ending(),
            half_speed: self.half_speed,
            can_double: self.loop_length > 0
                && 2 * self.loop_length <= self.max_samples
                && !matches!(self.state, LooperState::CountIn | LooperState::Recording),
            wave_version: self.wave_version,
        }
    }

    /// Overview of the loop: [`WAVE_POINTS`] peaks from its start to its end
    /// (while recording: of the recording so far). Empty without a loop.
    pub fn waveform(&self) -> Vec<f32> {
        let mut wave = vec![0.0; WAVE_POINTS];
        match self.state {
            LooperState::Recording => {
                let points = (self.playhead * RECORD_POINTS / self.max_samples.max(1) + 1)
                    .min(RECORD_POINTS);
                regroup(&self.record_wave[..points], &mut wave);
            }
            _ if self.loop_length == 0 => wave.clear(),
            _ => wave.copy_from_slice(&self.wave),
        }
        wave
    }

    /// The loop as audio (layers summed, before the level), to save it.
    /// Copied a few thousand frames per lock, so the audio thread is not
    /// held up. None without a loop, while recording, or when the loop
    /// changes meanwhile.
    pub fn snapshot(looper: &Mutex<Looper>) -> Option<LoopAudio> {
        const CHUNK: usize = 4096;
        let lock = || looper.lock().unwrap_or_else(|e| e.into_inner());
        let (length, sample_rate, bpm, beats_per_bar, bars, id) = {
            let l = lock();
            if l.loop_length == 0
                || matches!(l.state, LooperState::Recording | LooperState::CountIn)
            {
                return None;
            }
            (
                l.loop_length,
                l.sample_rate,
                60.0 * l.sample_rate / l.beat_length as f32,
                l.beats_per_bar() as u8,
                l.loop_bars(),
                l.loop_id,
            )
        };
        let mut frames = vec![0.0f32; 2 * length];
        for (n, chunk) in frames.chunks_mut(2 * CHUNK).enumerate() {
            let l = lock();
            // A new recording, a clear, a multiply or a load
            if l.loop_length != length || l.loop_id != id {
                return None;
            }
            for (i, frame) in chunk.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let (left, right) = l.mix(n * CHUNK + i);
                *frame = [left, right];
            }
        }
        Some(LoopAudio {
            frames,
            sample_rate: sample_rate as u32,
            bpm,
            beats_per_bar,
            bars,
        })
    }

    /// Replace the loop with `audio` (stopped, ready to play). The layer is
    /// prepared (resampled to the looper's rate if needed) outside the lock.
    pub fn load(looper: &Mutex<Looper>, audio: &LoopAudio) -> Result<(), String> {
        let lock = || looper.lock().unwrap_or_else(|e| e.into_inner());
        let (sample_rate, max_samples) = {
            let l = lock();
            (l.sample_rate, l.max_samples)
        };
        let frames = resample(&audio.frames, audio.sample_rate as f32, sample_rate);
        let mut length = (frames.len() / 2).min(max_samples);
        if length == 0 {
            return Err("the loop is empty".into());
        }
        let beats = audio
            .beats_per_bar
            .clamp(MIN_BEATS_PER_BAR, MAX_BEATS_PER_BAR) as usize;
        // Whole bars stay whole bars, whatever the rounding of the rate
        let beat_length = if audio.bars > 0 && length >= audio.bars * beats {
            length / (audio.bars * beats)
        } else {
            beat_samples(sample_rate, audio.bpm)
        };
        if audio.bars > 0 && length >= audio.bars * beats {
            length = beat_length * beats * audio.bars;
        }
        let mut layer = new_layer(max_samples);
        layer[..2 * length].copy_from_slice(&frames[..2 * length]);
        let peaks: Vec<f32> = layer[..2 * length]
            .chunks(2 * length.div_ceil(RECORD_POINTS).max(1))
            .map(|c| c.iter().fold(0.0f32, |a, s| a.max(s.abs())))
            .collect();
        let mut wave = [0.0; WAVE_POINTS];
        regroup(&peaks, &mut wave);
        let garbage = {
            let mut l = lock();
            if l.sample_rate != sample_rate {
                return Err("the audio settings changed: load the loop again".into());
            }
            // Moved out without giving up the room for every layer: the
            // audio thread pushes overdub layers and must not reallocate
            let l = &mut *l;
            l.garbage.append(&mut l.layers);
            l.layers.push(Layer {
                frames: layer,
                length,
            });
            l.loop_length = length;
            l.playhead = 0;
            l.fraction = 0.0;
            l.half_speed = false;
            l.count = 0;
            l.end_at = None;
            l.beat_length = beat_length;
            l.bar_length = beat_length * beats;
            l.click_beat = usize::MAX;
            l.wave = wave;
            l.wave_point = usize::MAX;
            l.wave_version += 1;
            l.loop_id += 1;
            l.state = LooperState::Stopped;
            std::mem::take(&mut l.garbage)
        };
        drop(garbage);
        Ok(())
    }
}

/// Interleaved stereo `frames` at `from` Hz, at `to` Hz (linear
/// interpolation: loops come back at the rate they were saved at, unless
/// the audio interface changed)
fn resample(frames: &[f32], from: f32, to: f32) -> Vec<f32> {
    let n = frames.len() / 2;
    if from == to || n == 0 || from <= 0.0 || to <= 0.0 {
        return frames[..2 * n].to_vec();
    }
    let out = ((n as f64) * to as f64 / from as f64).round() as usize;
    let step = from as f64 / to as f64;
    let mut resampled = Vec::with_capacity(2 * out);
    for i in 0..out {
        let at = i as f64 * step;
        let j = (at as usize).min(n - 1);
        let k = (j + 1).min(n - 1);
        let t = (at - j as f64) as f32;
        for c in 0..2 {
            let (a, b) = (frames[2 * j + c], frames[2 * k + c]);
            resampled.push(a + (b - a) * t);
        }
    }
    resampled
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

    #[test]
    fn the_metronome_clicks_every_beat_and_accents_the_bar() {
        let mut looper = clicking(LooperSettings {
            click: true,
            click_level: 1.0,
            ..LooperSettings::default()
        });
        looper.record();
        // Recording: one click per beat, the first of the bar louder
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 2 * 96000));
        let beats: Vec<f32> = out.chunks(24000).map(|b| peak(&b[..2000])).collect();
        assert_eq!(beats.len(), 8);
        assert!(beats.iter().all(|&p| p > 0.1));
        assert!(beats[0] > beats[1] * 1.2 && beats[4] > beats[3] * 1.2);
        assert_eq!(peak(&out[2000..24000]), 0.0);
        // The click implies whole bars: a stop in bar 3 ends it
        feed(&mut looper, std::iter::repeat_n(0.0, 5000));
        looper.play();
        assert!(looper.ending());
        feed(&mut looper, std::iter::repeat_n(0.0, 91001));
        assert_eq!(looper.state(), LooperState::Playing);
        assert_eq!(looper.loop_bars(), 3);
        // Playing: the accent comes back with the loop
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 3 * 96000));
        let bars: Vec<f32> = out.chunks(96000).map(|b| peak(&b[..2000])).collect();
        assert!(bars.iter().all(|&p| p > 0.9));
        assert!(peak(&out[24000..26000]) < 0.7);

        // Switched off: silence
        looper.set_settings(LooperSettings {
            click: false,
            ..looper.settings()
        });
        // (after the click already running)
        assert_eq!(
            peak(&feed(&mut looper, std::iter::repeat_n(0.0, 96000))[2000..]),
            0.0
        );
    }

    #[test]
    fn overdub_from_a_stop_counts_in_play_does_not() {
        let mut looper = timed(LooperSettings {
            count_in: true,
            click_level: 0.0,
            ..LooperSettings::default()
        });
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.0, 2000));
        feed(&mut looper, std::iter::repeat_n(0.5, 2000));
        looper.stop();
        assert_eq!(looper.state(), LooperState::Stopped);

        // Play: at once
        looper.play();
        assert_eq!(looper.state(), LooperState::Playing);
        looper.stop();

        // Overdub: a bar of clicks first, the loop silent meanwhile
        looper.overdub();
        assert_eq!(looper.state(), LooperState::CountIn);
        assert_eq!(looper.layer_count(), 1);
        let out = feed(&mut looper, std::iter::repeat_n(0.25, 2000));
        assert!(out.iter().all(|&s| s == 0.25));
        feed(&mut looper, std::iter::repeat_n(0.25, 1));
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert_eq!(looper.overdub_count(), 1);
        assert_eq!(looper.position_seconds(), 0.001);

        // Cancelled, the loop stays
        looper.stop();
        looper.overdub();
        looper.play();
        assert_eq!(looper.state(), LooperState::Stopped);
        assert_eq!(looper.loop_length_seconds(), 2.0);
        // From playing, the overdub starts at once
        looper.play();
        looper.overdub();
        assert_eq!(looper.state(), LooperState::Overdubbing);
    }

    #[test]
    fn undo_does_nothing_during_a_count_in() {
        let mut looper = timed(LooperSettings {
            count_in: true,
            click_level: 0.0,
            ..LooperSettings::default()
        });
        // Before a first recording: nothing to undo, the count-in goes on
        looper.record();
        looper.undo();
        assert_eq!(looper.state(), LooperState::CountIn);
        feed(&mut looper, std::iter::repeat_n(0.0, 2001));
        assert_eq!(looper.state(), LooperState::Recording);
        feed(&mut looper, std::iter::repeat_n(0.5, 1999));
        looper.play();
        looper.overdub();
        feed(&mut looper, std::iter::repeat_n(0.25, 2000));
        looper.stop();
        assert_eq!(looper.overdub_count(), 1);

        // Before an overdub from a stop: the overdub already made stays
        looper.overdub();
        assert_eq!(looper.state(), LooperState::CountIn);
        looper.undo();
        assert_eq!(looper.state(), LooperState::CountIn);
        assert_eq!(looper.overdub_count(), 1);
        feed(&mut looper, std::iter::repeat_n(0.0, 2001));
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert_eq!(looper.overdub_count(), 2);
    }

    #[test]
    fn multiply_repeats_the_loop_and_overdubs_span_it() {
        let mut looper = Looper::new(1000.0);
        looper.record();
        feed(&mut looper, (0..100).map(|i| i as f32));
        looper.play();
        assert!(looper.multiply(2));
        assert_eq!(looper.loop_length_seconds(), 0.2);
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 200));
        assert_eq!(out[150], 50.0);
        // An overdub on the second half only
        feed(&mut looper, std::iter::repeat_n(0.0, 100));
        looper.overdub();
        feed(&mut looper, std::iter::repeat_n(1.0, 100));
        looper.play();
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 200));
        assert_eq!(out[50], 50.0);
        assert_eq!(out[150], 50.0 + OVERDUB_GAIN);
        // Too long for the buffer (30 s)
        assert!(!looper.multiply(200));
        assert!(looper.multiply(3));
        assert_eq!(looper.loop_length_seconds(), 0.6);
        // Nothing to multiply
        looper.clear();
        assert!(!looper.multiply(2));
    }

    #[test]
    fn with_all_layers_used_the_last_one_unfolds_before_an_overdub() {
        let mut looper = Looper::new(1000.0);
        looper.max_layers = 1;
        looper.record();
        feed(&mut looper, (0..100).map(|i| i as f32));
        looper.play();
        looper.multiply(2);
        feed(&mut looper, std::iter::repeat_n(0.0, 100));
        looper.overdub();
        assert_eq!(looper.layer_count(), 1);
        feed(&mut looper, std::iter::repeat_n(1.0, 100));
        looper.play();
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 200));
        // The overdub went into the second repeat only
        assert_eq!(out[10], 10.0);
        assert_eq!(out[110], 10.0 + OVERDUB_GAIN);
    }

    #[test]
    fn half_speed_plays_an_octave_down_and_overdubs_at_full_speed() {
        let mut looper = Looper::new(1000.0);
        looper.record();
        feed(&mut looper, (0..100).map(|i| i as f32));
        looper.play();
        looper.set_half_speed(true);
        let out = feed(&mut looper, std::iter::repeat_n(0.0, 200));
        // Twice as long, halfway frames in between
        assert_eq!(&out[..4], &[0.0, 0.5, 1.0, 1.5]);
        assert_eq!(out[199], 99.0 / 2.0); // back between the last and the first
        assert_eq!(looper.position_seconds(), 0.0);
        assert!(looper.status().half_speed);
        // An overdub goes back to full speed
        looper.overdub();
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert!(!looper.half_speed());
        // Half speed while overdubbing keeps the overdub and plays on
        looper.set_half_speed(true);
        assert_eq!(looper.state(), LooperState::Playing);
        // A new recording is at full speed
        looper.record();
        assert!(!looper.half_speed());
    }

    #[test]
    fn half_speed_asked_before_an_overdub_starts_is_dropped() {
        let mut looper = timed(LooperSettings {
            count_in: true,
            click_level: 0.0,
            ..LooperSettings::default()
        });
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.0, 2001));
        feed(&mut looper, std::iter::repeat_n(0.5, 1999));
        looper.stop();

        // During the count-in of an overdub
        looper.overdub();
        looper.set_half_speed(true);
        feed(&mut looper, std::iter::repeat_n(0.0, 2001));
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert!(!looper.half_speed());
        feed(&mut looper, std::iter::repeat_n(0.0, 999));
        assert_eq!(looper.position_seconds(), 1.0);

        // During a recording that goes on into an overdub
        looper.set_settings(LooperSettings {
            click_level: 0.0,
            ..LooperSettings::default()
        });
        looper.record();
        feed(&mut looper, std::iter::repeat_n(0.5, 100));
        looper.set_half_speed(true);
        looper.overdub();
        assert_eq!(looper.state(), LooperState::Overdubbing);
        assert!(!looper.half_speed());
        feed(&mut looper, std::iter::repeat_n(0.0, 50));
        assert_eq!(looper.position_seconds(), 0.05);
    }

    #[test]
    fn the_waveform_follows_the_loop() {
        let mut looper = Looper::new(1000.0); // 30000 frames max
        assert!(looper.waveform().is_empty());
        looper.record();
        let version = looper.status().wave_version;
        feed(
            &mut looper,
            (0..3000).map(|i| if i < 1500 { 0.5 } else { 0.25 }),
        );
        assert!(looper.status().wave_version > version);
        // While recording: what was recorded so far, across
        let wave = looper.waveform();
        assert_eq!(wave.len(), WAVE_POINTS);
        assert_eq!(wave[0], 0.5);
        assert_eq!(wave[WAVE_POINTS - 1], 0.25);
        looper.play();
        let wave = looper.waveform();
        assert_eq!(wave[10], 0.5);
        assert_eq!(wave[200], 0.25);
        // Played over, unchanged: no new version
        feed(&mut looper, std::iter::repeat_n(0.0, 3000));
        let version = looper.status().wave_version;
        feed(&mut looper, std::iter::repeat_n(0.0, 3000));
        assert_eq!(looper.status().wave_version, version);
        assert_eq!(looper.waveform()[10], 0.5);
        // An overdub shows as it is played
        looper.overdub();
        feed(&mut looper, std::iter::repeat_n(0.5, 3000));
        assert!((looper.waveform()[10] - (0.5 + 0.5 * OVERDUB_GAIN)).abs() < 1e-6);
        looper.play();
        feed(&mut looper, std::iter::repeat_n(0.0, 3000));
        assert!((looper.waveform()[10] - (0.5 + 0.5 * OVERDUB_GAIN)).abs() < 1e-6);
        // Undone: shows on the next pass. Multiplied: twice across
        looper.undo();
        feed(&mut looper, std::iter::repeat_n(0.0, 3000));
        assert_eq!(looper.waveform()[10], 0.5);
        looper.multiply(2);
        let wave = looper.waveform();
        assert_eq!(wave[5], 0.5);
        assert_eq!(wave[100], 0.25);
        assert_eq!(wave[133], 0.5);
        looper.clear();
        assert!(looper.waveform().is_empty());
    }

    #[test]
    fn a_saved_loop_loads_back_on_its_bars() {
        let looper = Mutex::new(timed(quantized()));
        assert!(Looper::snapshot(&looper).is_none());
        Looper::control(&looper, |l| {
            l.set_level(0.5);
            l.record();
        });
        feed(&mut looper.locked(), (0..3990).map(|i| i as f32 / 4000.0));
        assert!(Looper::snapshot(&looper).is_none()); // still recording
        Looper::control(&looper, |l| l.play());
        feed(&mut looper.locked(), std::iter::repeat_n(0.0, 11));
        let audio = Looper::snapshot(&looper).unwrap();
        assert_eq!(audio.frames.len(), 2 * 4000);
        assert_eq!(audio.sample_rate, 1000);
        assert_eq!((audio.bpm, audio.beats_per_bar, audio.bars), (120.0, 4, 2));
        // Before the level
        assert_eq!(audio.frames[2 * 100], 0.025);

        let other = Mutex::new(Looper::new(1000.0));
        Looper::load(&other, &audio).unwrap();
        let mut l = other.locked();
        assert_eq!(l.state(), LooperState::Stopped);
        // Room kept for every layer: overdubs never reallocate in the callback
        assert!(l.layers.capacity() >= l.max_layers);
        assert_eq!(l.loop_bars(), 2);
        assert_eq!(l.beats_per_bar(), 4);
        assert_eq!(l.waveform().len(), WAVE_POINTS);
        l.play();
        let out = feed(&mut l, std::iter::repeat_n(0.0, 4000));
        assert_eq!(out[100], 0.025);
        assert_eq!(l.beat(), Some((1, 1)));
        drop(l);

        // At another rate, the loop keeps its bars and its duration
        let other = Mutex::new(Looper::new(1500.0));
        Looper::load(&other, &audio).unwrap();
        let l = other.locked();
        assert_eq!(l.loop_bars(), 2);
        assert_eq!(l.loop_length_seconds(), 4.0);
        assert!((l.mix(150).0 - 0.025).abs() < 1e-4);
        drop(l);

        // Free length (no bars)
        let free = LoopAudio {
            frames: vec![0.1; 2 * 1234],
            sample_rate: 1000,
            bpm: 90.0,
            beats_per_bar: 3,
            bars: 0,
        };
        Looper::load(&other, &free).unwrap();
        assert_eq!(other.locked().loop_length, 1234 * 3 / 2);
        assert_eq!(other.locked().loop_bars(), 0);
        let empty = LoopAudio {
            frames: vec![],
            ..free
        };
        assert!(Looper::load(&other, &empty).is_err());
    }
}
