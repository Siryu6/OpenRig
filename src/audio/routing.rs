//! Inputs and outputs beyond the guitar input and one stereo pair: which
//! physical input feeds the chain, and what each physical output plays (the
//! chain's main mix, a send, or an input passed straight through), with its
//! level, balance, mute, solo and polarity.
//!
//! Sends and returns are blocks of a preset (`plugins::scanner::SEND_URI`,
//! `RETURN_URI`): a send copies the signal at its place to a send bus,
//! which outputs can play; a return brings an input of the interface in.
//!
//! Set in the Routing view and kept in `routing.json` in the data directory:
//! like `hardware.json`, it belongs to the rig, not to a preset, so changing
//! presets never touches the front-of-house levels. Without the file, the
//! main mix plays on outputs 1 and 2, as before multi-channel interfaces were
//! supported.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::latency::{OutputLatency, OutputSource};
use super::master::{self, Limiter, MasterProc, MasterTarget, OutputEq};
use crate::plugins::chain::{pan_gains, ramp};

const FILE: &str = "routing.json";
/// Inputs and outputs the pedalboard registers at most
pub const MAX_PORTS: usize = 16;
/// Send buses
pub const MAX_SENDS: usize = 4;
/// Level range of an output, in dB
pub const GAIN_RANGE_DB: (f32, f32) = (-60.0, 12.0);
/// Gain changes, mute, solo and source changes are ramped over this time
const RAMP_MS: f32 = 10.0;

/// What an output plays
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The chain output (after the looper)
    #[default]
    Main,
    /// A physical input as it comes in, without effects (0 = input 1)
    Input(u8),
    /// A send bus, filled by the sends of the chain (0 = send 1)
    Send(u8),
}

/// How a stereo source plays on a single output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fold {
    /// Both sides at half level
    #[default]
    Sum,
    Left,
    Right,
}

impl Fold {
    /// As the L+R / L / R choice of the routing view
    pub fn index(self) -> i32 {
        self as i32
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Fold::Left,
            2 => Fold::Right,
            _ => Fold::Sum,
        }
    }
}

/// One or two outputs playing a source
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    /// First output (0 = output 1)
    pub port: u8,
    /// A stereo pair: `port` and the next output
    pub stereo: bool,
    pub source: Source,
    /// A stereo source on a single output
    #[serde(default)]
    pub fold: Fold,
    #[serde(default)]
    pub gain_db: f32,
    /// Stereo pair: balance of the two sides, or position of a mono source.
    /// -1 left … 1 right
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Polarity flipped
    #[serde(default)]
    pub invert: bool,
    /// Global EQ, after the level (off by default)
    #[serde(default)]
    pub eq: OutputEq,
    /// Safety limiter, last (on by default)
    #[serde(default)]
    pub limiter: Limiter,
}

impl Strip {
    /// The main mix on `port` (and the next output if `stereo`), at unity
    pub fn main(port: u8, stereo: bool) -> Self {
        Self {
            port,
            stereo,
            source: Source::Main,
            fold: Fold::Sum,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            invert: false,
            eq: OutputEq::default(),
            limiter: Limiter::default(),
        }
    }

    /// Outputs it covers
    pub fn ports(&self) -> std::ops::Range<usize> {
        let first = self.port as usize;
        first..first + if self.stereo { 2 } else { 1 }
    }
}

/// The input feeding the chain and what each output plays
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routing {
    /// Input feeding the chain and the tuner (0 = input 1)
    pub input: u8,
    /// Outputs not covered by a strip are silent
    pub strips: Vec<Strip>,
}

impl Default for Routing {
    /// The main mix on outputs 1 and 2
    fn default() -> Self {
        Self {
            input: 0,
            strips: vec![Strip::main(0, true)],
        }
    }
}

