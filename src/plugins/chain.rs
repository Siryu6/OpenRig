//! Real-time effect chain: a tree of plugins and splits (A/B, Mix, L/R),
//! built off the audio thread from a `ChainSpec` and processed in place.
//!
//! The guitar comes in mono. In stereo mode the signal becomes stereo where
//! something makes it so: an L/R split (top branch to the left output,
//! bottom branch to the right one), a plugin with two outputs, or a stereo
//! impulse response. Whether each point of the chain is mono or stereo is
//! known when the chain is built (`SignalMap` gives it to the UI): mono
//! points only process the left buffer. A Mix split can also place each
//! branch between left and right (pan).
//! On a stereo signal, impulse responses and mono LV2 plugins run twice
//! (one instance per side); neural models, too costly to double, play the
//! mix of both sides on both. In mono mode everything stays mono and L/R
//! splits are summed.
//!
//! Send and return blocks (`scanner::SEND_URI`, `RETURN_URI`) are run by
//! the chain itself: a return brings an input of the interface into the
//! signal, a send adds the signal to a send bus (`ChainIo`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::warn;

use super::instance::PluginInstance;
use super::lv2_host::Lv2Host;
use super::scanner::{self, FileKind};
use crate::audio::dsp_pool;
use crate::audio::spectrum::BlockTap;
use crate::audio::split_chain::{ChainNode, SplitType};
use crate::midi::RawMidi;
use crate::monitor::BlockMeter;
use crate::sync::MutexExt;

/// How long a new LV2 instance may take to load its files before its
/// chain goes to the audio thread anyway
const FILE_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Split gain ramp duration (A/B switching and mix changes)
const SPLIT_RAMP_SECONDS: f32 = 0.01;

/// Everything needed to build a chain: structure plus parameter values.
#[derive(Debug, Clone, Default)]
pub struct ChainSpec {
    /// Incremented by the UI side on every structural change; commands sent
    /// after this spec carry the same generation so they can be replayed on
    /// the chain once it is built.
    pub generation: u64,
    pub nodes: Vec<ChainNode>,
    /// Parameter values per plugin instance_id
    pub params: HashMap<usize, Vec<f32>>,
    /// File loaded by a block (NAM model), as an absolute path
    pub files: HashMap<usize, PathBuf>,
    /// Files of the LV2 plugins that load some: (parameter URI, absolute
    /// path) per instance_id
    pub param_files: HashMap<usize, Vec<(String, PathBuf)>>,
    /// Two outputs (L/R splits and stereo blocks); otherwise mono
    pub stereo: bool,
    /// Tempo sent to plugins taking the transport (0: none)
    pub bpm: f32,
}

/// What the chain reads and writes besides its own signal, for one audio
/// period: the inputs of the interface (returns) and the send buses (sends)
pub struct ChainIo<'a> {
    /// Buffer of each input, for the whole period
    inputs: &'a [&'a [f32]],
    /// Send buses (left, right), for the whole period; sends add to them,
    /// so the caller clears them every period
    sends: &'a mut [[Vec<f32>; 2]],
    /// Start of the part of the period being processed
    offset: usize,
    /// Level of the sends over that part (a chain fading out of a switch
    /// fades its sends too): at its start, and its change per sample
    gain: f32,
    gain_step: f32,
}

impl<'a> ChainIo<'a> {
    pub fn new(inputs: &'a [&'a [f32]], sends: &'a mut [[Vec<f32>; 2]]) -> Self {
        Self {
            inputs,
            sends,
            offset: 0,
            gain: 1.0,
            gain_step: 0.0,
        }
    }

    /// The part of the period starting at `offset`, with the sends going
    /// from `from` to `to` over its `len` samples
    pub fn part(&mut self, offset: usize, len: usize, from: f32, to: f32) {
        self.offset = offset;
        self.gain = from;
        self.gain_step = (to - from) / len.max(1) as f32;
    }

    /// The same inputs and part, without the sends: for a branch run on
    /// another core, which must not add to the buses at the same time
    pub fn without_sends(&self) -> ChainIo<'a> {
        ChainIo {
            inputs: self.inputs,
            sends: &mut [],
            offset: self.offset,
            gain: self.gain,
            gain_step: self.gain_step,
        }
    }
}

/// No inputs and no sends: returns play silence, sends go nowhere
pub fn no_io<R>(f: impl FnOnce(&mut ChainIo) -> R) -> R {
    let mut sends: [[Vec<f32>; 2]; 0] = [];
    f(&mut ChainIo::new(&[], &mut sends))
}

/// Live change applied to the running chain without rebuilding it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChainCommand {
    SetParameter {
        instance_id: usize,
        index: usize,
        value: f32,
    },
    /// Bypass a plugin or a whole split
    SetEnabled {
        instance_id: usize,
        enabled: bool,
    },
    SetSplitAb {
        instance_id: usize,
        select_a: bool,
    },
    SetSplitMix {
        instance_id: usize,
        ratio: f32,
    },
    /// Mix split: position of the top and bottom branches (-1 left … 1 right)
    SetSplitPan {
        instance_id: usize,
        pan: [f32; 2],
    },
    /// Tempo for every plugin taking the transport (time:Position)
    SetTempo {
        bpm: f32,
    },
}

impl ChainCommand {
    /// Instance the command is for; None when it is for all of them
    fn target(&self) -> Option<usize> {
        Some(match *self {
            ChainCommand::SetParameter { instance_id, .. }
            | ChainCommand::SetEnabled { instance_id, .. }
            | ChainCommand::SetSplitAb { instance_id, .. }
            | ChainCommand::SetSplitMix { instance_id, .. }
            | ChainCommand::SetSplitPan { instance_id, .. } => instance_id,
            ChainCommand::SetTempo { .. } => return None,
        })
    }
}

/// Left and right gains placing a branch at `pan` (-1 left … 1 right). The
/// centre keeps both sides at full level, so a centred mix sounds as in
/// mono; hard left and right make an L/R split.
pub(crate) fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// True for a Mix split that places a branch off centre (stereo output)
pub fn is_panned(split_type: SplitType, pan: [f32; 2]) -> bool {
    split_type == SplitType::Mix && pan.iter().any(|&p| p != 0.0)
}

/// Move `current` towards `target` by at most `step`
#[inline]
pub(crate) fn ramp(current: &mut f32, target: f32, step: f32) {
    if *current < target {
        *current = (*current + step).min(target);
    } else if *current > target {
        *current = (*current - step).max(target);
    }
}

/// How a plugin runs on its input: once per side (dual mono) or not, and
/// whether its output is stereo
fn plugin_routing(
    spec: &ChainSpec,
    host: &Lv2Host,
    uri: &str,
    instance_id: usize,
    stereo_in: bool,
) -> (bool, bool) {
    let file = spec.files.get(&instance_id).map(PathBuf::as_path);
    let scanner = host.scanner();
    let info = scanner.find_by_uri(uri);
    // Impulse responses run once per side on a stereo signal or with a
    // stereo file; mono LV2 plugins on a stereo signal. Neural models
    // never (too costly).
    let dual = spec.stereo
        && match scanner::file_block(uri).map(|b| b.kind) {
            Some(FileKind::ImpulseResponse) => {
                stereo_in || file.is_some_and(|f| super::ir::ImpulseResponse::channel_count(f) > 1)
            }
            Some(_) => false,
            None => stereo_in && info.is_some_and(|info| info.audio_ports() == (1, 1)),
        };
    let outputs = info.map_or(1, |info| info.audio_ports().1);
    (dual, dual || stereo_in || (spec.stereo && outputs >= 2))
}

