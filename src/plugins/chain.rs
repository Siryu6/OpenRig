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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::warn;

use super::instance::PluginInstance;
use super::lv2_host::Lv2Host;
use super::scanner::{self, FileKind};
use crate::audio::split_chain::{ChainNode, SplitType};
use crate::midi::RawMidi;
use crate::monitor::BlockMeter;

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
    /// Two outputs (L/R splits and stereo blocks); otherwise mono
    pub stereo: bool,
    /// Tempo sent to plugins taking the transport (0: none)
    pub bpm: f32,
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
fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// True for a Mix split that places a branch off centre (stereo output)
pub fn is_panned(split_type: SplitType, pan: [f32; 2]) -> bool {
    split_type == SplitType::Mix && pan.iter().any(|&p| p != 0.0)
}

/// Move `current` towards `target` by at most `step`
fn ramp(current: &mut f32, target: f32, step: f32) {
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
    let info = host.scanner().find_by_uri(uri);
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
}

impl PluginProc {
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let start = Instant::now();
        self.run(l, r);
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
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
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
            process_nodes(&mut self.bottom, bl, br);
            let tr = &mut self.top_buf[1][..n];
            process_nodes(&mut self.top, l, tr);
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
            process_nodes(branch, l, r);
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
            process_nodes(&mut self.top, &mut tl[..n], &mut tr[..n]);
            let [bl, br] = &mut self.bottom_buf;
            process_nodes(&mut self.bottom, &mut bl[..n], &mut br[..n]);
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

enum ProcNode {
    Plugin(Box<PluginProc>),
    Split(Box<SplitProc>),
}

/// Process `l` (and `r` where the signal is stereo) through `nodes`
fn process_nodes(nodes: &mut [ProcNode], l: &mut [f32], r: &mut [f32]) {
    for node in nodes {
        match node {
            ProcNode::Plugin(plugin) => plugin.process(l, r),
            ProcNode::Split(split) => split.process(l, r),
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
}

impl EffectChain {
    /// Empty chain (passthrough)
    pub fn empty(max_block: usize) -> Self {
        Self {
            nodes: Vec::new(),
            max_block: max_block.max(1),
            plugin_count: 0,
            stereo_out: false,
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
        let (nodes, stereo_out) =
            chain.build_nodes(&spec.nodes, false, spec, host, sample_rate, &mut errors);
        chain.nodes = nodes;
        chain.stereo_out = stereo_out;
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
                                        warn!("No right-side instance of {}: {}", uri, e);
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
                            })));
                            stereo = stereo_out;
                        }
                        Err(e) => {
                            warn!("Skipping plugin {}: {}", uri, e);
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
                    };
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
        for (l, r) in left
            .chunks_mut(self.max_block)
            .zip(right.chunks_mut(self.max_block))
        {
            process_nodes(&mut self.nodes, l, r);
            if !self.stereo_out {
                r.copy_from_slice(l);
            }
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
}