impl Routing {
    /// The saved routing, else the default one
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let routing = crate::persist::load_json::<Self>(&path)
            .and_then(|r| r.map(|r| r.validate().map(|()| r)).transpose());
        routing
            .unwrap_or_else(|e| {
                warn!(
                    path = %path.display(),
                    error = format_args!("{e:#}"),
                    "Routing not loaded, using the default one"
                );
                None
            })
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.validate()?;
        crate::persist::save_json(&data_dir.join(FILE), self)
    }

    /// An output used by two strips, or one that cannot exist
    pub fn validate(&self) -> Result<()> {
        if self.input as usize >= MAX_PORTS {
            bail!("There is no input {}", self.input as usize + 1);
        }
        let mut used = [false; MAX_PORTS];
        for strip in &self.strips {
            match strip.source {
                Source::Input(i) if i as usize >= MAX_PORTS => {
                    bail!("There is no input {}", i as usize + 1)
                }
                Source::Send(s) if s as usize >= MAX_SENDS => {
                    bail!("There is no send {}", s as usize + 1)
                }
                _ => {}
            }
            if !strip.gain_db.is_finite() || !strip.pan.is_finite() {
                bail!("Output {}: invalid level or pan", strip.port as usize + 1);
            }
            if !strip.eq.is_valid() || !strip.limiter.is_valid() {
                bail!("Output {}: invalid EQ or limiter", strip.port as usize + 1);
            }
            for port in strip.ports() {
                if port >= MAX_PORTS {
                    bail!("There is no output {}", port + 1);
                }
                if std::mem::replace(&mut used[port], true) {
                    bail!("Output {} is used twice", port + 1);
                }
            }
        }
        Ok(())
    }

    /// Gains, EQ and limiters for the audio thread, for an interface with
    /// `inputs` and `outputs` ports running at `sample_rate`. A strip past
    /// the last output is ignored; a pair on the last output plays as a
    /// single output.
    pub fn mix(&self, inputs: usize, outputs: usize, sample_rate: f32) -> Mix {
        let mut mix = Mix {
            input: (self.input as usize).min(inputs.saturating_sub(1)) as u8,
            ports: [PortMix::SILENT; MAX_PORTS],
            masters: [MasterTarget::NONE; MAX_PORTS],
            widths: [0; MAX_PORTS],
        };
        let outputs = outputs.min(MAX_PORTS);
        let solo = self.strips.iter().any(|s| s.solo);
        for strip in &self.strips {
            let first = strip.port as usize;
            if first >= outputs {
                continue;
            }
            let heard = !strip.mute && (!solo || strip.solo);
            let gain = if heard {
                let db = strip.gain_db.clamp(GAIN_RANGE_DB.0, GAIN_RANGE_DB.1);
                let polarity = if strip.invert { -1.0 } else { 1.0 };
                10f32.powf(db / 20.0) * polarity
            } else {
                0.0
            };
            // A stereo bus (0 = main mix, then the sends), or an input
            let (bus, input) = match strip.source {
                Source::Main => (0, None),
                Source::Send(s) if (s as usize) < MAX_SENDS => (s + 1, None),
                Source::Input(i) if (i as usize) < inputs => (0, Some(i)),
                // An input this interface does not have
                Source::Input(_) | Source::Send(_) => continue,
            };
            mix.masters[first] = master::target(&strip.eq, &strip.limiter, sample_rate);
            mix.widths[first] = if strip.stereo && first + 1 < outputs {
                2
            } else {
                1
            };
            if strip.stereo && first + 1 < outputs {
                let (left, right) = pan_gains(strip.pan);
                for (port, side, side_gain) in [(first, 0, left), (first + 1, 1, right)] {
                    let g = gain * side_gain;
                    mix.ports[port] = match input {
                        None => {
                            let mut gains = [0.0; 2];
                            gains[side] = g;
                            PortMix::bus(bus, gains)
                        }
                        Some(i) => PortMix::input(i, g),
                    };
                }
            } else {
                mix.ports[first] = match input {
                    None => PortMix::bus(
                        bus,
                        match strip.fold {
                            Fold::Sum => [gain * 0.5, gain * 0.5],
                            Fold::Left => [gain, 0.0],
                            Fold::Right => [0.0, gain],
                        },
                    ),
                    Some(i) => PortMix::input(i, gain),
                };
            }
        }
        mix
    }
}

/// Editing, as done from the Routing view. `inputs` and `outputs` are the
/// ports of the interface: a change never reaches past them.
impl Routing {
    /// The strip starting on output `port`
    pub fn strip(&self, port: u8) -> Option<&Strip> {
        self.strips.iter().find(|s| s.port == port)
    }

    fn strip_mut(&mut self, port: u8) -> Option<&mut Strip> {
        self.strips.iter_mut().find(|s| s.port == port)
    }