/// Mono or stereo signal around the blocks of a chain, as `EffectChain`
/// routes it
#[derive(Debug, Clone, Default)]
pub struct SignalMap {
    /// Stereo-ness of each plugin and split
    blocks: HashMap<usize, Signal>,
    output: bool,
}

/// True for a stereo signal
#[derive(Debug, Clone, Copy, Default)]
struct Signal {
    input: bool,
    output: bool,
    /// Split: signal entering its branches
    branches: bool,
}

impl SignalMap {
    pub fn new(spec: &ChainSpec, host: &Lv2Host) -> Self {
        let mut map = Self::default();
        map.output = map.walk(&spec.nodes, false, spec, host);
        map
    }

    fn walk(
        &mut self,
        nodes: &[ChainNode],
        mut stereo: bool,
        spec: &ChainSpec,
        host: &Lv2Host,
    ) -> bool {
        for node in nodes {
            let (id, out, branches) = match node {
                ChainNode::Plugin {
                    instance_id, uri, ..
                } => {
                    if host.scanner().find_by_uri(uri).is_none() {
                        continue; // skipped by the chain
                    }
                    let out = plugin_routing(spec, host, uri, *instance_id, stereo).1;
                    (*instance_id, out, false)
                }
                ChainNode::Split {
                    instance_id,
                    split_type,
                    top_chain,
                    bottom_chain,
                    pan,
                    ..
                } => {
                    let sides = spec.stereo && *split_type == SplitType::Lr;
                    let branch_in = stereo && !sides;
                    let top = self.walk(top_chain, branch_in, spec, host);
                    let bottom = self.walk(bottom_chain, branch_in, spec, host);
                    let panned = spec.stereo && is_panned(*split_type, *pan);
                    let out = sides || panned || stereo || top || bottom;
                    (*instance_id, out, branch_in)
                }
            };
            self.set(id, stereo, out, branches);
            stereo = out;
        }
        stereo
    }

    /// Record the signal around block `id` (`branches`: entering the
    /// branches of a split)
    pub fn set(&mut self, id: usize, input: bool, output: bool, branches: bool) {
        self.blocks.insert(
            id,
            Signal {
                input,
                output,
                branches,
            },
        );
    }

    /// True if the signal entering block `id` is stereo
    pub fn input(&self, id: usize) -> bool {
        self.blocks.get(&id).is_some_and(|b| b.input)
    }

    /// True if the signal leaving block `id` is stereo
    pub fn output(&self, id: usize) -> bool {
        self.blocks.get(&id).is_some_and(|b| b.output)
    }

    /// True if the signal entering the branches of split `id` is stereo
    pub fn branches(&self, id: usize) -> bool {
        self.blocks.get(&id).is_some_and(|b| b.branches)
    }

    pub fn set_chain_output(&mut self, stereo: bool) {
        self.output = stereo;
    }

    /// True if the chain output is stereo
    pub fn chain_output(&self) -> bool {
        self.output
    }
}

/// A plugin, with a second instance for the right side when a mono block
/// processes a stereo signal (dual mono)
struct PluginProc {
    left: Box<PluginInstance>,
    right: Option<Box<PluginInstance>>,
    stereo_in: bool,
    stereo_out: bool,
    /// Nanoseconds spent processing (see `BlockMeter`)
    spent: Arc<AtomicU64>,
    /// Records the sound in and out while the block's view is shown
    tap: &'static BlockTap,
}

impl PluginProc {
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let start = Instant::now();
        if self.tap.watches(self.left.instance_id) {
            self.tap.record_input(l, r, self.stereo_in);
            self.run(l, r);
            self.tap.record_output(l, r, self.stereo_out);
            self.tap.record_controls(self.left.controls());
        } else {
            self.run(l, r);
        }
        self.spent
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        match &mut self.right {
            Some(right) => {
                if !self.stereo_in {
                    r.copy_from_slice(l);
                }
                self.left.process_in_place(l);
                right.process_in_place(r);
            }
            None => self
                .left
                .process(l, self.stereo_out.then_some(r), self.stereo_in),
        }
    }

    fn instances(&mut self) -> impl Iterator<Item = &mut PluginInstance> {
        std::iter::once(&mut *self.left).chain(self.right.as_deref_mut())
    }
}

struct SplitProc {
    instance_id: usize,
    split_type: SplitType,
    enabled: bool,
    select_a: bool,
    mix: f32,
    /// Position of the top and bottom branches, and its ramped value
    pan: [f32; 2],
    pan_now: [f32; 2],
    /// Current weight of the top branch, ramped towards `target()`
    top_gain: f32,
    step: f32,
    top: Vec<ProcNode>,
    bottom: Vec<ProcNode>,
    /// Stereo-ness of the input, of each branch output and of the output
    stereo_in: bool,
    top_stereo: bool,
    bottom_stereo: bool,
    stereo_out: bool,
    /// Branch buffers (left, right)
    top_buf: [Box<[f32]>; 2],
    bottom_buf: [Box<[f32]>; 2],
    /// The branch that may run on another core (see `Offload`)
    offload: Offload,
}

/// Which branch of a split runs on another core while the audio thread runs
/// the other one: one with plugins and without sends (the send buses are
/// the audio thread's), when the other branch has plugins too
#[derive(Debug, Clone, Copy, PartialEq)]
enum Offload {
    None,
    Top,
    Bottom,
}

impl Offload {
    fn choose(top: &[ProcNode], bottom: &[ProcNode]) -> Self {
        if !has_plugin(top) || !has_plugin(bottom) {
            Offload::None
        } else if !has_send(bottom) {
            Offload::Bottom
        } else if !has_send(top) {
            Offload::Top
        } else {
            Offload::None
        }
    }
}

fn has_plugin(nodes: &[ProcNode]) -> bool {
    nodes.iter().any(|node| match node {
        ProcNode::Plugin(_) => true,
        ProcNode::Split(split) => has_plugin(&split.top) || has_plugin(&split.bottom),
        ProcNode::Route(_) => false,
    })
}

fn has_send(nodes: &[ProcNode]) -> bool {
    nodes.iter().any(|node| match node {
        ProcNode::Plugin(_) => false,
        ProcNode::Split(split) => has_send(&split.top) || has_send(&split.bottom),
        ProcNode::Route(route) => route.send,
    })
}

/// Process two branches, at the same time when one may go to another core
#[allow(clippy::too_many_arguments)]
fn process_branches(
    offload: Offload,
    io: &mut ChainIo,
    top: &mut [ProcNode],
    [tl, tr]: [&mut [f32]; 2],
    bottom: &mut [ProcNode],
    [bl, br]: [&mut [f32]; 2],
) {
    let mut other = io.without_sends();
    match offload {
        Offload::Bottom => {
            dsp_pool::join(
                || process_nodes(top, tl, tr, io),
                || process_nodes(bottom, bl, br, &mut other),
            );
        }
        Offload::Top => {
            dsp_pool::join(
                || process_nodes(bottom, bl, br, io),
                || process_nodes(top, tl, tr, &mut other),
            );
        }
        Offload::None => {
            process_nodes(top, tl, tr, io);
            process_nodes(bottom, bl, br, io);
        }
    }
}

impl SplitProc {
    fn target(&self) -> f32 {
        match self.split_type {
            SplitType::Ab => {
                if self.select_a {
                    1.0
                } else {
                    0.0
                }
            }
            SplitType::Mix => self.mix,
            // In mono mode, L/R branches are summed equally
            SplitType::Lr => 0.5,
        }
    }

    /// Pan applied to the branches: only a Mix split with a stereo output
    /// places them
    fn pan_target(&self) -> [f32; 2] {
        if self.split_type == SplitType::Mix && self.stereo_out {
            self.pan
        } else {
            [0.0; 2]
        }
    }

