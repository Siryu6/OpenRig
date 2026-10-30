use tracing::debug;

use super::atom::{self, AtomUrids};
use super::dsp::Cuts;
#[cfg(feature = "nam")]
use super::dsp::{NoiseGate, ToneStack};
use super::scanner::{PluginCategory, PluginInfo, PortType, IR_SPACE_URI};
use crate::midi::RawMidi;

// Control port indices of the built-in blocks (ports 0 and 1 are audio).
// New controls go at the end: presets store values by position.
mod port {
    pub const IR_LEVEL: usize = 2;
    pub const IR_MIX: usize = 3;
    pub const IR_NORMALIZE: usize = 4;
    pub const IR_LOW_CUT: usize = 5;
    pub const IR_HIGH_CUT: usize = 6;

    #[cfg(feature = "nam")]
    pub const NAM_INPUT: usize = 2;
    #[cfg(feature = "nam")]
    pub const NAM_OUTPUT: usize = 3;
    #[cfg(feature = "nam")]
    pub const NAM_NORMALIZE: usize = 4;
    #[cfg(feature = "nam")]
    pub const NAM_GATE: usize = 5;
    #[cfg(feature = "nam")]
    pub const NAM_THRESHOLD: usize = 6;
    #[cfg(feature = "nam")]
    pub const NAM_BASS: usize = 7;
    #[cfg(feature = "nam")]
    pub const NAM_MIDDLE: usize = 8;
    #[cfg(feature = "nam")]
    pub const NAM_TREBLE: usize = 9;
}

/// Bypass crossfade duration, long enough to avoid clicks
const BYPASS_RAMP_SECONDS: f32 = 0.01;
/// A bypassed block with trails stops running after this much silence
const TAIL_SILENCE_SECONDS: f32 = 0.5;
/// Below this level (about -100 dB) a tail is over
const TAIL_FLOOR: f32 = 1e-5;
/// Byte capacity of each atom port buffer
const ATOM_CAPACITY: usize = 8192;
/// MIDI messages kept for the next run of a plugin
const MIDI_QUEUE: usize = 64;

/// Buffer of an atom port (u64 for 8-byte alignment)
struct AtomPort {
    buffer: Box<[u64]>,
    input: bool,
    /// Takes MIDI events / the transport / patch messages
    midi: bool,
    time: bool,
    #[cfg_attr(not(feature = "lv2"), allow(dead_code))]
    patch: bool,
}

enum Backend {
    /// Copies input to output (demo plugins)
    Passthrough,
    #[cfg(feature = "lv2")]
    Lv2 {
        // Declared before the instance: its thread must stop before the
        // plugin is freed
        worker: Option<super::lv2_worker::Worker>,
        // Declared before `_features`: the instance must be dropped first
        // because the plugin may hold pointers into the feature data.
        instance: lilv::instance::ActiveInstance,
        _features: Box<super::lv2_features::FeatureSet>,
    },
    /// Built-in neural amp; passes audio through until a model is chosen
    #[cfg(feature = "nam")]
    Nam(Box<NamBlock>),
    /// Built-in impulse response loader (cabinets, spaces)
    Ir(Box<IrBlock>),
}

/// Convolution with an impulse response. Controls: level dB, mix %,
/// normalize, low cut Hz, high cut Hz (on the wet signal).
struct IrBlock {
    convolver: Option<super::ir::Convolver>,
    cuts: Cuts,
    normalize_gain: f32,
    /// Gains applied on the previous block, ramped to avoid zipper noise
    wet_gain: f32,
    dry_gain: f32,
}

impl IrBlock {
    /// Linear (wet, dry) gains for the current controls
    fn target_gains(&self, controls: &[f32]) -> (f32, f32) {
        let mix = (controls[port::IR_MIX] / 100.0).clamp(0.0, 1.0);
        let mut wet = 10f32.powf(controls[port::IR_LEVEL] / 20.0) * mix;
        if controls[port::IR_NORMALIZE] >= 0.5 && self.convolver.is_some() {
            wet *= self.normalize_gain;
        }
        (wet, 1.0 - mix)
    }
}

/// Model plus the gains around it. Controls: input dB, output dB,
/// normalize, gate on/off, gate threshold dB, then bass / middle / treble on
/// the amp blocks only, then the quality, then the EQ position (amp blocks).
#[cfg(feature = "nam")]
struct NamBlock {
    model: Option<super::nam::NamModel>,
    /// Port of the Quality control, and the quality the model runs at
    quality_port: Option<usize>,
    quality: f32,
    gate: NoiseGate,
    tone_stack: Option<ToneStack>,
    /// Port of the EQ position (tone stack before or after the model), the
    /// last position seen, and the dry input kept for the gate in Pre mode
    eq_position_port: Option<usize>,
    eq_pre: Option<bool>,
    dry: Vec<f32>,
    /// Gains applied on the previous block, ramped to avoid zipper noise
    input_gain: f32,
    output_gain: f32,
}