    /// The strip playing on output `port`, whether it starts there or not
    fn covering(&self, port: u8) -> Option<&Strip> {
        self.strips
            .iter()
            .find(|s| s.ports().contains(&(port as usize)))
    }

    /// Next or previous input feeding the chain
    pub fn step_input(&mut self, delta: i32, inputs: usize) {
        let last = inputs.clamp(1, MAX_PORTS) as i32 - 1;
        self.input = (self.input as i32 + delta).clamp(0, last) as u8;
    }

    /// Next or previous source of the output (or pair) starting on `port`:
    /// off, the main mix, each send, then each input. Off removes the strip.
    pub fn step_source(&mut self, port: u8, delta: i32, inputs: usize, outputs: usize) {
        if port as usize >= outputs.min(MAX_PORTS) {
            return;
        }
        // Off, Main, Send 1..4, Input 1..n
        let sends = MAX_SENDS as i32;
        let count = 2 + sends + inputs.min(MAX_PORTS) as i32;
        let index = match self.strip(port).map(|s| s.source) {
            None if self.covering(port).is_some() => return,
            None => 0,
            Some(Source::Main) => 1,
            Some(Source::Send(s)) => 2 + s as i32,
            Some(Source::Input(i)) => 2 + sends + i as i32,
        };
        let next = (index + delta).clamp(0, count - 1);
        let source = match next {
            0 => {
                self.strips.retain(|s| s.port != port);
                return;
            }
            1 => Source::Main,
            n if n < 2 + sends => Source::Send((n - 2) as u8),
            n => Source::Input((n - 2 - sends) as u8),
        };
        match self.strip_mut(port) {
            Some(strip) => strip.source = source,
            None => {
                self.strips.push(Strip {
                    source,
                    ..Strip::main(port, false)
                });
                self.strips.sort_by_key(|s| s.port);
            }
        }
    }

    /// Whether the output (or pair) on `port` can become a pair with the
    /// next output: it plays something, and the next output exists and is
    /// not part of another pair
    pub fn can_link(&self, port: u8, outputs: usize) -> bool {
        let next = port as usize + 1;
        matches!(self.strip(port), Some(s) if !s.stereo)
            && next < outputs.min(MAX_PORTS)
            && self.strip(next as u8).is_none_or(|s| !s.stereo)
    }

    /// Make the output on `port` and the next one a stereo pair (the next
    /// output's own settings are dropped), or split a pair into two outputs
    /// playing the same source
    pub fn set_linked(&mut self, port: u8, linked: bool, outputs: usize) {
        if linked {
            if !self.can_link(port, outputs) {
                return;
            }
            self.strips.retain(|s| s.port != port + 1);
            if let Some(strip) = self.strip_mut(port) {
                strip.stereo = true;
            }
        } else if let (Some(next), Some(strip)) = (
            port.checked_add(1),
            self.strip_mut(port).filter(|s| s.stereo),
        ) {
            // Two outputs playing the same source: the left and right sides
            // of the main mix or a send, or the same input on both
            let bus = !matches!(strip.source, Source::Input(_));
            strip.stereo = false;
            strip.fold = if bus { Fold::Left } else { Fold::Sum };
            let second = Strip {
                port: next,
                fold: if bus { Fold::Right } else { Fold::Sum },
                ..*strip
            };
            self.strips.push(second);
            self.strips.sort_by_key(|s| s.port);
        }
    }

    /// Change the strip starting on `port`, if there is one
    pub fn edit(&mut self, port: u8, change: impl FnOnce(&mut Strip)) {
        if let Some(strip) = self.strip_mut(port) {
            change(strip);
            strip.gain_db = strip.gain_db.clamp(GAIN_RANGE_DB.0, GAIN_RANGE_DB.1);
            strip.pan = strip.pan.clamp(-1.0, 1.0);
            strip.eq.clamp();
            strip.limiter.clamp();
        }
    }
}

/// What one output plays, as gains
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortMix {
    /// Stereo bus played: 0 = the main mix, then the sends
    pub bus: u8,
    /// Gains of its left and right sides
    pub gains: [f32; 2],
    /// Input passed through, and its gain
    pub input: Option<u8>,
    pub input_gain: f32,
}