    /// True when the branches go to separate sides
    fn sides(&self) -> bool {
        self.split_type == SplitType::Lr && self.stereo_out
    }

    /// `l.len()` must not exceed the branch buffer size
    fn process(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
        if !self.enabled {
            if self.stereo_out && !self.stereo_in {
                r.copy_from_slice(l);
            }
            return;
        }
        let n = l.len();
        if self.sides() {
            // Top branch to the left, bottom branch to the right: each
            // branch gets its side of the input, and is mixed to mono
            let [bl, br] = &mut self.bottom_buf;
            let (bl, br) = (&mut bl[..n], &mut br[..n]);
            bl.copy_from_slice(if self.stereo_in { r } else { l });
            let tr = &mut self.top_buf[1][..n];
            process_branches(
                self.offload,
                io,
                &mut self.top,
                [&mut *l, &mut *tr],
                &mut self.bottom,
                [&mut *bl, &mut *br],
            );
            if self.top_stereo {
                for (l, tr) in l.iter_mut().zip(tr.iter()) {
                    *l = (*l + tr) * 0.5;
                }
            }
            for (i, r) in r.iter_mut().enumerate() {
                *r = if self.bottom_stereo {
                    (bl[i] + br[i]) * 0.5
                } else {
                    bl[i]
                };
            }
            return;
        }

        let target = self.target();
        let pan_target = self.pan_target();
        if self.top_gain == target && (target == 1.0 || target == 0.0) {
            // Steady A/B state: only the selected branch is processed
            let (branch, branch_stereo, side) = if target == 1.0 {
                (&mut self.top, self.top_stereo, 0)
            } else {
                (&mut self.bottom, self.bottom_stereo, 1)
            };
            process_nodes(branch, l, r, io);
            if self.stereo_out && !branch_stereo {
                r.copy_from_slice(l);
            }
            if self.pan_now != pan_target || pan_target[side] != 0.0 {
                for (l, r) in l.iter_mut().zip(r.iter_mut()) {
                    for (now, target) in self.pan_now.iter_mut().zip(pan_target) {
                        ramp(now, target, self.step);
                    }
                    let (gl, gr) = pan_gains(self.pan_now[side]);
                    *l *= gl;
                    *r *= gr;
                }
            }
            return;
        }

        for [bl, br] in [&mut self.top_buf, &mut self.bottom_buf] {
            bl[..n].copy_from_slice(l);
            if self.stereo_in {
                br[..n].copy_from_slice(r);
            }
        }
        {
            let [tl, tr] = &mut self.top_buf;
            let [bl, br] = &mut self.bottom_buf;
            process_branches(
                self.offload,
                io,
                &mut self.top,
                [&mut tl[..n], &mut tr[..n]],
                &mut self.bottom,
                [&mut bl[..n], &mut br[..n]],
            );
        }

        let ([tl, tr], [bl, br]) = (&self.top_buf, &self.bottom_buf);
        for i in 0..n {
            ramp(&mut self.top_gain, target, self.step);
            for (now, target) in self.pan_now.iter_mut().zip(pan_target) {
                ramp(now, target, self.step);
            }
            let g = self.top_gain;
            let (top_l, top_r) = pan_gains(self.pan_now[0]);
            let (bottom_l, bottom_r) = pan_gains(self.pan_now[1]);
            l[i] = tl[i] * top_l * g + bl[i] * bottom_l * (1.0 - g);
            if self.stereo_out {
                let t = if self.top_stereo { tr[i] } else { tl[i] };
                let b = if self.bottom_stereo { br[i] } else { bl[i] };
                r[i] = t * top_r * g + b * bottom_r * (1.0 - g);
            }
        }
    }
}

/// A send or return block
struct RouteProc {
    instance_id: usize,
    send: bool,
    /// Send bus or input (0 = the first)
    port: usize,
    /// Port chosen while the old one fades out
    next_port: Option<usize>,
    enabled: bool,
    /// Level (linear) and return mix (0..1), with their ramped values
    level: f32,
    mix: f32,
    level_now: f32,
    mix_now: f32,
    step: f32,
    stereo: bool,
}

impl RouteProc {
    fn new(
        instance_id: usize,
        send: bool,
        values: &[f32],
        enabled: bool,
        stereo: bool,
        sample_rate: f32,
    ) -> Self {
        let mut route = Self {
            instance_id,
            send,
            port: 0,
            next_port: None,
            enabled,
            level: 1.0,
            mix: 1.0,
            level_now: 0.0,
            mix_now: 0.0,
            step: 1.0 / (sample_rate * SPLIT_RAMP_SECONDS).max(1.0),
            stereo,
        };
        for (index, value) in values.iter().enumerate() {
            route.set_parameter(index, *value);
        }
        if let Some(port) = route.next_port.take() {
            route.port = port;
        }
        (route.level_now, route.mix_now) = route.targets();
        route
    }

    fn set_parameter(&mut self, index: usize, value: f32) {
        use scanner::{ret, send};
        if !value.is_finite() {
            return;
        }
        match (self.send, index) {
            (true, send::BUS) | (false, ret::INPUT) => {
                let port = value.round().max(0.0) as usize;
                self.next_port = (port != self.port).then_some(port);
            }
            (true, send::LEVEL) | (false, ret::LEVEL) => self.level = 10f32.powf(value / 20.0),
            (false, ret::MIX) => self.mix = (value / 100.0).clamp(0.0, 1.0),
            _ => {}
        }
    }

    /// Ramp targets of the level and of the return mix: a bypassed send
    /// stops adding to its bus, a bypassed return lets the signal through,
    /// and both fade out before changing port
    fn targets(&self) -> (f32, f32) {
        let on = self.enabled && self.next_port.is_none();
        let mix = if on { self.mix } else { 0.0 };
        let level = if self.send && !on { 0.0 } else { self.level };
        (level, mix)
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
        let range = io.offset..io.offset + l.len();
        let (level, mix) = self.targets();
        if self.send {
            let mut bus = io
                .sends
                .get_mut(self.port)
                .and_then(|[sl, sr]| Some((sl.get_mut(range.clone())?, sr.get_mut(range)?)));
            let right: &[f32] = if self.stereo { r } else { l };
            for i in 0..l.len() {
                ramp(&mut self.level_now, level, self.step);
                if let Some((sl, sr)) = &mut bus {
                    let g = self.level_now * (io.gain + io.gain_step * i as f32);
                    sl[i] += l[i] * g;
                    sr[i] += right[i] * g;
                }
            }
        } else {
            // An input the interface lacks (a preset made on a bigger one,
            // or the tuner muting) plays silence
            let input = io.inputs.get(self.port).and_then(|b| b.get(range));
            for i in 0..l.len() {
                ramp(&mut self.level_now, level, self.step);
                ramp(&mut self.mix_now, mix, self.step);
                let wet = input.map_or(0.0, |b| b[i]) * self.level_now * self.mix_now;
                let dry = 1.0 - self.mix_now;
                l[i] = l[i] * dry + wet;
                if self.stereo {
                    r[i] = r[i] * dry + wet;
                }
            }
        }
        // Faded out: switch to the new port, then fade in
        let out = if self.send {
            self.level_now
        } else {
            self.mix_now
        };
        if out == 0.0 {
            if let Some(port) = self.next_port.take() {
                self.port = port;
            }
        }
    }
}

enum ProcNode {
    Plugin(Box<PluginProc>),
    Split(Box<SplitProc>),
    Route(Box<RouteProc>),
}