#[cfg(feature = "nam")]
impl NamBlock {
    /// Linear (input, output) gains for the current controls
    fn target_gains(&self, controls: &[f32]) -> (f32, f32) {
        let (mut input_db, mut output_db) = (controls[port::NAM_INPUT], controls[port::NAM_OUTPUT]);
        if let Some(model) = &self.model {
            if controls[port::NAM_NORMALIZE] >= 0.5 {
                input_db += model.input_adjustment_db();
                output_db += model.output_adjustment_db();
            }
        }
        let gain = |db: f32| 10f32.powf(db / 20.0);
        (gain(input_db), gain(output_db))
    }
}

/// Multiply `buffer` by a gain ramping linearly from `from` to `to`
#[cfg(feature = "nam")]
fn apply_gain_ramp(buffer: &mut [f32], from: f32, to: f32) {
    let step = (to - from) / buffer.len().max(1) as f32;
    for (i, sample) in buffer.iter_mut().enumerate() {
        *sample *= from + step * (i + 1) as f32;
    }
}

/// A running plugin with every port connected to buffers it owns.
///
/// Buffers are allocated once and connected at construction, so `process`
/// does no allocation and no `connect_port` calls (real-time safe).
pub struct PluginInstance {
    pub info: PluginInfo,
    pub instance_id: usize,
    backend: Backend,
    max_block: usize,

    /// Control value per port index (inputs and outputs)
    controls: Box<[f32]>,
    /// Port index and range of each parameter (control inputs, in order).
    /// Port indexes are not positions in `info.ports`, which skips optional
    /// ports of unknown type
    param_ports: Vec<(usize, f32, f32)>,
    audio_in: Vec<Box<[f32]>>,
    audio_out: Vec<Box<[f32]>>,
    /// CV inputs are fed silence, CV outputs are ignored
    _cv: Vec<Box<[f32]>>,
    atoms: Vec<AtomPort>,
    urids: AtomUrids,
    atom_chunk_urid: u32,
    /// Tempo to send as a transport position at the next run
    tempo: Option<f32>,
    /// MIDI messages for the next run
    midi: [RawMidi; MIDI_QUEUE],
    midi_len: usize,

    bypassed: bool,
    /// 1.0 = fully processed, 0.0 = fully bypassed
    wet: f32,
    wet_step: f32,
    /// Delays and reverbs keep ringing once bypassed (spillover): the
    /// crossfade mutes their input instead of their output
    trails: bool,
    /// Input gain of each frame of the current chunk, with trails
    ramp: Box<[f32]>,
    /// Frames of silence out of a bypassed block with trails
    quiet: usize,
    quiet_limit: usize,
}

// The LV2 handle is only used from one thread at a time.
unsafe impl Send for PluginInstance {}

impl PluginInstance {
    fn with_buffers(
        info: PluginInfo,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
    ) -> Self {
        let block = || vec![0.0f32; max_block].into_boxed_slice();
        let count = |t: PortType| info.ports.iter().filter(|p| p.port_type == t).count();
        let port_count = info.ports.iter().map(|p| p.index + 1).max().unwrap_or(0);

        let info_category = info.category.clone();
        let is_space = info.uri == IR_SPACE_URI;
        let mut controls = vec![0.0f32; port_count].into_boxed_slice();
        for port in &info.ports {
            controls[port.index] = port.default;
        }

        Self {
            param_ports: info
                .control_inputs()
                .map(|p| (p.index, p.min.min(p.max), p.max.max(p.min)))
                .collect(),
            audio_in: (0..count(PortType::AudioInput)).map(|_| block()).collect(),
            audio_out: (0..count(PortType::AudioOutput)).map(|_| block()).collect(),
            _cv: (0..count(PortType::CVInput) + count(PortType::CVOutput))
                .map(|_| block())
                .collect(),
            atoms: Vec::new(),
            urids: AtomUrids::default(),
            atom_chunk_urid: 0,
            tempo: None,
            midi: [RawMidi::default(); MIDI_QUEUE],
            midi_len: 0,
            controls,
            info,
            instance_id,
            backend: Backend::Passthrough,
            max_block,
            bypassed: false,
            wet: 1.0,
            wet_step: 1.0 / (sample_rate * BYPASS_RAMP_SECONDS).max(1.0),
            trails: matches!(
                info_category,
                PluginCategory::Delay | PluginCategory::Reverb
            ) || is_space,
            ramp: block(),
            quiet: 0,
            quiet_limit: (sample_rate * TAIL_SILENCE_SECONDS) as usize,
        }
    }