impl PortMix {
    pub const SILENT: Self = Self {
        bus: 0,
        gains: [0.0; 2],
        input: None,
        input_gain: 0.0,
    };

    fn bus(bus: u8, gains: [f32; 2]) -> Self {
        Self {
            bus,
            gains,
            ..Self::SILENT
        }
    }

    fn input(input: u8, gain: f32) -> Self {
        Self {
            input: Some(input),
            input_gain: gain,
            ..Self::SILENT
        }
    }

    fn is_silent(&self) -> bool {
        self.gains == [0.0; 2] && self.input_gain == 0.0
    }
}

/// What the audio thread plays: computed by [`Routing::mix`] off the audio
/// thread and sent whole (no allocation)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mix {
    /// Input feeding the chain
    pub input: u8,
    pub ports: [PortMix; MAX_PORTS],
    /// EQ and limiter of the strip starting on each output
    pub masters: [MasterTarget; MAX_PORTS],
    /// Outputs of the strip starting on each output: 0 (none, or the
    /// second side of a pair), 1 or 2
    pub widths: [u8; MAX_PORTS],
}

/// Peak level of every input and output, for the Routing view. Written by
/// the audio thread, read and reset by the UI.
#[derive(Debug)]
pub struct PortMeter {
    /// f32 bits: the order of the bits of non-negative floats is their
    /// numeric order, so `fetch_max` keeps the highest peak
    inputs: [AtomicU32; MAX_PORTS],
    outputs: [AtomicU32; MAX_PORTS],
    /// Gain reduction of the limiter of the strip starting on each output,
    /// 0 (none) to 1
    reductions: [AtomicU32; MAX_PORTS],
}