/// Process `l` (and `r` where the signal is stereo) through `nodes`
fn process_nodes(nodes: &mut [ProcNode], l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
    for node in nodes {
        node.process(l, r, io);
    }
}

impl ProcNode {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
        match self {
            ProcNode::Plugin(plugin) => plugin.process(l, r),
            ProcNode::Split(split) => split.process(l, r, io),
            ProcNode::Route(route) => route.process(l, r, io),
        }
    }
}

/// Sub-blocks a period is cut into when the chain runs on several cores
const WAVE_PARTS: usize = 8;
/// Smallest sub-block, in samples
const WAVE_MIN: usize = 16;
/// The chain goes on one more core for each share of the period it takes
/// (f32 bits; see `set_stage_load`)
static STAGE_LOAD: AtomicU32 = AtomicU32::new(0x3e80_0000); // 0.25

/// Share of the period a chain takes before it goes on one more core: low
/// spreads it early (a shorter callback, more cores busy), high keeps it
/// on fewer cores
pub fn set_stage_load(share: f32) {
    STAGE_LOAD.store(share.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}
/// Most cores a chain ran on since `take_peak_cores`
static PEAK_CORES: AtomicUsize = AtomicUsize::new(1);

/// Most cores a chain ran on since the last call (the resource monitor)
pub fn take_peak_cores() -> usize {
    PEAK_CORES.swap(1, Ordering::Relaxed)
}

/// Weight of the last period in the time a block takes (moving average)
const COST_WEIGHT: f32 = 0.1;

/// Cut `costs` into `stages` runs of nodes taking about the same time:
/// stage k runs the nodes `bounds[k]..bounds[k + 1]`. `stages` must not
/// exceed the number of nodes
fn partition(costs: &[f32], stages: usize) -> [usize; dsp_pool::MAX_WORKERS + 2] {
    let mut bounds = [0; dsp_pool::MAX_WORKERS + 2];
    let len = costs.len();
    let stages = stages.clamp(1, len.clamp(1, dsp_pool::MAX_WORKERS + 1));
    let target = costs.iter().sum::<f32>() / stages as f32;
    let mut k = 1;
    let mut done = 0.0;
    for (i, cost) in costs.iter().enumerate() {
        // Cut before the node that would end past its middle the share of
        // the stage, leaving a node for each stage after
        if k < stages
            && i > bounds[k - 1]
            && done + cost / 2.0 > target * k as f32
            && len - i >= stages - k
        {
            bounds[k] = i;
            k += 1;
        }
        done += cost;
    }
    while k < stages {
        bounds[k] = (bounds[k - 1] + 1).max(len.saturating_sub(stages - k));
        k += 1;
    }
    bounds[stages] = len;
    bounds
}

/// Most sub-blocks of a pipelined period (`WAVE_PARTS`, one more when the
/// period does not divide evenly)
const WAVE_BLOCKS: usize = WAVE_PARTS + 1;

/// Nodes of one stage, with their times
type StageNodes<'a> = Option<(&'a mut [ProcNode], &'a mut [f32])>;
/// One sub-block of the period: left and right
type SubBlock<'a> = Option<(&'a mut [f32], &'a mut [f32])>;

/// What the stages of a pipelined period share. Each stage owns its nodes,
/// and a sub-block is used by one stage at a time (the progress of the
/// stage before says when): the locks are never contended, each taken
/// with a single atomic operation, and nothing is allocated.
struct Wave<'a> {
    stages: [Mutex<StageNodes<'a>>; dsp_pool::MAX_WORKERS + 1],
    blocks: [Mutex<SubBlock<'a>>; WAVE_BLOCKS],
    part: usize,
    inputs: &'a [&'a [f32]],
    offset: usize,
    /// Sub-blocks each stage has finished
    progress: [AtomicUsize; dsp_pool::MAX_WORKERS + 1],
}

impl<'a> Wave<'a> {
    /// Cut `nodes` (and `spent`) at `bounds` into stages, and `l` / `r`
    /// into sub-blocks of `part` samples
    fn new(
        mut nodes: &'a mut [ProcNode],
        mut spent: &'a mut [f32],
        bounds: &[usize],
        (l, r): (&'a mut [f32], &'a mut [f32]),
        part: usize,
        (inputs, offset): (&'a [&'a [f32]], usize),
    ) -> Self {
        let stages = std::array::from_fn(|_| Mutex::new(None));
        let blocks = std::array::from_fn(|_| Mutex::new(None));
        let wave = Self {
            stages,
            blocks,
            part,
            inputs,
            offset,
            progress: Default::default(),
        };
        for (slot, ends) in wave.stages.iter().zip(bounds.windows(2)) {
            let count = ends[1].saturating_sub(ends[0]);
            let all_nodes = std::mem::take(&mut nodes);
            let (stage_nodes, rest_nodes) = all_nodes.split_at_mut(count.min(all_nodes.len()));
            let all_spent = std::mem::take(&mut spent);
            let (stage_spent, rest_spent) = all_spent.split_at_mut(count.min(all_spent.len()));
            (nodes, spent) = (rest_nodes, rest_spent);
            *slot.locked() = Some((stage_nodes, stage_spent));
        }
        for (slot, block) in wave
            .blocks
            .iter()
            .zip(l.chunks_mut(part.max(1)).zip(r.chunks_mut(part.max(1))))
        {
            *slot.locked() = Some(block);
        }
        wave
    }

    /// Run stage `k` over every sub-block, each once the stage before is
    /// done with it
    fn stage(&self, k: usize) {
        // However the stage ends (a plugin may panic), the stages after it
        // must not wait for it forever
        struct Finish<'a>(Option<&'a AtomicUsize>);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                if let Some(progress) = self.0 {
                    progress.store(WAVE_BLOCKS, Ordering::Release);
                }
            }
        }
        let _finish = Finish(self.progress.get(k));
        let Some(stage) = self.stages.get(k) else {
            return;
        };
        let mut stage = stage.locked();
        let Some((nodes, spent)) = stage.as_mut() else {
            return;
        };
        for (j, block) in self.blocks.iter().enumerate() {
            if let Some(before) = k.checked_sub(1).and_then(|k| self.progress.get(k)) {
                dsp_pool::spin_until(|| before.load(Ordering::Acquire) > j);
            }
            // The stage before is done with this sub-block (its progress,
            // Acquire) and the stage after waits for ours
            if let Some((l, r)) = block.locked().as_mut() {
                let mut io = ChainIo {
                    inputs: self.inputs,
                    sends: &mut [],
                    offset: self.offset + j * self.part,
                    gain: 1.0,
                    gain_step: 0.0,
                };
                for (node, spent) in nodes.iter_mut().zip(spent.iter_mut()) {
                    let start = Instant::now();
                    node.process(l, r, &mut io);
                    *spent += start.elapsed().as_secs_f32();
                }
            }
            if let Some(progress) = self.progress.get(k) {
                progress.store(j + 1, Ordering::Release);
            }
        }
    }
}

fn for_each_instance(nodes: &mut [ProcNode], f: &mut impl FnMut(&mut PluginInstance)) {
    for node in nodes {
        match node {
            ProcNode::Plugin(plugin) => plugin.instances().for_each(&mut *f),
            ProcNode::Split(split) => {
                for_each_instance(&mut split.top, f);
                for_each_instance(&mut split.bottom, f);
            }
            ProcNode::Route(_) => {}
        }
    }
}