    /// Plugin that passes audio through unchanged (demo catalogue)
    pub fn passthrough(info: PluginInfo, instance_id: usize, max_block: usize) -> Self {
        Self::with_buffers(info, instance_id, 48000.0, max_block)
    }

    /// Instantiate, connect and activate a real LV2 plugin
    #[cfg(feature = "lv2")]
    pub fn lv2(
        world: &lilv::World,
        info: PluginInfo,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
    ) -> anyhow::Result<Self> {
        use super::lv2_features::{map_uri, FeatureSet, ATOM_CHUNK, ATOM_SEQUENCE};

        debug!("Instantiating LV2 plugin {} @ {}Hz", info.uri, sample_rate);
        let plugin = world
            .plugins()
            .plugin(&world.new_uri(&info.uri))
            .ok_or_else(|| anyhow::anyhow!("Plugin not in LV2 world: {}", info.uri))?;

        let features = Box::new(FeatureSet::new(sample_rate, max_block));
        let mut instance = unsafe { plugin.instantiate(sample_rate as f64, features.features()) }
            .ok_or_else(|| anyhow::anyhow!("Failed to instantiate {}", info.name))?;

        let mut this = Self::with_buffers(info, instance_id, sample_rate, max_block);
        this.urids = AtomUrids::new(map_uri(ATOM_SEQUENCE), map_uri);
        this.atom_chunk_urid = map_uri(ATOM_CHUNK);

        let (mut audio_in, mut audio_out, mut cv) = (0, 0, 0);
        for port in this.info.ports.clone() {
            let index = port.index;
            unsafe {
                match port.port_type {
                    PortType::ControlInput | PortType::ControlOutput => {
                        instance.connect_port_mut(index, this.controls.as_mut_ptr().add(index));
                    }
                    PortType::AudioInput => {
                        instance.connect_port_mut(index, this.audio_in[audio_in].as_mut_ptr());
                        audio_in += 1;
                    }
                    PortType::AudioOutput => {
                        instance.connect_port_mut(index, this.audio_out[audio_out].as_mut_ptr());
                        audio_out += 1;
                    }
                    PortType::CVInput | PortType::CVOutput => {
                        instance.connect_port_mut(index, this._cv[cv].as_mut_ptr());
                        cv += 1;
                    }
                    PortType::AtomInput | PortType::AtomOutput => {
                        let mut buffer = vec![0u64; ATOM_CAPACITY / 8].into_boxed_slice();
                        instance.connect_port_mut(index, buffer.as_mut_ptr());
                        this.atoms.push(AtomPort {
                            buffer,
                            input: port.port_type == PortType::AtomInput,
                            midi: port.midi,
                            time: port.time,
                            patch: port.patch,
                        });
                    }
                }
            }
        }

        this.reset_atoms();
        let instance = unsafe { instance.activate() };
        // Plugins doing slow jobs (loading files, applying a state) off
        // the audio thread
        // (lilv calls extension_data without checking it, and it is optional)
        let has_extension_data = instance.instance().descriptor().is_some_and(|d| {
            // Typed as a function by lv2_raw, though plugins may leave it NULL
            let field = std::ptr::addr_of!(d.extension_data) as *const *const std::ffi::c_void;
            !unsafe { field.read() }.is_null()
        });
        let interface = if has_extension_data {
            unsafe {
                instance
                    .instance()
                    .extension_data::<super::lv2_worker::Interface>(super::lv2_worker::INTERFACE)
            }
        } else {
            None
        };
        let worker = match interface {
            Some(interface) => Some(unsafe {
                super::lv2_worker::Worker::start(
                    features.worker(),
                    interface,
                    instance.instance().handle(),
                )
            }?),
            None => None,
        };
        this.backend = Backend::Lv2 {
            worker,
            instance,
            _features: features,
        };
        Ok(this)
    }