impl Default for PortMeter {
    fn default() -> Self {
        Self {
            inputs: std::array::from_fn(|_| AtomicU32::new(0)),
            outputs: std::array::from_fn(|_| AtomicU32::new(0)),
            reductions: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

impl PortMeter {
    fn record(slot: Option<&AtomicU32>, buffer: &[f32]) {
        let peak = buffer.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if let (Some(slot), true) = (slot, peak.is_finite()) {
            slot.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }

    /// Record one cycle of input `port` (real-time safe)
    pub fn record_input(&self, port: usize, buffer: &[f32]) {
        Self::record(self.inputs.get(port), buffer);
    }

    /// Record one cycle of output `port` (real-time safe)
    pub fn record_output(&self, port: usize, buffer: &[f32]) {
        Self::record(self.outputs.get(port), buffer);
    }

    /// Record the gain reduction of the limiter on output `port`, 0 to 1
    /// (real-time safe)
    pub fn record_reduction(&self, port: usize, reduction: f32) {
        if let (Some(slot), true) = (self.reductions.get(port), reduction.is_finite()) {
            slot.fetch_max(reduction.max(0.0).to_bits(), Ordering::Relaxed);
        }
    }

    /// Deepest gain reduction of each limiter since the previous call, dB
    /// (0 or below)
    pub fn take_reductions(&self) -> [f32; MAX_PORTS] {
        std::array::from_fn(|i| {
            let r = f32::from_bits(self.reductions[i].swap(0, Ordering::Relaxed));
            20.0 * (1.0 - r.min(0.999_999)).log10()
        })
    }

    /// Peaks of the inputs and outputs since the previous call
    pub fn take(&self) -> ([f32; MAX_PORTS], [f32; MAX_PORTS]) {
        let take = |slots: &[AtomicU32; MAX_PORTS]| {
            std::array::from_fn(|i| f32::from_bits(slots[i].swap(0, Ordering::Relaxed)))
        };
        (take(&self.inputs), take(&self.outputs))
    }
}

/// Sends a new routing to the audio thread
#[derive(Clone)]
pub struct RoutingControl {
    tx: Sender<Mix>,
    /// Ports the pedalboard registered
    pub inputs: usize,
    pub outputs: usize,
    /// The routing the audio thread started with
    pub initial: Routing,
    pub sample_rate: f32,
}

impl RoutingControl {
    pub fn apply(&self, routing: &Routing) {
        let mix = routing.mix(self.inputs, self.outputs, self.sample_rate);
        // Disconnected: the audio thread is gone (shutdown, audio restart)
        if let Err(TrySendError::Full(_)) = self.tx.try_send(mix) {
            warn!("Audio thread not taking routing changes, change dropped");
        }
    }
}

/// Output mixer of the audio thread: plays the main mix, the sends and the
/// inputs on the outputs, with every gain ramped, then runs the EQ and
/// limiter of each output or pair
pub struct OutputRouter {
    target: Mix,
    current: [PortMix; MAX_PORTS],
    rx: Receiver<Mix>,
    step: f32,
    /// EQ and limiter of the strip starting on each output
    masters: Vec<MasterProc>,
}

impl OutputRouter {
    /// Router starting on `routing`, and the control to change it
    pub fn new(
        routing: &Routing,
        inputs: usize,
        outputs: usize,
        sample_rate: f32,
    ) -> (Self, RoutingControl) {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let target = routing.mix(inputs, outputs, sample_rate);
        let router = Self {
            target,
            current: target.ports,
            rx,
            step: 1.0 / (sample_rate * RAMP_MS / 1000.0).max(1.0),
            masters: (0..MAX_PORTS)
                .map(|_| MasterProc::new(sample_rate))
                .collect(),
        };
        (
            router,
            RoutingControl {
                tx,
                inputs,
                outputs,
                initial: routing.clone(),
                sample_rate,
            },
        )
    }

    /// Take the latest routing sent (real-time safe), once per cycle
    pub fn update(&mut self) {
        while let Ok(mix) = self.rx.try_recv() {
            self.target = mix;
        }
    }

    /// Input feeding the chain
    pub fn chain_input(&self) -> usize {
        self.target.input as usize
    }

    /// Fill output `port` from the stereo buses (`buses[0]` is the main
    /// mix, then the sends) and the inputs (`input(i)` is the buffer of
    /// input i)
    pub fn render<'a>(
        &mut self,
        port: usize,
        buses: &[[&[f32]; 2]],
        input: impl Fn(u8) -> Option<&'a [f32]>,
        out: &mut [f32],
    ) {
        let Some(current) = self.current.get_mut(port) else {
            out.fill(0.0);
            return;
        };
        let target = self.target.ports[port];
        let stereo = |bus: u8| buses.get(bus as usize).copied();
        if *current == target {
            // Steady: no ramp
            if target.is_silent() {
                out.fill(0.0);
                return;
            }
            let bus = stereo(target.bus);
            let buffer = target.input.and_then(&input);
            for (i, o) in out.iter_mut().enumerate() {
                let played = bus.map_or(0.0, |[l, r]| {
                    target.gains[0] * sample(l, i) + target.gains[1] * sample(r, i)
                });
                let passed = buffer.map_or(0.0, |b| sample(b, i) * target.input_gain);
                *o = played + passed;
            }
            return;
        }
        // Another bus or input: fade the current one out, then the new one in
        let mut bus = stereo(current.bus);
        let mut buffer = current.input.and_then(&input);
        for (i, o) in out.iter_mut().enumerate() {
            let other_bus = current.bus != target.bus;
            for side in 0..2 {
                let gain = if other_bus { 0.0 } else { target.gains[side] };
                ramp(&mut current.gains[side], gain, self.step);
            }
            if other_bus && current.gains == [0.0; 2] {
                current.bus = target.bus;
                bus = stereo(current.bus);
            }
            let other_input = current.input != target.input;
            let gain = if other_input { 0.0 } else { target.input_gain };
            ramp(&mut current.input_gain, gain, self.step);
            if other_input && current.input_gain == 0.0 {
                current.input = target.input;
                buffer = current.input.and_then(&input);
            }
            let played = bus.map_or(0.0, |[l, r]| {
                current.gains[0] * sample(l, i) + current.gains[1] * sample(r, i)
            });
            let passed = buffer.map_or(0.0, |b| sample(b, i) * current.input_gain);
            *o = played + passed;
        }
    }
}

impl OutputRouter {
    /// Run the EQ and limiter of each strip on `outputs`, the buffers
    /// [`Self::render`] filled (every output, in order), and record the
    /// limiters' gain reduction (real-time safe)
    pub fn master(&mut self, outputs: &mut [&mut [f32]], meter: &PortMeter) {
        let count = outputs.len().min(MAX_PORTS);
        for (port, proc) in self.masters.iter_mut().enumerate() {
            let width = self.target.widths[port] as usize;
            if width == 0 || port + width > count {
                proc.idle();
                continue;
            }
            let reduction =
                proc.process(&self.target.masters[port], &mut outputs[port..port + width]);
            meter.record_reduction(port, reduction);
        }
    }
}

impl OutputRouter {
    /// Latency of output `port`: of what it plays (the main mix, late by
    /// `chain` frames, a send, late by `sends`, or an input), plus the
    /// lookahead of its limiter (real-time safe)
    pub fn output_latency(
        &self,
        port: usize,
        chain: usize,
        sends: &[Option<(usize, usize)>],
    ) -> OutputLatency {
        let silent = OutputLatency {
            min: 0,
            max: 0,
            input: self.chain_input(),
            source: OutputSource::Silent,
        };
        let Some(target) = self.target.ports.get(port).filter(|t| !t.is_silent()) else {
            return silent;
        };
        // The strip starting on this output, or the pair it is the right
        // side of
        let widths = &self.target.widths;
        let start = if widths.get(port).is_some_and(|&w| w > 0) {
            Some(port)
        } else {
            port.checked_sub(1).filter(|&p| widths.get(p) == Some(&2))
        };
        let lookahead = start
            .filter(|&s| self.target.masters.get(s).is_some_and(|m| m.limiter))
            .and_then(|s| self.masters.get(s))
            .map_or(0, MasterProc::latency);
        let (min, max, input, source) = match target.input {
            Some(input) => (0, 0, input as usize, OutputSource::Input),
            None => {
                let (min, max) = match target.bus {
                    0 => (chain, chain),
                    bus => sends
                        .get(bus as usize - 1)
                        .copied()
                        .flatten()
                        .unwrap_or((0, 0)),
                };
                (min, max, self.chain_input(), OutputSource::Effects)
            }
        };
        OutputLatency {
            min: (min + lookahead) as u32,
            max: (max + lookahead) as u32,
            input,
            source,
        }
    }
}

/// Sample `i` of `buffer`, silence past its end (a buffer shorter than the
/// output)
fn sample(buffer: &[f32], i: usize) -> f32 {
    buffer.get(i).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn strip(port: u8, stereo: bool, source: Source) -> Strip {
        Strip {
            source,
            ..Strip::main(port, stereo)
        }
    }

    #[test]
    fn default_plays_the_main_mix_on_outputs_1_and_2() {
        let mix = Routing::default().mix(1, 4, RATE);
        assert_eq!(mix.ports[0].gains, [1.0, 0.0]);
        assert_eq!(mix.ports[1].gains, [0.0, 1.0]);
        assert!(mix.ports[2].is_silent() && mix.ports[3].is_silent());
    }

    #[test]
    fn a_pair_on_the_last_output_folds_to_mono() {
        let mix = Routing::default().mix(1, 1, RATE);
        assert_eq!(mix.ports[0].gains, [0.5, 0.5]);
    }

    #[test]
    fn mono_outputs_fold_the_main_mix() {
        let routing = Routing {
            input: 0,
            strips: vec![
                Strip {
                    fold: Fold::Left,
                    ..Strip::main(2, false)
                },
                Strip {
                    fold: Fold::Right,
                    ..Strip::main(3, false)
                },
            ],
        };
        let mix = routing.mix(2, 4, RATE);
        assert_eq!(mix.ports[2].gains, [1.0, 0.0]);
        assert_eq!(mix.ports[3].gains, [0.0, 1.0]);
    }

    #[test]
    fn solo_mutes_the_others_and_invert_flips_the_polarity() {
        let routing = Routing {
            input: 0,
            strips: vec![
                Strip::main(0, true),
                Strip {
                    solo: true,
                    invert: true,
                    ..strip(2, false, Source::Input(1))
                },
            ],
        };
        let mix = routing.mix(2, 4, RATE);
        assert!(mix.ports[0].is_silent() && mix.ports[1].is_silent());
        assert_eq!(mix.ports[2].input, Some(1));
        assert_eq!(mix.ports[2].input_gain, -1.0);
    }

    #[test]
    fn missing_inputs_and_outputs_are_ignored() {
        let routing = Routing {
            input: 5,
            strips: vec![strip(0, false, Source::Input(3)), Strip::main(6, true)],
        };
        let mix = routing.mix(2, 4, RATE);
        assert_eq!(mix.input, 1);
        assert!(mix.ports.iter().all(PortMix::is_silent));
    }

    #[test]
    fn overlapping_strips_are_refused() {
        let routing = Routing {
            input: 0,
            strips: vec![Strip::main(0, true), Strip::main(1, false)],
        };
        assert!(routing.validate().is_err());
        assert!(Routing::default().validate().is_ok());
        let missing_send = Routing {
            input: 0,
            strips: vec![Strip {
                source: Source::Send(MAX_SENDS as u8),
                ..Strip::main(0, true)
            }],
        };
        assert!(missing_send.validate().is_err());
    }

    #[test]
    fn saved_routing_is_read_back() {
        let dir = std::env::temp_dir().join(format!("pedalboard-routing-{}", uuid::Uuid::new_v4()));
        let routing = Routing {
            input: 1,
            strips: vec![
                Strip::main(0, true),
                Strip {
                    gain_db: -6.0,
                    ..strip(2, false, Source::Input(1))
                },
            ],
        };
        routing.save(&dir).unwrap();
        assert_eq!(Routing::load(&dir), routing);
        std::fs::write(dir.join(FILE), b"{oops").unwrap();
        assert_eq!(Routing::load(&dir), Routing::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn router_ramps_to_a_new_routing() {
        let (mut router, control) = OutputRouter::new(&Routing::default(), 2, 4, 1000.0);
        let main = [1.0f32; 8];
        let guitar = [0.5f32; 8];
        let input = |i: u8| (i == 1).then_some(&guitar[..]);
        let mut out = [0.0f32; 8];
        router.render(0, &[[&main, &main]], input, &mut out);
        assert_eq!(out, [1.0; 8]);

        // Output 1 switches to input 2: the main mix fades out over 10 ms
        // (10 samples at 1 kHz) while the input fades in
        control.apply(&Routing {
            input: 0,
            strips: vec![strip(0, false, Source::Input(1))],
        });
        router.update();
        let mut out = [0.0f32; 16];
        let main = [1.0f32; 16];
        let guitar = [0.5f32; 16];
        let input = |i: u8| (i == 1).then_some(&guitar[..]);
        router.render(0, &[[&main, &main]], input, &mut out);
        assert!(out[0] > 0.5 && out[0] < 1.0, "ramping: {}", out[0]);
        assert!((out[15] - 0.5).abs() < 1e-6, "input alone: {}", out[15]);
    }

    #[test]
    fn sources_step_from_off_to_the_inputs() {
        let mut routing = Routing::default();
        routing.step_source(2, 1, 2, 4);
        assert_eq!(routing.strip(2).map(|s| s.source), Some(Source::Main));
        routing.step_source(2, 1, 2, 4);
        assert_eq!(routing.strip(2).map(|s| s.source), Some(Source::Send(0)));
        routing.step_source(2, MAX_SENDS as i32 + 1, 2, 4);
        assert_eq!(routing.strip(2).map(|s| s.source), Some(Source::Input(1)));
        // No input 3 on this interface
        routing.step_source(2, 1, 2, 4);
        assert_eq!(routing.strip(2).map(|s| s.source), Some(Source::Input(1)));
        routing.step_source(2, -9, 2, 4);
        assert!(routing.strip(2).is_none());
        // Output 2 belongs to the 1–2 pair; output 5 does not exist
        routing.step_source(1, 1, 2, 4);
        routing.step_source(4, 1, 2, 4);
        assert_eq!(routing, Routing::default());
    }

    #[test]
    fn a_send_plays_on_its_outputs() {
        let routing = Routing {
            input: 0,
            strips: vec![
                Strip::main(0, true),
                strip(2, true, Source::Send(1)),
                strip(4, false, Source::Send(MAX_SENDS as u8)),
            ],
        };
        let mix = routing.mix(1, 6, RATE);
        assert_eq!((mix.ports[2].bus, mix.ports[2].gains), (2, [1.0, 0.0]));
        assert_eq!((mix.ports[3].bus, mix.ports[3].gains), (2, [0.0, 1.0]));
        // No such send
        assert!(mix.ports[4].is_silent());

        let (mut router, _) = OutputRouter::new(&routing, 1, 6, 48000.0);
        let (main, send) = ([1.0f32; 4], [0.25f32; 4]);
        let mut out = [0.0f32; 4];
        router.render(
            3,
            &[[&main, &main], [&send, &send], [&send, &send]],
            |_| None,
            &mut out,
        );
        assert_eq!(out, [0.25; 4]);
        // A missing bus is silent
        router.render(3, &[[&main, &main]], |_| None, &mut out);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn linking_makes_a_pair_and_unlinking_keeps_both_sides() {
        let mut routing = Routing::default();
        routing.set_linked(0, false, 4);
        assert_eq!(
            routing.strip(0).map(|s| (s.stereo, s.fold)),
            Some((false, Fold::Left))
        );
        assert_eq!(
            routing.strip(1).map(|s| (s.stereo, s.fold)),
            Some((false, Fold::Right))
        );
        // The same sound; each side now has its own limiter
        assert_eq!(
            routing.mix(1, 4, RATE).ports,
            Routing::default().mix(1, 4, RATE).ports
        );

        routing.set_linked(0, true, 4);
        assert!(routing.strip(0).is_some_and(|s| s.stereo));
        assert!(routing.strip(1).is_none());
        assert!(routing.validate().is_ok());

        // The last output has no next one to pair with
        routing.step_source(3, 1, 1, 4);
        assert!(!routing.can_link(3, 4));
        routing.set_linked(3, true, 4);
        assert!(routing.strip(3).is_some_and(|s| !s.stereo));
    }

    #[test]
    fn edits_stay_in_range() {
        let mut routing = Routing::default();
        routing.edit(0, |s| {
            s.gain_db = 40.0;
            s.pan = -3.0;
        });
        let strip = routing.strip(0).unwrap();
        assert_eq!((strip.gain_db, strip.pan), (GAIN_RANGE_DB.1, -1.0));
        routing.step_input(5, 2);
        assert_eq!(routing.input, 1);
        routing.step_input(-5, 2);
        assert_eq!(routing.input, 0);
    }

    #[test]
    fn a_file_from_before_the_eq_and_limiter_loads_with_the_limiter_on() {
        let old = r#"{ "input": 0, "strips": [
            { "port": 0, "stereo": true, "source": "main", "gain_db": -3.0 } ] }"#;
        let routing: Routing = serde_json::from_str(old).unwrap();
        let strip = routing.strip(0).unwrap();
        assert!(!strip.eq.on);
        assert!(strip.limiter.on);
        assert_eq!(strip.limiter.ceiling_db, -1.0);
        assert!(routing.validate().is_ok());
        let mix = routing.mix(1, 2, RATE);
        assert_eq!(mix.widths[..2], [2, 0]);
        assert!(mix.masters[0].limiter);
    }

    #[test]
    fn a_pair_is_limited_as_one() {
        let (mut router, _) = OutputRouter::new(&Routing::default(), 1, 2, RATE);
        let meter = PortMeter::default();
        // Left over the ceiling, right far below: both sides get the same
        // reduction, so the image stays
        let main_left = [2.0f32; 256];
        let main_right = [0.1f32; 256];
        let (mut left, mut right) = ([0.0f32; 256], [0.0f32; 256]);
        for _ in 0..4 {
            router.render(0, &[[&main_left, &main_right]], |_| None, &mut left);
            router.render(1, &[[&main_left, &main_right]], |_| None, &mut right);
            router.master(&mut [&mut left, &mut right], &meter);
        }
        let ceiling = 10f32.powf(-1.0 / 20.0);
        assert!(left.iter().all(|x| x.abs() <= ceiling * 1.000_001));
        assert!((left[255] / right[255] - 20.0).abs() < 0.01);
        let reduction = meter.take_reductions()[0];
        assert!((reduction + 7.0).abs() < 0.1, "{reduction} dB");
        assert_eq!(meter.take_reductions()[0], 0.0);
    }

    #[test]
    fn port_meter_keeps_the_peak_until_read() {
        let meter = PortMeter::default();
        meter.record_output(2, &[0.1, -0.6, 0.3]);
        meter.record_output(2, &[0.2]);
        meter.record_input(0, &[0.5]);
        let (inputs, outputs) = meter.take();
        assert_eq!((inputs[0], outputs[2]), (0.5, 0.6));
        assert_eq!(meter.take().1[2], 0.0);
    }
}