/// Apply a command to the node it targets; returns true if found
fn apply_to(nodes: &mut [ProcNode], command: &ChainCommand) -> bool {
    let Some(target) = command.target() else {
        return false;
    };
    for node in nodes {
        match node {
            ProcNode::Plugin(plugin) if plugin.left.instance_id == target => {
                for instance in plugin.instances() {
                    match *command {
                        ChainCommand::SetParameter { index, value, .. } => {
                            instance.set_parameter(index, value)
                        }
                        ChainCommand::SetEnabled { enabled, .. } => instance.set_bypass(!enabled),
                        _ => {}
                    }
                }
                return true;
            }
            ProcNode::Route(route) if route.instance_id == target => {
                match *command {
                    ChainCommand::SetParameter { index, value, .. } => {
                        route.set_parameter(index, value)
                    }
                    ChainCommand::SetEnabled { enabled, .. } => route.enabled = enabled,
                    _ => {}
                }
                return true;
            }
            ProcNode::Split(split) => {
                if split.instance_id == target {
                    match *command {
                        ChainCommand::SetEnabled { enabled, .. } => split.enabled = enabled,
                        ChainCommand::SetSplitAb { select_a, .. } => split.select_a = select_a,
                        ChainCommand::SetSplitMix { ratio, .. } => {
                            split.mix = ratio.clamp(0.0, 1.0)
                        }
                        ChainCommand::SetSplitPan { pan, .. } => {
                            split.pan = pan.map(|p| p.clamp(-1.0, 1.0))
                        }
                        ChainCommand::SetParameter { .. } | ChainCommand::SetTempo { .. } => {}
                    }
                    return true;
                }
                if apply_to(&mut split.top, command) || apply_to(&mut split.bottom, command) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Effect chain processed by the audio thread
pub struct EffectChain {
    nodes: Vec<ProcNode>,
    max_block: usize,
    plugin_count: usize,
    /// The output is stereo (else the right side copies the left one)
    stereo_out: bool,
    sample_rate: f32,
    /// Time each node takes per period, in seconds (moving average), and
    /// the time it took in the period being processed
    costs: Vec<f32>,
    spent: Vec<f32>,
    /// A send somewhere: the chain stays on the audio thread, which owns
    /// the send buses
    has_send: bool,
}

impl EffectChain {
    /// Empty chain (passthrough)
    pub fn empty(max_block: usize) -> Self {
        Self {
            nodes: Vec::new(),
            max_block: max_block.max(1),
            plugin_count: 0,
            stereo_out: false,
            sample_rate: 48000.0,
            costs: Vec::new(),
            spent: Vec::new(),
            has_send: false,
        }
    }

    /// Instantiate every plugin of `spec`. Plugins that fail to load are
    /// skipped (and reported) so the rest of the chain still works.
    pub fn build(
        spec: &ChainSpec,
        host: &Lv2Host,
        sample_rate: f32,
        max_block: usize,
    ) -> (Self, Vec<String>) {
        let mut chain = Self::empty(max_block);
        let mut errors = Vec::new();
        BlockTap::global().set_rate(sample_rate);
        let (nodes, stereo_out) =
            chain.build_nodes(&spec.nodes, false, spec, host, sample_rate, &mut errors);
        chain.nodes = nodes;
        chain.stereo_out = stereo_out;
        chain.sample_rate = sample_rate;
        chain.costs = vec![0.0; chain.nodes.len()];
        chain.spent = vec![0.0; chain.nodes.len()];
        chain.has_send = has_send(&chain.nodes);
        if spec.bpm > 0.0 {
            chain.apply(&ChainCommand::SetTempo { bpm: spec.bpm });
        }
        (chain, errors)
    }

    /// Build `nodes` for an input that is stereo or not; returns them with
    /// the stereo-ness of their output
    fn build_nodes(
        &mut self,
        nodes: &[ChainNode],
        mut stereo: bool,
        spec: &ChainSpec,
        host: &Lv2Host,
        sample_rate: f32,
        errors: &mut Vec<String>,
    ) -> (Vec<ProcNode>, bool) {
        let mut built = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node {
                ChainNode::Plugin {
                    instance_id,
                    uri,
                    enabled,
                    ..
                } if scanner::is_routing(uri) => {
                    let values = spec.params.get(instance_id).map_or(&[][..], Vec::as_slice);
                    let send = uri == scanner::SEND_URI;
                    let route =
                        RouteProc::new(*instance_id, send, values, *enabled, stereo, sample_rate);
                    self.plugin_count += 1;
                    built.push(ProcNode::Route(Box::new(route)));
                }
                ChainNode::Plugin {
                    instance_id,
                    uri,
                    enabled,
                    ..
                } => {
                    let file = spec.files.get(instance_id).map(PathBuf::as_path);
                    let kind = scanner::file_block(uri).map(|b| b.kind);
                    let instantiate = |channel| {
                        host.instantiate_channel(
                            uri,
                            *instance_id,
                            sample_rate,
                            self.max_block,
                            file,
                            channel,
                        )
                        .map(|mut plugin| {
                            if let Some(values) = spec.params.get(instance_id) {
                                for (index, value) in values.iter().enumerate() {
                                    plugin.set_parameter(index, *value);
                                }
                            }
                            if let Some(files) = spec.param_files.get(instance_id) {
                                plugin.load_files(files, FILE_LOAD_TIMEOUT);
                            }
                            plugin.set_bypass_immediate(!enabled);
                            Box::new(plugin)
                        })
                    };
                    let (dual, _) = plugin_routing(spec, host, uri, *instance_id, stereo);
                    let ir = kind == Some(FileKind::ImpulseResponse);
                    let left = instantiate((dual && ir).then_some(0));
                    match left {
                        Ok(left) => {
                            let right = if dual {
                                match instantiate(ir.then_some(1)) {
                                    Ok(right) => Some(right),
                                    Err(e) => {
                                        warn!(
                                            uri = %uri,
                                            error = format_args!("{e:#}"),
                                            "No right-side instance, the block plays in mono"
                                        );
                                        None
                                    }
                                }
                            } else {
                                None
                            };
                            let stereo_out = right.is_some()
                                || stereo
                                || (spec.stereo && left.audio_ports().1 >= 2);
                            self.plugin_count += 1;
                            built.push(ProcNode::Plugin(Box::new(PluginProc {
                                left,
                                right,
                                stereo_in: stereo,
                                stereo_out,
                                spent: BlockMeter::global().counter(*instance_id),
                                tap: BlockTap::global(),
                            })));
                            stereo = stereo_out;
                        }
                        Err(e) => {
                            warn!(
                                uri = %uri,
                                error = format_args!("{e:#}"),
                                "Plugin skipped from the chain"
                            );
                            errors.push(format!("{}: {}", uri, e));
                        }
                    }
                }
                ChainNode::Split {
                    instance_id,
                    split_type,
                    enabled,
                    top_chain,
                    bottom_chain,
                    ab_select_a,
                    mix_ratio,
                    pan,
                    ..
                } => {
                    // Each side of an L/R split gets a mono signal
                    let sides = spec.stereo && *split_type == SplitType::Lr;
                    let branch_in = stereo && !sides;
                    let (top, top_stereo) =
                        self.build_nodes(top_chain, branch_in, spec, host, sample_rate, errors);
                    let (bottom, bottom_stereo) =
                        self.build_nodes(bottom_chain, branch_in, spec, host, sample_rate, errors);
                    let panned = spec.stereo && is_panned(*split_type, *pan);
                    let stereo_out = sides || panned || stereo || top_stereo || bottom_stereo;
                    let buffer = || vec![0.0; self.max_block].into_boxed_slice();
                    let mut split = SplitProc {
                        instance_id: *instance_id,
                        split_type: *split_type,
                        enabled: *enabled,
                        select_a: *ab_select_a,
                        mix: mix_ratio.clamp(0.0, 1.0),
                        pan: pan.map(|p| p.clamp(-1.0, 1.0)),
                        pan_now: [0.0; 2],
                        top_gain: 0.0,
                        step: 1.0 / (sample_rate * SPLIT_RAMP_SECONDS).max(1.0),
                        top,
                        bottom,
                        stereo_in: stereo,
                        top_stereo,
                        bottom_stereo,
                        stereo_out,
                        top_buf: [buffer(), buffer()],
                        bottom_buf: [buffer(), buffer()],
                        offload: Offload::None,
                    };
                    split.offload = Offload::choose(&split.top, &split.bottom);
                    split.top_gain = split.target();
                    split.pan_now = split.pan_target();
                    built.push(ProcNode::Split(Box::new(split)));
                    stereo = stereo_out;
                }
            }
        }
        (built, stereo)
    }

    /// Process the mono input in `left` (any length): the left and right
    /// outputs are written in place to `left` and `right` (same length)
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        no_io(|io| {
            for (k, (l, r)) in left
                .chunks_mut(self.max_block)
                .zip(right.chunks_mut(self.max_block))
                .enumerate()
            {
                io.part(k * self.max_block, l.len(), 1.0, 1.0);
                self.process_io(l, r, io);
            }
        })
    }

    /// Process one part of a period (at most `max_block` samples, at the
    /// offset set in `io`), with the inputs and sends of `io`
    pub fn process_io(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
        let len = l.len().min(r.len());
        let (l, r) = (&mut l[..len], &mut r[..len]);
        if len > self.max_block {
            // Longer than the chain's buffers: in parts that fit them
            let (offset, gain) = (io.offset, io.gain);
            for (k, (l, r)) in l
                .chunks_mut(self.max_block)
                .zip(r.chunks_mut(self.max_block))
                .enumerate()
            {
                let start = k * self.max_block;
                io.offset = offset + start;
                io.gain = gain + io.gain_step * start as f32;
                self.process_part(l, r, io);
            }
            (io.offset, io.gain) = (offset, gain);
            return;
        }
        self.process_part(l, r, io);
    }

    /// `process_io` on at most `max_block` samples, `l` and `r` as long
    fn process_part(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo) {
        let stages = self.stages(l.len());
        let crew = (stages > 1).then(|| dsp_pool::crew(stages - 1));
        match crew {
            Some(crew) if !crew.is_empty() => {
                PEAK_CORES.fetch_max(crew.len() + 1, Ordering::Relaxed);
                self.process_wave(l, r, io, crew)
            }
            _ => {
                for ((node, cost), spent) in self
                    .nodes
                    .iter_mut()
                    .zip(&mut self.costs)
                    .zip(&mut self.spent)
                {
                    let start = Instant::now();
                    node.process(l, r, io);
                    *spent = start.elapsed().as_secs_f32();
                    *cost += (*spent - *cost) * COST_WEIGHT;
                    *spent = 0.0;
                }
            }
        }
        if !self.stereo_out {
            r.copy_from_slice(l);
        }
    }

    /// The chain feeds a send bus: it must run on the audio thread
    pub fn has_send(&self) -> bool {
        self.has_send
    }

    /// Cores to run a period of `len` samples on: one more for each
    /// `set_stage_load` share of the period the chain takes, as many as there are
    /// workers and nodes
    fn stages(&self, len: usize) -> usize {
        let workers = dsp_pool::workers();
        if workers == 0 || self.has_send || len < 2 * WAVE_MIN || self.nodes.len() < 2 {
            return 1;
        }
        let load = self.costs.iter().sum::<f32>() * self.sample_rate / len as f32;
        let share = f32::from_bits(STAGE_LOAD.load(Ordering::Relaxed)).max(1e-3);
        ((load / share).ceil() as usize).clamp(1, (workers + 1).min(self.nodes.len()))
    }

    /// Run the nodes as a pipeline over the cores of `crew` and this one:
    /// stage k runs its share of the nodes on each sub-block of the period
    /// once stage k - 1 is done with it, so the stages work at the same
    /// time on successive sub-blocks, and the period ends with no added
    /// latency
    fn process_wave(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        io: &mut ChainIo,
        crew: dsp_pool::Crew,
    ) {
        let len = l.len();
        let stages = crew.len() + 1;
        let part = (len / WAVE_PARTS).max(WAVE_MIN);
        let bounds = partition(&self.costs, stages);
        let wave = Wave::new(
            &mut self.nodes,
            &mut self.spent,
            &bounds[..=stages.min(bounds.len() - 1)],
            (l, r),
            part,
            (io.inputs, io.offset),
        );
        crew.run(&|k| wave.stage(k));

        for (cost, spent) in self.costs.iter_mut().zip(&mut self.spent) {
            *cost += (*spent - *cost) * COST_WEIGHT;
            *spent = 0.0;
        }
    }

    /// True if the two outputs can differ
    pub fn is_stereo(&self) -> bool {
        self.stereo_out
    }

    /// Apply a live change; returns false if no node has that instance_id
    pub fn apply(&mut self, command: &ChainCommand) -> bool {
        if let ChainCommand::SetTempo { bpm } = *command {
            for_each_instance(&mut self.nodes, &mut |i| i.set_tempo(bpm));
            return true;
        }
        apply_to(&mut self.nodes, command)
    }

    /// Hand a MIDI message to every plugin with a MIDI input
    pub fn send_midi(&mut self, message: RawMidi) {
        for_each_instance(&mut self.nodes, &mut |i| i.send_midi(message));
    }

    /// Number of instantiated plugins
    pub fn len(&self) -> usize {
        self.plugin_count
    }

    pub fn is_empty(&self) -> bool {
        self.plugin_count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::routing::MAX_SENDS;

    fn plugin(id: usize) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: "urn:pedalboard:demo:delay".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        }
    }

    fn split(id: usize, split_type: SplitType) -> ChainNode {
        ChainNode::Split {
            instance_id: id,
            split_type,
            enabled: true,
            top_chain: vec![plugin(id + 1)],
            bottom_chain: vec![],
            ab_select_a: true,
            mix_ratio: 0.5,
            pan: [0.0, 0.0],
            num_inputs: 2,
            num_outputs: 2,
        }
    }

    /// Left and right outputs for a constant mono input
    fn run(chain: &mut EffectChain, value: f32) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![value; 300], vec![0.0; 300]);
        chain.process(&mut left, &mut right);
        (left, right)
    }

    /// True once gains have ramped (after the first block)
    fn settles_at(buffer: &[f32], value: f32) -> bool {
        buffer[64..].iter().all(|s| (s - value).abs() < 1e-5)
    }

    #[test]
    fn empty_chain_is_passthrough() {
        let mut chain = EffectChain::empty(64);
        let (left, right) = run(&mut chain, 0.5);
        assert!(settles_at(&left, 0.5) && settles_at(&right, 0.5));
    }

    /// Cabinet block without a file: plays its input at its level
    fn cab(id: usize) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: scanner::IR_URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    fn lr_split(top: Vec<ChainNode>, then: Vec<ChainNode>, stereo: bool) -> ChainSpec {
        split_spec(SplitType::Lr, 0.5, [0.0; 2], top, then, stereo)
    }

    /// Split with `top` on its top branch and an empty bottom branch
    fn split_spec(
        split_type: SplitType,
        mix_ratio: f32,
        pan: [f32; 2],
        top: Vec<ChainNode>,
        then: Vec<ChainNode>,
        stereo: bool,
    ) -> ChainSpec {
        let mut nodes = vec![ChainNode::Split {
            instance_id: 1,
            split_type,
            enabled: true,
            top_chain: top,
            bottom_chain: vec![],
            ab_select_a: true,
            mix_ratio,
            pan,
            num_inputs: 1,
            num_outputs: 2,
        }];
        nodes.extend(then);
        ChainSpec {
            nodes,
            // -6 dB on the cabinet of the top branch
            params: HashMap::from([(10, vec![20.0 * 0.5f32.log10()])]),
            stereo,
            ..Default::default()
        }
    }

    fn route(id: usize, uri: &str) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: uri.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    /// Run `chain` on input 1 at 1.0 and input 2 at 0.25; returns the
    /// outputs and the send buses
    fn run_io(chain: &mut EffectChain) -> (Vec<f32>, Vec<f32>, Vec<[Vec<f32>; 2]>) {
        let (guitar, bass) = (vec![1.0f32; 300], vec![0.25f32; 300]);
        let inputs: [&[f32]; 2] = [&guitar, &bass];
        let mut sends: Vec<[Vec<f32>; 2]> = (0..MAX_SENDS)
            .map(|_| [vec![0.0; 300], vec![0.0; 300]])
            .collect();
        let (mut left, mut right) = (guitar.clone(), vec![0.0; 300]);
        let mut io = ChainIo::new(&inputs, &mut sends);
        for (k, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            io.part(k * 64, l.len(), 1.0, 1.0);
            chain.process_io(l, r, &mut io);
        }
        (left, right, sends)
    }

    #[test]
    fn returns_replace_the_signal_and_sends_copy_it() {
        let host = Lv2Host::demo();
        // Cabinet 10 at -6 dB, sent to send 2 by block 11; input 2 brought
        // in by block 12 before cabinet 20
        let spec = ChainSpec {
            nodes: vec![
                cab(10),
                route(11, scanner::SEND_URI),
                route(12, scanner::RETURN_URI),
                cab(20),
            ],
            params: HashMap::from([
                (10, vec![20.0 * 0.5f32.log10()]),
                (11, vec![1.0, 0.0]),
                (12, vec![1.0, 0.0, 100.0]),
            ]),
            ..Default::default()
        };
        let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(errors.is_empty(), "{errors:?}");
        let (left, right, sends) = run_io(&mut chain);
        assert!(
            settles_at(&left, 0.25) && settles_at(&right, 0.25),
            "{}",
            left[299]
        );
        let [send_l, send_r] = &sends[1];
        assert!(
            settles_at(send_l, 0.5) && settles_at(send_r, 0.5),
            "{}",
            send_l[299]
        );
        assert!(sends[0].iter().flatten().all(|&s| s == 0.0));

        // Without the inputs (tuner muting), a return is silent
        let (mut left, mut right) = (vec![1.0; 300], vec![0.0; 300]);
        chain.process(&mut left, &mut right);
        assert!(settles_at(&left, 0.0));

        // Half mix blends the input in; a bypassed return lets the signal
        // through and a bypassed send stops sending
        chain.apply(&ChainCommand::SetParameter {
            instance_id: 12,
            index: scanner::ret::MIX,
            value: 50.0,
        });
        chain.apply(&ChainCommand::SetEnabled {
            instance_id: 11,
            enabled: false,
        });
        // 10 ms ramps: settled by the second run
        run_io(&mut chain);
        let (left, _, sends) = run_io(&mut chain);
        assert!(settles_at(&left, 0.375), "{}", left[299]);
        assert!(sends[1][0][299] == 0.0);
        chain.apply(&ChainCommand::SetEnabled {
            instance_id: 12,
            enabled: false,
        });
        run_io(&mut chain);
        let (left, _, _) = run_io(&mut chain);
        assert!(settles_at(&left, 0.5), "{}", left[299]);
    }

    #[test]
    fn a_send_fades_out_before_changing_bus() {
        let host = Lv2Host::demo();
        let spec = ChainSpec {
            nodes: vec![route(1, scanner::SEND_URI)],
            ..Default::default()
        };
        let (mut chain, _) = EffectChain::build(&spec, &host, 48000.0, 64);
        let (_, _, sends) = run_io(&mut chain);
        assert!(settles_at(&sends[0][0], 1.0));
        chain.apply(&ChainCommand::SetParameter {
            instance_id: 1,
            index: scanner::send::BUS,
            value: 2.0,
        });
        // 10 ms at 48 kHz: the old bus fades out over the first 480
        // samples, then the new one fades in
        let mut faded = Vec::new();
        for _ in 0..6 {
            let (_, _, sends) = run_io(&mut chain);
            faded.push((sends[0][0][299], sends[2][0][299]));
        }
        assert!(faded[0].0 < 1.0 && faded[0].1 == 0.0, "{faded:?}");
        assert_eq!(*faded.last().unwrap(), (0.0, 1.0));
        assert_eq!(MAX_SENDS, sends.len());
    }

    #[test]
    fn lr_split_sends_each_branch_to_its_side() {
        let host = Lv2Host::demo();
        let spec = lr_split(vec![cab(10)], vec![], true);
        let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(chain.is_stereo());
        let (left, right) = run(&mut chain, 1.0);
        assert!(settles_at(&left, 0.5), "{}", left[299]);
        assert!(settles_at(&right, 1.0), "{}", right[299]);

        // A block after it processes each side on its own
        let spec = lr_split(vec![cab(10)], vec![cab(20)], true);
        let (mut chain, _) = EffectChain::build(&spec, &host, 48000.0, 64);
        let (left, right) = run(&mut chain, 1.0);
        assert!(settles_at(&left, 0.5) && settles_at(&right, 1.0));

        // In mono mode both sides are summed
        let spec = lr_split(vec![cab(10)], vec![cab(20)], false);
        let (mut chain, _) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(!chain.is_stereo());
        let (left, right) = run(&mut chain, 1.0);
        assert!(settles_at(&left, 0.75) && settles_at(&right, 0.75));
    }

    /// Left and right outputs once ramps are over
    fn settled(chain: &mut EffectChain, value: f32) -> (f32, f32) {
        let (mut left, mut right) = (vec![value; 4096], vec![0.0; 4096]);
        chain.process(&mut left, &mut right);
        (left[4095], right[4095])
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    #[test]
    fn pan_gains_keep_the_centre_at_full_level() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
        assert_eq!(pan_gains(0.5), (0.5, 1.0));
        assert_eq!(pan_gains(-3.0), (1.0, 0.0));
    }

    #[test]
    fn mix_split_places_its_branches_with_pan() {
        let host = Lv2Host::demo();
        // Top branch at -6 dB, bottom branch dry, mixed half and half
        let mix = |pan, stereo| split_spec(SplitType::Mix, 0.5, pan, vec![cab(10)], vec![], stereo);

        // Centred: same mono mix as before panning existed
        let (mut chain, _) = EffectChain::build(&mix([0.0; 2], true), &host, 48000.0, 64);
        assert!(!chain.is_stereo());
        assert!(close(settled(&mut chain, 1.0), (0.75, 0.75)));

        // Hard left / hard right: each branch on its side
        let (mut chain, _) = EffectChain::build(&mix([-1.0, 1.0], true), &host, 48000.0, 64);
        assert!(chain.is_stereo());
        assert!(close(settled(&mut chain, 1.0), (0.25, 0.5)));

        // Moved live without a rebuild
        assert!(chain.apply(&ChainCommand::SetSplitPan {
            instance_id: 1,
            pan: [-0.5, 0.5],
        }));
        settled(&mut chain, 1.0);
        assert!(close(settled(&mut chain, 1.0), (0.5, 0.625)));

        // Ignored in mono mode
        let (mut chain, _) = EffectChain::build(&mix([-1.0, 1.0], false), &host, 48000.0, 64);
        assert!(!chain.is_stereo());
        assert!(close(settled(&mut chain, 1.0), (0.75, 0.75)));

        // Mix fully on one branch: only that branch runs, still panned
        let spec = split_spec(
            SplitType::Mix,
            1.0,
            [-1.0, 0.0],
            vec![cab(10)],
            vec![],
            true,
        );
        let (mut chain, _) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(close(settled(&mut chain, 1.0), (0.5, 0.0)));
    }

    #[test]
    fn signal_map_follows_the_routing() {
        let host = Lv2Host::demo();
        // Mono up to the L/R split, stereo after it
        let spec = lr_split(vec![cab(10)], vec![cab(20)], true);
        let map = SignalMap::new(&spec, &host);
        assert!(!map.input(1) && map.output(1) && !map.branches(1));
        assert!(!map.input(10) && !map.output(10));
        assert!(map.input(20) && map.output(20) && map.chain_output());

        // All mono in mono mode
        let map = SignalMap::new(&lr_split(vec![cab(10)], vec![cab(20)], false), &host);
        assert!(!map.output(1) && !map.output(20) && !map.chain_output());

        // A panned mix outputs stereo, a centred one mono
        let panned = split_spec(
            SplitType::Mix,
            0.5,
            [-1.0, 1.0],
            vec![cab(10)],
            vec![],
            true,
        );
        assert!(SignalMap::new(&panned, &host).output(1));
        let centred = split_spec(SplitType::Mix, 0.5, [0.0; 2], vec![cab(10)], vec![], true);
        assert!(!SignalMap::new(&centred, &host).chain_output());
    }

    #[test]
    fn bypassed_lr_split_plays_the_input_on_both_sides() {
        let host = Lv2Host::demo();
        let spec = lr_split(vec![cab(10)], vec![], true);
        let (mut chain, _) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(chain.apply(&ChainCommand::SetEnabled {
            instance_id: 1,
            enabled: false
        }));
        let (left, right) = run(&mut chain, 0.3);
        assert!(settles_at(&left, 0.3) && settles_at(&right, 0.3));
    }

    #[test]
    fn builds_nested_splits_and_applies_commands() {
        let host = Lv2Host::demo();
        let spec = ChainSpec {
            generation: 1,
            nodes: vec![
                plugin(0),
                split(1, SplitType::Ab),
                split(10, SplitType::Mix),
            ],
            params: HashMap::from([(0, vec![150.0, 0.5, 0.5])]),
            ..Default::default()
        };
        let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert!(errors.is_empty());
        assert_eq!(chain.len(), 3);

        // Passthrough demo plugins keep the signal intact, whatever the routing
        let (left, right) = run(&mut chain, 0.25);
        assert!(settles_at(&left, 0.25) && settles_at(&right, 0.25));

        assert!(chain.apply(&ChainCommand::SetSplitAb {
            instance_id: 1,
            select_a: false
        }));
        assert!(chain.apply(&ChainCommand::SetParameter {
            instance_id: 11,
            index: 0,
            value: 1.0
        }));
        assert!(chain.apply(&ChainCommand::SetEnabled {
            instance_id: 0,
            enabled: false
        }));
        assert!(!chain.apply(&ChainCommand::SetEnabled {
            instance_id: 99,
            enabled: false
        }));

        let (left, right) = run(&mut chain, 0.25);
        assert!(settles_at(&left, 0.25) && settles_at(&right, 0.25));
    }

    #[test]
    fn unknown_plugins_are_reported_not_fatal() {
        let host = Lv2Host::demo();
        let mut missing = plugin(0);
        if let ChainNode::Plugin { uri, .. } = &mut missing {
            *uri = "urn:missing".to_string();
        }
        let spec = ChainSpec {
            nodes: vec![missing, plugin(1)],
            ..Default::default()
        };
        let (chain, errors) = EffectChain::build(&spec, &host, 48000.0, 64);
        assert_eq!(chain.len(), 1);
        assert_eq!(errors.len(), 1);
    }

    /// NAM blocks without a model: their tone stack filters, which keep
    /// state from one sample to the next, so any sample processed out of
    /// order or twice shows
    #[cfg(feature = "nam")]
    fn tone_block(id: usize) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: scanner::NAM_URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    #[cfg(feature = "nam")]
    #[test]
    fn several_cores_play_exactly_what_one_core_plays() {
        let host = Lv2Host::demo();
        let mix = |id, top, bottom| ChainNode::Split {
            instance_id: id,
            split_type: SplitType::Mix,
            enabled: true,
            top_chain: top,
            bottom_chain: bottom,
            ab_select_a: true,
            mix_ratio: 0.3,
            pan: [-0.5, 0.5],
            num_inputs: 1,
            num_outputs: 2,
        };
        let nodes = vec![
            tone_block(1),
            tone_block(2),
            mix(
                3,
                vec![tone_block(4), cab(5)],
                vec![
                    tone_block(6),
                    mix(7, vec![tone_block(8)], vec![tone_block(9)]),
                ],
            ),
            tone_block(10),
            cab(11),
            tone_block(12),
        ];
        // Each block's bass, middle and treble set apart
        let params = (1..=12)
            .map(|id| {
                let mut values = vec![0.0; 10];
                values[5] = (id % 10) as f32;
                values[6] = ((id * 3) % 10) as f32;
                values[7] = ((id * 7) % 10) as f32;
                (id, values)
            })
            .collect();
        let spec = ChainSpec {
            nodes,
            params,
            stereo: true,
            ..Default::default()
        };
        let input: Vec<f32> = (0..128 * 40)
            .map(|i| 0.4 * ((i as f32 * 0.031).sin() + (i as f32 * 0.17).sin() * 0.5))
            .collect();
        let play = |cores: bool| {
            let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 128);
            assert!(errors.is_empty(), "{errors:?}");
            let (mut left, mut right) = (input.clone(), vec![0.0; input.len()]);
            for (l, r) in left.chunks_mut(128).zip(right.chunks_mut(128)) {
                if cores {
                    no_io(|io| chain.process_io(l, r, io));
                } else {
                    // The nodes one after the other, as before the cores
                    no_io(|io| process_nodes(&mut chain.nodes, l, r, io));
                    if !chain.stereo_out {
                        r.copy_from_slice(l);
                    }
                }
            }
            (left, right)
        };
        let one = play(false);
        dsp_pool::start(3);
        // Every period on as many cores as there are
        set_stage_load(0.0);
        let before = dsp_pool::jobs_run();
        let several = play(true);
        assert!(dsp_pool::jobs_run() > before);
        assert_eq!(one.0, several.0);
        assert_eq!(one.1, several.1);
        set_stage_load(0.25);
    }

    #[test]
    fn stages_take_about_the_same_time() {
        let bounds = partition(&[1.0, 1.0, 1.0, 1.0], 2);
        assert_eq!(&bounds[..3], &[0, 2, 4]);
        // A heavy block alone on its core
        let bounds = partition(&[0.1, 5.0, 0.1, 0.1, 0.1], 2);
        assert_eq!(&bounds[..3], &[0, 2, 5]);
        let bounds = partition(&[5.0, 0.1, 0.1, 0.1], 2);
        assert_eq!(&bounds[..3], &[0, 1, 4]);
        // Every stage gets a node, even with no time measured yet
        let bounds = partition(&[0.0; 4], 4);
        assert_eq!(&bounds[..5], &[0, 1, 2, 3, 4]);
        let bounds = partition(&[0.0, 0.0, 9.0], 3);
        assert_eq!(&bounds[..4], &[0, 1, 2, 3]);
    }
}