    /// Built-in neural amp block, loading the model at `file` if given
    #[cfg(feature = "nam")]
    pub fn nam(
        info: PluginInfo,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
        file: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let model = match file {
            Some(path) => {
                let model = super::nam::NamModel::load(path, sample_rate, max_block)?;
                if let Some(rate) = model.sample_rate() {
                    if (rate - sample_rate).abs() > 1.0 && sample_rate % rate != 0.0 {
                        tracing::warn!(
                            "{} was trained at {} Hz but audio runs at {} Hz: tone will be off",
                            path.display(),
                            rate,
                            sample_rate
                        );
                    }
                }
                Some(model)
            }
            None => None,
        };
        let mut this = Self::with_buffers(info, instance_id, sample_rate, max_block);
        let tone_stack =
            (this.controls.len() > port::NAM_TREBLE).then(|| ToneStack::new(sample_rate));
        let port_named = |name: &str| {
            this.info
                .ports
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.index)
        };
        let quality_port = port_named(super::scanner::NAM_QUALITY);
        let eq_position_port = port_named(super::scanner::NAM_EQ_POSITION);
        let dry = if eq_position_port.is_some() {
            vec![0.0; max_block]
        } else {
            Vec::new()
        };
        let mut block = NamBlock {
            model,
            quality_port,
            // Applied by the first process
            quality: f32::NAN,
            gate: NoiseGate::new(sample_rate),
            tone_stack,
            eq_position_port,
            eq_pre: None,
            dry,
            input_gain: 1.0,
            output_gain: 1.0,
        };
        (block.input_gain, block.output_gain) = block.target_gains(&this.controls);
        this.backend = Backend::Nam(Box::new(block));
        Ok(this)
    }

    /// Built-in impulse response loader, convolving with the WAV file at
    /// `file` if given: with its `channel` (one side of a stereo file), or
    /// with all its channels mixed to mono
    pub fn ir(
        info: PluginInfo,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
        file: Option<&std::path::Path>,
        channel: Option<usize>,
    ) -> anyhow::Result<Self> {
        let (convolver, normalize_gain) = match file {
            Some(path) => {
                let response = super::ir::ImpulseResponse::load(path, sample_rate, channel)?;
                debug!(
                    "Impulse response {}: {} samples",
                    path.display(),
                    response.samples.len()
                );
                (
                    Some(response.convolver(max_block)?),
                    response.normalize_gain,
                )
            }
            None => (None, 1.0),
        };
        let mut this = Self::with_buffers(info, instance_id, sample_rate, max_block);
        let mut block = IrBlock {
            convolver,
            cuts: Cuts::new(sample_rate),
            normalize_gain,
            wet_gain: 0.0,
            dry_gain: 0.0,
        };
        (block.wet_gain, block.dry_gain) = block.target_gains(&this.controls);
        this.backend = Backend::Ir(Box::new(block));
        Ok(this)
    }

    /// Prepare atom buffers before `run`: input sequences with the pending
    /// transport and MIDI events (once), and output buffers advertising
    /// their capacity as required by the atom spec.
    fn reset_atoms(&mut self) {
        let midi = &self.midi[..self.midi_len];
        for port in &mut self.atoms {
            if port.input {
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        port.buffer.as_mut_ptr() as *mut u8,
                        port.buffer.len() * 8,
                    )
                };
                let tempo = self.tempo.filter(|_| port.time);
                let midi = if port.midi { midi } else { &[] };
                atom::write_input(bytes, &self.urids, tempo, midi);
            } else {
                // LV2_Atom header: { size, type }
                let header = port.buffer.as_mut_ptr() as *mut u32;
                unsafe {
                    *header = (ATOM_CAPACITY - 8) as u32;
                    *header.add(1) = self.atom_chunk_urid;
                }
            }
        }
        self.tempo = None;
        self.midi_len = 0;
    }

    /// Give an LV2 plugin its files (`patch:Set` of each parameter URI to a
    /// path), then run it on silence until it has loaded them: until its
    /// worker is idle, at most `timeout`. Not real-time safe: for a new
    /// instance, before the audio thread gets it.
    pub fn load_files(
        &mut self,
        files: &[(String, std::path::PathBuf)],
        timeout: std::time::Duration,
    ) {
        #[cfg(feature = "lv2")]
        if let Backend::Lv2 { .. } = self.backend {
            use super::lv2_features::map_uri;
            let Some(port) = self
                .atoms
                .iter()
                .position(|p| p.input && p.patch)
                .or_else(|| self.atoms.iter().position(|p| p.input))
            else {
                debug!("{}: no atom input to send its files to", self.info.uri);
                return;
            };
            let paths: Vec<(u32, String)> = files
                .iter()
                .filter_map(|(uri, path)| Some((map_uri(uri), path.to_str()?.to_string())))
                .collect();
            if paths.is_empty() {
                return;
            }
            let sets: Vec<(u32, &str)> = paths.iter().map(|(u, p)| (*u, p.as_str())).collect();
            let frames = self.max_block;
            let deadline = std::time::Instant::now() + timeout;
            let mut sent = 0;
            loop {
                self.reset_atoms();
                if sent < sets.len() {
                    let buffer = &mut self.atoms[port].buffer;
                    let bytes = unsafe {
                        std::slice::from_raw_parts_mut(
                            buffer.as_mut_ptr() as *mut u8,
                            buffer.len() * 8,
                        )
                    };
                    sent += atom::write_patch_sets(bytes, &self.urids, &sets[sent..]).max(1);
                }
                for input in &mut self.audio_in {
                    input.fill(0.0);
                }
                self.run(frames);
                let idle = match &self.backend {
                    Backend::Lv2 { worker, .. } => worker.as_ref().is_none_or(|w| w.is_idle()),
                    _ => true,
                };
                if sent >= sets.len() && idle {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    tracing::warn!(
                        "{}: still loading its files after {:?}",
                        self.info.name,
                        timeout
                    );
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            self.reset_atoms();
        }
        let _ = (files, timeout);
    }

    /// Send the tempo as a transport position at the next run (plugins
    /// taking time:Position)
    pub fn set_tempo(&mut self, bpm: f32) {
        if self.atoms.iter().any(|p| p.time) {
            self.tempo = Some(bpm);
        }
    }

    /// Queue a MIDI message for the next run (plugins with a MIDI input);
    /// dropped when the queue is full
    pub fn send_midi(&mut self, message: RawMidi) {
        if self.midi_len < MIDI_QUEUE && self.atoms.iter().any(|p| p.midi) {
            self.midi[self.midi_len] = message;
            self.midi_len += 1;
        }
    }

    fn run(&mut self, frames: usize) {
        match &mut self.backend {
            Backend::Passthrough => {
                // Each output copies the input of the same rank (or the last)
                if let Some(last) = self.audio_in.len().checked_sub(1) {
                    for (i, output) in self.audio_out.iter_mut().enumerate() {
                        output[..frames].copy_from_slice(&self.audio_in[i.min(last)][..frames]);
                    }
                }
            }
            #[cfg(feature = "lv2")]
            Backend::Lv2 {
                instance, worker, ..
            } => {
                unsafe { instance.run(frames) };
                if let Some(worker) = worker {
                    worker.end_run();
                }
            }
            Backend::Ir(block) => {
                let (wet_gain, dry_gain) = block.target_gains(&self.controls);
                let input = &self.audio_in[0][..frames];
                let output = &mut self.audio_out[0][..frames];
                match &mut block.convolver {
                    Some(convolver) => convolver.process(input, output),
                    None => output.copy_from_slice(input),
                }
                if let (Some(&low), Some(&high)) = (
                    self.controls.get(port::IR_LOW_CUT),
                    self.controls.get(port::IR_HIGH_CUT),
                ) {
                    block.cuts.process(output, low, high);
                }
                let n = frames.max(1) as f32;
                let (wet_step, dry_step) = (
                    (wet_gain - block.wet_gain) / n,
                    (dry_gain - block.dry_gain) / n,
                );
                for (i, (out, dry)) in output.iter_mut().zip(input).enumerate() {
                    let k = (i + 1) as f32;
                    *out = *out * (block.wet_gain + wet_step * k)
                        + *dry * (block.dry_gain + dry_step * k);
                }
                block.wet_gain = wet_gain;
                block.dry_gain = dry_gain;
            }
            #[cfg(feature = "nam")]
            Backend::Nam(block) => {
                let (input_gain, output_gain) = block.target_gains(&self.controls);
                let input = &mut self.audio_in[0][..frames];
                let output = &mut self.audio_out[0][..frames];
                apply_gain_ramp(input, block.input_gain, input_gain);
                if let (Some(model), Some(port)) = (&mut block.model, block.quality_port) {
                    let quality = self.controls[port];
                    if quality != block.quality {
                        model.set_quality(quality);
                        block.quality = quality;
                    }
                }
                let controls = &self.controls;
                // Tone stack after the model (as in the NAM plugin), or before
                // it to shape what hits the amp
                let pre = block
                    .eq_position_port
                    .is_some_and(|port| controls[port] < 0.5);
                if block.eq_pre != Some(pre) {
                    if let Some(tone_stack) = &mut block.tone_stack {
                        tone_stack.reset();
                    }
                    block.eq_pre = Some(pre);
                }
                let mut tone_stack = |buffer: &mut [f32]| {
                    if let Some(tone_stack) = &mut block.tone_stack {
                        tone_stack.process(
                            buffer,
                            controls[port::NAM_BASS],
                            controls[port::NAM_MIDDLE],
                            controls[port::NAM_TREBLE],
                        );
                    }
                };
                // The gate listens to the input before the EQ
                let detector: &[f32] = if pre {
                    block.dry[..frames].copy_from_slice(input);
                    tone_stack(input);
                    &block.dry[..frames]
                } else {
                    input
                };
                match &mut block.model {
                    Some(model) => model.process(input, output),
                    None => output.copy_from_slice(input),
                }
                // Same order as the NAM plugin: gate, tone stack, output level
                if controls.get(port::NAM_GATE).is_some_and(|&on| on >= 0.5) {
                    block
                        .gate
                        .process(detector, output, controls[port::NAM_THRESHOLD]);
                } else {
                    block.gate.reset();
                }
                if !pre {
                    tone_stack(output);
                }
                apply_gain_ramp(output, block.output_gain, output_gain);
                block.input_gain = input_gain;
                block.output_gain = output_gain;
            }
        }
    }

    /// Process a mono signal in place. Multiple outputs are summed back to
    /// mono.
    pub fn process_in_place(&mut self, buffer: &mut [f32]) {
        self.process(buffer, None, false);
    }

    /// Process a block in place. `stereo_in` says whether `right` holds the
    /// right side of the input (else the input is mono, in `left`); with
    /// `right`, the output is stereo. A mono input feeds every audio input;
    /// a stereo one feeds the first two, or their mix a single input.
    pub fn process(&mut self, left: &mut [f32], mut right: Option<&mut [f32]>, stereo_in: bool) {
        let stereo_in = stereo_in && right.is_some();
        if self.bypassed && self.wet <= 0.0 && (!self.trails || self.quiet >= self.quiet_limit) {
            // Not run: queued notes would play late
            self.midi_len = 0;
            if let (Some(right), false) = (right, stereo_in) {
                right.copy_from_slice(left);
            }
            return;
        }
        let mut start = 0;
        while start < left.len() {
            let end = (start + self.max_block).min(left.len());
            let right = right.as_deref_mut().map(|r| &mut r[start..end]);
            if self.trails {
                self.process_trails_chunk(&mut left[start..end], right, stereo_in);
            } else {
                self.process_chunk(&mut left[start..end], right, stereo_in);
            }
            start = end;
        }
    }

    fn process_chunk(&mut self, left: &mut [f32], mut right: Option<&mut [f32]>, stereo_in: bool) {
        let frames = left.len();
        let right_in = right.as_deref().filter(|_| stereo_in);
        fill_inputs(&mut self.audio_in, left, right_in, None);
        self.reset_atoms();
        self.run(frames);

        let outputs = self.audio_out.len().min(2);
        if outputs == 0 {
            // Nothing comes out (analysis plugin): the signal goes on
            if let (Some(right), false) = (right, stereo_in) {
                right.copy_from_slice(left);
            }
            return;
        }
        let target = if self.bypassed { 0.0 } else { 1.0 };
        if self.wet == 1.0 && target == 1.0 {
            // Fully on, the common case: no ramp to mix in
            let (wet_l, wet_r) = (
                &self.audio_out[0][..frames],
                &self.audio_out[outputs - 1][..frames],
            );
            match right.as_deref_mut() {
                Some(r) => {
                    left.copy_from_slice(wet_l);
                    r.copy_from_slice(wet_r);
                }
                None => {
                    for ((l, a), b) in left.iter_mut().zip(wet_l).zip(wet_r) {
                        *l = (a + b) * 0.5;
                    }
                }
            }
            return;
        }
        for i in 0..frames {
            let wet_l = self.audio_out[0][i];
            let wet_r = self.audio_out[outputs - 1][i];
            if self.wet != target {
                self.wet = if target > self.wet {
                    (self.wet + self.wet_step).min(1.0)
                } else {
                    (self.wet - self.wet_step).max(0.0)
                };
            }
            let (w, dry_l) = (self.wet, left[i]);
            match right.as_deref_mut() {
                Some(r) => {
                    let dry_r = if stereo_in { r[i] } else { dry_l };
                    left[i] = wet_l * w + dry_l * (1.0 - w);
                    r[i] = wet_r * w + dry_r * (1.0 - w);
                }
                None => left[i] = (wet_l + wet_r) * 0.5 * w + dry_l * (1.0 - w),
            }
        }
    }

    /// A chunk of a block with trails: the crossfade ramps its input, and
    /// the dry signal is added to its output while it fades out. Bypassed,
    /// it runs on silence until its tail dies out.
    fn process_trails_chunk(
        &mut self,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        stereo_in: bool,
    ) {
        let frames = left.len();
        let target = if self.bypassed { 0.0 } else { 1.0 };
        for gain in &mut self.ramp[..frames] {
            if self.wet != target {
                self.wet = if target > self.wet {
                    (self.wet + self.wet_step).min(1.0)
                } else {
                    (self.wet - self.wet_step).max(0.0)
                };
            }
            *gain = self.wet;
        }
        let ramp = &self.ramp[..frames];
        let right_in = right.as_deref().filter(|_| stereo_in);
        fill_inputs(&mut self.audio_in, left, right_in, Some(ramp));
        self.reset_atoms();
        self.run(frames);

        let outputs = self.audio_out.len().min(2);
        if outputs == 0 {
            if let (Some(right), false) = (right, stereo_in) {
                right.copy_from_slice(left);
            }
            return;
        }
        let ramp = &self.ramp[..frames];
        let (wet_l, wet_r) = (
            &self.audio_out[0][..frames],
            &self.audio_out[outputs - 1][..frames],
        );
        let mut peak = 0.0f32;
        for i in 0..frames {
            let (dry, dry_l) = (1.0 - ramp[i], left[i]);
            peak = peak.max(wet_l[i].abs()).max(wet_r[i].abs());
            match right.as_deref_mut() {
                Some(r) => {
                    let dry_r = if stereo_in { r[i] } else { dry_l };
                    left[i] = wet_l[i] + dry_l * dry;
                    r[i] = wet_r[i] + dry_r * dry;
                }
                None => left[i] = (wet_l[i] + wet_r[i]) * 0.5 + dry_l * dry,
            }
        }
        self.quiet = if self.bypassed && self.wet <= 0.0 && peak < TAIL_FLOOR {
            self.quiet + frames
        } else {
            0
        };
    }

    /// True for blocks that keep ringing once bypassed (delays, reverbs)
    #[cfg(test)]
    pub fn has_trails(&self) -> bool {
        self.trails
    }

    /// Number of audio inputs and outputs
    pub fn audio_ports(&self) -> (usize, usize) {
        (self.audio_in.len(), self.audio_out.len())
    }

    /// Set a parameter value (clamped to its range)
    pub fn set_parameter(&mut self, index: usize, value: f32) {
        // Called from the audio thread: no logging
        if let Some(&(port, lo, hi)) = self.param_ports.get(index) {
            self.controls[port] = value.clamp(lo, hi);
        }
    }

    /// Value of every control port, by port index (outputs too)
    pub fn controls(&self) -> &[f32] {
        &self.controls
    }

    /// Current value of a parameter
    pub fn parameter(&self, index: usize) -> Option<f32> {
        self.param_ports
            .get(index)
            .map(|&(port, ..)| self.controls[port])
    }

    /// Bypass with a short crossfade
    pub fn set_bypass(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
        self.quiet = 0;
    }

    /// Bypass immediately, without crossfade (when building a chain)
    pub fn set_bypass_immediate(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
        self.wet = if bypassed { 0.0 } else { 1.0 };
        // Built bypassed: no tail to play
        self.quiet = if bypassed { self.quiet_limit } else { 0 };
    }
}

/// Copy a chunk to the audio inputs, scaled by `gain` per frame if given.
/// A mono input feeds every audio input; a stereo one (`right`) feeds the
/// first two, or their mix a single input.
fn fill_inputs(
    inputs: &mut [Box<[f32]>],
    left: &[f32],
    right: Option<&[f32]>,
    gain: Option<&[f32]>,
) {
    let frames = left.len();
    let count = inputs.len();
    for (i, input) in inputs.iter_mut().enumerate() {
        let input = &mut input[..frames];
        match right {
            Some(r) if count == 1 => {
                for ((s, l), r) in input.iter_mut().zip(left).zip(r) {
                    *s = (l + r) * 0.5;
                }
            }
            Some(r) if i == 1 => input.copy_from_slice(r),
            _ => input.copy_from_slice(left),
        }
        if let Some(gain) = gain {
            for (s, g) in input.iter_mut().zip(gain) {
                *s *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scanner::PluginScanner;

    fn demo_instance() -> PluginInstance {
        let scanner = PluginScanner::demo();
        let info = scanner
            .find_by_uri("urn:pedalboard:demo:overdrive")
            .unwrap()
            .clone();
        PluginInstance::passthrough(info, 7, 64)
    }

    #[test]
    fn passthrough_handles_blocks_larger_than_max_block() {
        let mut plugin = demo_instance();
        let mut buffer: Vec<f32> = (0..200).map(|i| i as f32).collect();
        let expected = buffer.clone();
        plugin.process_in_place(&mut buffer);
        assert_eq!(buffer, expected);
    }

    #[test]
    fn ir_block_convolves_and_mixes() {
        use crate::plugins::scanner::{builtin_plugins, IR_URI};
        let info = builtin_plugins()
            .into_iter()
            .find(|p| p.uri == IR_URI)
            .unwrap();
        let dir = std::env::temp_dir().join(format!("pedalboard-ir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("echo.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for s in [1.0f32, 0.0, 0.0, 0.5] {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();

        let mut plugin =
            PluginInstance::ir(info.clone(), 1, 48000.0, 64, Some(&path), None).unwrap();
        plugin.set_parameter(2, 0.0); // normalize off
        let mut buffer = vec![0.0f32; 64];
        buffer[0] = 1.0;
        plugin.process_in_place(&mut buffer);
        // Gains settle after the first block: check the second one
        let mut buffer = vec![0.0f32; 64];
        buffer[0] = 1.0;
        plugin.process_in_place(&mut buffer);
        assert!((buffer[0] - 1.0).abs() < 1e-5);
        assert!((buffer[3] - 0.5).abs() < 1e-5);

        // 50 % mix: half dry, half wet
        plugin.set_parameter(1, 50.0);
        for _ in 0..2 {
            buffer = vec![0.0f32; 64];
            buffer[0] = 1.0;
            plugin.process_in_place(&mut buffer);
        }
        assert!((buffer[0] - 1.0).abs() < 1e-5);
        assert!((buffer[3] - 0.25).abs() < 1e-5);

        // Without a file the block is transparent
        let mut empty = PluginInstance::ir(info, 2, 48000.0, 64, None, None).unwrap();
        let mut buffer = vec![0.3f32; 64];
        empty.process_in_place(&mut buffer);
        assert!(buffer.iter().all(|s| (s - 0.3).abs() < 1e-6));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn space_keeps_ringing_once_bypassed() {
        use crate::plugins::scanner::{builtin_plugins, IR_SPACE_URI};
        let info = builtin_plugins()
            .into_iter()
            .find(|p| p.uri == IR_SPACE_URI)
            .unwrap();
        let dir = std::env::temp_dir().join(format!("pedalboard-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("echo.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        // An echo 1000 frames late, well after the 10 ms crossfade
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..=1000 {
            writer
                .write_sample(if i == 1000 { 0.5f32 } else { 0.0 })
                .unwrap();
        }
        writer.finalize().unwrap();

        let mut plugin = PluginInstance::ir(info, 1, 48000.0, 64, Some(&path), None).unwrap();
        assert!(plugin.has_trails());
        plugin.set_parameter(2, 0.0); // normalize off
        plugin.process_in_place(&mut vec![0.0f32; 64]);
        let mut buffer = vec![0.0f32; 64];
        buffer[0] = 1.0;
        plugin.process_in_place(&mut buffer);

        // Bypassed: the input is muted, the echo still comes out
        plugin.set_bypass(true);
        let mut echo = 0.0f32;
        for _ in 0..20 {
            let mut buffer = vec![0.0f32; 64];
            plugin.process_in_place(&mut buffer);
            echo = buffer.iter().fold(echo, |m, s| m.max(s.abs()));
        }
        assert!(echo > 0.05, "echo {echo}");

        // New notes pass dry, without reaching the reverb
        let mut buffer = vec![0.25f32; 64];
        plugin.process_in_place(&mut buffer);
        assert!(buffer.iter().all(|s| (s - 0.25).abs() < 1e-5));

        // Once the tail has died out the block stops running
        for _ in 0..800 {
            plugin.process_in_place(&mut vec![0.0f32; 64]);
        }
        assert!(plugin.quiet >= plugin.quiet_limit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(feature = "nam")]
    #[test]
    fn nam_block_without_model_applies_gains() {
        use crate::plugins::scanner::{builtin_plugins, NAM_URI};
        let info = builtin_plugins()
            .into_iter()
            .find(|p| p.uri == NAM_URI)
            .unwrap();
        let mut plugin = PluginInstance::nam(info, 1, 48000.0, 64, None).unwrap();
        let mut buffer = vec![0.5f32; 64];
        plugin.process_in_place(&mut buffer);
        assert_eq!(buffer, vec![0.5; 64]);

        plugin.set_parameter(1, -6.0); // output -6 dB, ramped over one block
        let mut buffer = vec![0.5f32; 128];
        plugin.process_in_place(&mut buffer);
        assert!((buffer[127] - 0.5 * 10f32.powf(-6.0 / 20.0)).abs() < 1e-4);
    }

    #[cfg(feature = "nam")]
    #[test]
    fn nam_tone_stack_runs_before_or_after_the_model() {
        use crate::plugins::scanner::{builtin_plugins, NAM_URI};
        let info = builtin_plugins()
            .into_iter()
            .find(|p| p.uri == NAM_URI)
            .unwrap();
        // A low tone: the bass shelf boosts it in both positions
        let tone: Vec<f32> = (0..4800)
            .map(|i| 0.1 * (i as f32 * 2.0 * std::f32::consts::PI * 80.0 / 48000.0).sin())
            .collect();
        let peak = |eq_position: f32| {
            let mut plugin = PluginInstance::nam(info.clone(), 1, 48000.0, 64, None).unwrap();
            plugin.set_parameter(5, 10.0); // bass up
            plugin.set_parameter(9, eq_position);
            let mut peak = 0.0f32;
            for chunk in tone.chunks(64) {
                let mut buffer = chunk.to_vec();
                plugin.process_in_place(&mut buffer);
                peak = buffer.iter().fold(peak, |p, s| p.max(s.abs()));
            }
            peak
        };
        let (pre, post) = (peak(0.0), peak(1.0));
        assert!(pre > 0.15 && post > 0.15, "pre {pre}, post {post}");
        // Without a model both orders sound the same
        assert!((pre - post).abs() < 1e-3);
    }

    #[test]
    fn parameters_are_clamped() {
        let mut plugin = demo_instance();
        assert_eq!(plugin.parameter(0), Some(0.5));
        plugin.set_parameter(0, 5.0);
        assert_eq!(plugin.parameter(0), Some(1.0));
        plugin.set_parameter(99, 1.0); // ignored
        assert_eq!(plugin.parameter(99), None);
    }
}
