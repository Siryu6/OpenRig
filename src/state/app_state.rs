use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

use super::tempo::TapTempo;
use crate::audio::{ChainControl, ChainNode, Looper, SplitType, Tuner};
use crate::midi::{MidiAssign, MidiSettings, MidiTarget, Step};
use crate::plugins::{ChainCommand, ChainSpec, Lv2Host, SignalMap};
use crate::presets::{
    PluginConfig, Preset, PresetChain, PresetManager, SceneManager, SceneSnapshot, DEFAULT_BPM,
};

/// Models of one tone: the files of a folder of the models directory (a
/// TONE3000 download or a folder the user made). A file at the top of the
/// directory is a tone of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelPack {
    pub title: String,
    pub creator: Option<String>,
    /// TONE3000 gear type, if known
    pub gear: Option<String>,
    /// Presets: paths relative to the models directory, sorted
    pub files: Vec<String>,
}

impl ModelPack {
    fn new(root: &Path, dir: &Path, files: Vec<String>) -> Self {
        #[derive(serde::Deserialize)]
        struct ToneInfo {
            title: Option<String>,
            creator: Option<String>,
            gear: Option<String>,
        }
        let info: Option<ToneInfo> = (!dir.as_os_str().is_empty())
            .then(|| {
                let path = root.join(dir).join(crate::plugins::scanner::TONE_INFO_FILE);
                serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
            })
            .flatten();
        let fallback = if dir.as_os_str().is_empty() {
            files
                .first()
                .map(|f| Self::preset_name(f))
                .unwrap_or_default()
        } else {
            without_id(&dir.file_name().unwrap_or_default().to_string_lossy()).to_string()
        };
        let (title, creator, gear) = match info {
            Some(info) => (info.title.unwrap_or(fallback), info.creator, info.gear),
            None => (fallback, None, None),
        };
        Self {
            title,
            creator,
            gear,
            files,
        }
    }

    /// Title for the small rack blocks, without what follows a separator
    /// ("Mesa Boogie 2x12 V30 - XR IR PACK" -> "Mesa Boogie 2x12 V30")
    pub fn short_title(&self) -> String {
        let cut = [" - ", " | ", " (", " [", ": "]
            .iter()
            .filter_map(|sep| self.title.find(sep))
            .filter(|&i| i > 0)
            .min();
        cut.map_or(&*self.title, |i| self.title[..i].trim_end())
            .to_string()
    }

    /// Short preset names: file names without the words every preset of
    /// the tone starts with ("Boss MT-2 - Doom Metal" -> "Doom Metal")
    pub fn preset_labels(&self) -> Vec<String> {
        let names: Vec<String> = self.files.iter().map(|f| Self::preset_name(f)).collect();
        if names.len() < 2 {
            return names;
        }
        let first = &names[0];
        let mut common = names.iter().skip(1).fold(first.len(), |len, name| {
            first
                .char_indices()
                .zip(name.chars())
                .take_while(|((i, a), b)| *i < len && a == b)
                .last()
                .map_or(0, |((i, a), _)| i + a.len_utf8())
        });
        // Cut at a word boundary only
        common = first[..common]
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + 1);
        let trimmed: Vec<String> = names
            .iter()
            .map(|n| {
                n[common..]
                    .trim_start_matches([' ', '-', '_', ':', '·'])
                    .to_string()
            })
            .collect();
        if trimmed.iter().any(String::is_empty) {
            names
        } else {
            trimmed
        }
    }

    /// Display name of a preset: its file name without extension
    pub fn preset_name(file: &str) -> String {
        Path::new(file).file_stem().map_or_else(
            || file.to_string(),
            |s| without_id(&s.to_string_lossy()).to_string(),
        )
    }
}

/// A path relative to the models directory as saved in presets: always
/// with `/`, so presets move between Windows and other systems
fn portable(path: &Path) -> String {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        path.into_owned()
    } else {
        path.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// TONE3000 folders and models end with their id, which is not shown:
/// "Boss MT-2 [96210]" -> "Boss MT-2"
fn without_id(name: &str) -> &str {
    match name.rsplit_once(" [") {
        Some((title, id))
            if id.len() > 1
                && id.ends_with(']')
                && id[..id.len() - 1].bytes().all(|b| b.is_ascii_digit()) =>
        {
            title
        }
        _ => name,
    }
}

/// Where presets and scenes are stored: `$PEDALBOARD_DATA_DIR`, else the
/// platform data directory (`~/.local/share/pedalboard` on Linux).
pub fn data_dir() -> PathBuf {
    std::env::var_os("PEDALBOARD_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("pedalboard")))
        .unwrap_or_else(|| PathBuf::from("pedalboard-data"))
}

pub(super) fn node_id(node: &ChainNode) -> usize {
    match node {
        ChainNode::Plugin { instance_id, .. } | ChainNode::Split { instance_id, .. } => {
            *instance_id
        }
    }
}

pub(super) fn find_in(nodes: &[ChainNode], id: usize) -> Option<&ChainNode> {
    for node in nodes {
        if node_id(node) == id {
            return Some(node);
        }
        if let ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } = node
        {
            if let Some(found) = find_in(top_chain, id).or_else(|| find_in(bottom_chain, id)) {
                return Some(found);
            }
        }
    }
    None
}

pub(super) fn find_in_mut(nodes: &mut [ChainNode], id: usize) -> Option<&mut ChainNode> {
    for node in nodes {
        if node_id(node) == id {
            return Some(node);
        }
        if let ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } = node
        {
            if let Some(found) = find_in_mut(top_chain, id) {
                return Some(found);
            }
            if let Some(found) = find_in_mut(bottom_chain, id) {
                return Some(found);
            }
        }
    }
    None
}

fn remove_in(nodes: &mut Vec<ChainNode>, id: usize) -> Option<ChainNode> {
    if let Some(pos) = nodes.iter().position(|n| node_id(n) == id) {
        return Some(nodes.remove(pos));
    }
    for node in nodes.iter_mut() {
        if let ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } = node
        {
            if let Some(removed) = remove_in(top_chain, id).or_else(|| remove_in(bottom_chain, id))
            {
                return Some(removed);
            }
        }
    }
    None
}

/// Chain holding the places of `parent`: the main chain, or a split branch
fn container_mut(
    nodes: &mut Vec<ChainNode>,
    parent: Option<(usize, bool)>,
) -> Option<&mut Vec<ChainNode>> {
    match parent {
        None => Some(nodes),
        Some((split_id, top)) => match find_in_mut(nodes, split_id)? {
            ChainNode::Split {
                top_chain,
                bottom_chain,
                ..
            } => Some(if top { top_chain } else { bottom_chain }),
            ChainNode::Plugin { .. } => None,
        },
    }
}

/// Position of a node: the chain it is in and its index there
fn place_of(nodes: &[ChainNode], id: usize, parent: Option<(usize, bool)>) -> Option<ChainPlace> {
    if let Some(index) = nodes.iter().position(|n| node_id(n) == id) {
        return Some(ChainPlace { parent, index });
    }
    nodes.iter().find_map(|node| match node {
        ChainNode::Split {
            instance_id,
            top_chain,
            bottom_chain,
            ..
        } => place_of(top_chain, id, Some((*instance_id, true)))
            .or_else(|| place_of(bottom_chain, id, Some((*instance_id, false)))),
        ChainNode::Plugin { .. } => None,
    })
}

pub(super) fn collect_ids(nodes: &[ChainNode], out: &mut Vec<usize>) {
    for node in nodes {
        out.push(node_id(node));
        if let ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } = node
        {
            collect_ids(top_chain, out);
            collect_ids(bottom_chain, out);
        }
    }
}

fn collect_plugins<'a>(nodes: &'a [ChainNode], out: &mut Vec<&'a ChainNode>) {
    for node in nodes {
        match node {
            ChainNode::Plugin { .. } => out.push(node),
            ChainNode::Split {
                top_chain,
                bottom_chain,
                ..
            } => {
                collect_plugins(top_chain, out);
                collect_plugins(bottom_chain, out);
            }
        }
    }
}

/// A place in the chain tree: index in the main chain (`parent` None) or in
/// the top / bottom branch of split `parent`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainPlace {
    pub parent: Option<(usize, bool)>,
    pub index: usize,
}

/// The effect chain as edited by the UI: a tree of plugins and splits plus
/// the parameter values of every plugin. This is the single source of truth;
/// the audio thread receives copies of it.
#[derive(Debug, Clone)]
pub struct EffectChainState {
    pub nodes: Vec<ChainNode>,
    /// Parameter values per plugin instance_id (control inputs, in order)
    pub params: HashMap<usize, Vec<f32>>,
    /// Model file per block instance_id, relative to the models directory
    pub files: HashMap<usize, String>,
    /// Colour name per block instance_id (rack drawing only)
    pub colors: HashMap<usize, String>,
    pub next_instance_id: usize,
    pub stereo_mode: bool,
    /// Scenes of the preset (see `state::scenes`); the chain above is the
    /// live state of scene `scene`
    pub scenes: Vec<Option<SceneSnapshot>>,
    pub scene: usize,
    /// Tempo, beats per minute
    pub bpm: f32,
    /// Block toggled by each footswitch in stomp mode (None: the n-th block)
    pub stomps: Vec<Option<usize>>,
    /// MIDI CCs learned by the blocks
    pub midi: Vec<MidiAssign>,
}

impl Default for EffectChainState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            params: HashMap::new(),
            files: HashMap::new(),
            colors: HashMap::new(),
            next_instance_id: 0,
            stereo_mode: false,
            scenes: Vec::new(),
            scene: 0,
            bpm: DEFAULT_BPM,
            stomps: Vec::new(),
            midi: Vec::new(),
        }
    }
}

impl EffectChainState {
    pub fn new() -> Self {
        Self::default()
    }

    fn new_id(&mut self) -> usize {
        let id = self.next_instance_id;
        self.next_instance_id += 1;
        id
    }

    fn plugin_node(&mut self, uri: &str, defaults: Vec<f32>) -> (usize, ChainNode) {
        let instance_id = self.new_id();
        self.params.insert(instance_id, defaults);
        let node = ChainNode::Plugin {
            instance_id,
            uri: uri.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        };
        (instance_id, node)
    }

    pub fn find(&self, instance_id: usize) -> Option<&ChainNode> {
        find_in(&self.nodes, instance_id)
    }

    /// Plugins in processing order (depth-first, top branch before bottom)
    pub fn plugins(&self) -> Vec<&ChainNode> {
        let mut out = Vec::new();
        collect_plugins(&self.nodes, &mut out);
        out
    }

    /// Number of plugins (splits not counted)
    pub fn plugin_count(&self) -> usize {
        self.plugins().len()
    }

    /// Append a plugin at the end of the main chain
    pub fn add_plugin(&mut self, uri: &str, defaults: Vec<f32>) -> usize {
        let (id, node) = self.plugin_node(uri, defaults);
        self.nodes.push(node);
        id
    }

    /// Append an empty split at the end of the main chain
    pub fn add_split(&mut self, split_type: SplitType) -> usize {
        let instance_id = self.new_id();
        self.nodes.push(ChainNode::Split {
            instance_id,
            split_type,
            enabled: true,
            top_chain: Vec::new(),
            bottom_chain: Vec::new(),
            ab_select_a: true,
            mix_ratio: 0.5,
            pan: [0.0, 0.0],
            num_inputs: 1,
            num_outputs: 1,
        });
        instance_id
    }

    /// Append a plugin to one branch of a split (any depth)
    pub fn add_to_split(
        &mut self,
        split_id: usize,
        top: bool,
        uri: &str,
        defaults: Vec<f32>,
    ) -> Result<usize> {
        if !matches!(self.find(split_id), Some(ChainNode::Split { .. })) {
            return Err(anyhow!("Node {} is not a split", split_id));
        }
        let (id, node) = self.plugin_node(uri, defaults);
        if let Some(ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        }) = find_in_mut(&mut self.nodes, split_id)
        {
            if top {
                top_chain.push(node)
            } else {
                bottom_chain.push(node)
            }
        }
        Ok(id)
    }

    /// Remove a node (and everything inside it, for a split)
    pub fn remove(&mut self, instance_id: usize) -> Result<()> {
        let removed = remove_in(&mut self.nodes, instance_id)
            .ok_or_else(|| anyhow!("Node {} not found", instance_id))?;
        let mut ids = Vec::new();
        collect_ids(std::slice::from_ref(&removed), &mut ids);
        for id in ids {
            self.params.remove(&id);
            self.files.remove(&id);
            self.colors.remove(&id);
        }
        Ok(())
    }

    /// Where a node is
    pub fn place_of(&self, instance_id: usize) -> Option<ChainPlace> {
        place_of(&self.nodes, instance_id, None)
    }

    /// Move a node (a split with its branches) to `place`, as counted
    /// before the move. Fails when `place` is inside the node itself.
    pub fn move_to(&mut self, instance_id: usize, mut place: ChainPlace) -> Result<()> {
        let from = self
            .place_of(instance_id)
            .ok_or_else(|| anyhow!("Node {} not found", instance_id))?;
        if let Some((split_id, _)) = place.parent {
            let mut inside = Vec::new();
            collect_ids(
                std::slice::from_ref(self.find(instance_id).unwrap()),
                &mut inside,
            );
            if inside.contains(&split_id) {
                return Err(anyhow!("Cannot move a split into itself"));
            }
        }
        let len = container_mut(&mut self.nodes, place.parent)
            .ok_or_else(|| anyhow!("No such branch"))?
            .len();
        if place.index > len {
            return Err(anyhow!("Position {} out of range", place.index));
        }
        if from.parent == place.parent && from.index < place.index {
            place.index -= 1;
        }
        let node = remove_in(&mut self.nodes, instance_id).unwrap();
        container_mut(&mut self.nodes, place.parent)
            .unwrap()
            .insert(place.index, node);
        Ok(())
    }

    pub fn is_enabled(&self, instance_id: usize) -> Option<bool> {
        match self.find(instance_id)? {
            ChainNode::Plugin { enabled, .. } | ChainNode::Split { enabled, .. } => Some(*enabled),
        }
    }

    pub fn set_enabled(&mut self, instance_id: usize, value: bool) -> Result<()> {
        match find_in_mut(&mut self.nodes, instance_id) {
            Some(ChainNode::Plugin { enabled, .. }) | Some(ChainNode::Split { enabled, .. }) => {
                *enabled = value;
                Ok(())
            }
            None => Err(anyhow!("Node {} not found", instance_id)),
        }
    }

    /// Flip the A/B selection of a split; returns the new `select_a`
    pub fn toggle_split_ab(&mut self, split_id: usize) -> Result<bool> {
        match find_in_mut(&mut self.nodes, split_id) {
            Some(ChainNode::Split { ab_select_a, .. }) => {
                *ab_select_a = !*ab_select_a;
                Ok(*ab_select_a)
            }
            _ => Err(anyhow!("Node {} is not a split", split_id)),
        }
    }

    /// Change what a split does with its branches (its blocks are kept)
    pub fn set_split_type(&mut self, split_id: usize, kind: SplitType) -> Result<()> {
        match find_in_mut(&mut self.nodes, split_id) {
            Some(ChainNode::Split { split_type, .. }) => {
                *split_type = kind;
                Ok(())
            }
            _ => Err(anyhow!("Node {} is not a split", split_id)),
        }
    }

    /// Set the top/bottom balance of a split; returns the clamped ratio
    pub fn set_split_mix(&mut self, split_id: usize, ratio: f32) -> Result<f32> {
        match find_in_mut(&mut self.nodes, split_id) {
            Some(ChainNode::Split { mix_ratio, .. }) => {
                *mix_ratio = ratio.clamp(0.0, 1.0);
                Ok(*mix_ratio)
            }
            _ => Err(anyhow!("Node {} is not a split", split_id)),
        }
    }

    /// Place the top and bottom branches of a split between left and right;
    /// returns the clamped pans and whether the split was panned before
    pub fn set_split_pan(&mut self, split_id: usize, value: [f32; 2]) -> Result<([f32; 2], bool)> {
        match find_in_mut(&mut self.nodes, split_id) {
            Some(ChainNode::Split {
                split_type, pan, ..
            }) => {
                let was = crate::plugins::chain::is_panned(*split_type, *pan);
                *pan = value.map(|p| p.clamp(-1.0, 1.0));
                Ok((*pan, was))
            }
            _ => Err(anyhow!("Node {} is not a split", split_id)),
        }
    }

    pub fn parameters(&self, instance_id: usize) -> &[f32] {
        self.params
            .get(&instance_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_parameter(&mut self, instance_id: usize, index: usize, value: f32) -> Result<()> {
        let values = self
            .params
            .get_mut(&instance_id)
            .ok_or_else(|| anyhow!("Plugin {} not found", instance_id))?;
        let slot = values
            .get_mut(index)
            .ok_or_else(|| anyhow!("Parameter {} out of range", index))?;
        *slot = value;
        Ok(())
    }

    pub fn to_preset_chain(&self) -> PresetChain {
        PresetChain {
            nodes: self.nodes.clone(),
            params: self.params.clone(),
            files: self.files.clone(),
            colors: self.colors.clone(),
            scenes: self.saved_scenes(),
            scene: self.scene,
            bpm: self.bpm,
            stomps: self.stomps.clone(),
            midi: self.saved_midi(),
        }
    }

    /// Rebuild from a saved chain; unknown parameter lists get the defaults
    /// of the plugin so that `params` always matches the plugin ports.
    pub fn from_preset_chain(chain: &PresetChain, host: &Lv2Host, stereo_mode: bool) -> Self {
        let mut state = Self {
            nodes: chain.nodes.clone(),
            params: HashMap::new(),
            files: HashMap::new(),
            colors: HashMap::new(),
            next_instance_id: 0,
            stereo_mode,
            scenes: chain.scenes.clone(),
            scene: chain.scene.min(super::scenes::SCENE_COUNT - 1),
            bpm: super::tempo::clamp_bpm(chain.bpm),
            stomps: chain.stomps.clone(),
            midi: chain.midi.clone(),
        };
        let mut ids = Vec::new();
        collect_ids(&state.nodes, &mut ids);
        state.next_instance_id = ids.iter().max().map_or(0, |m| m + 1);

        let plugins: Vec<(usize, String)> = state
            .plugins()
            .into_iter()
            .filter_map(|n| match n {
                ChainNode::Plugin {
                    instance_id, uri, ..
                } => Some((*instance_id, uri.clone())),
                _ => None,
            })
            .collect();
        for (id, uri) in plugins {
            let mut values = default_parameters(host, &uri);
            match chain.params.get(&id) {
                // Plugin not installed: keep its values for when it is back
                Some(saved) if values.is_empty() => values = saved.clone(),
                Some(saved) => {
                    for (value, saved) in values.iter_mut().zip(saved) {
                        *value = *saved;
                    }
                }
                None => {}
            }
            state.params.insert(id, values);
        }
        // Files and colours of the nodes that exist (splits can be coloured)
        let keep = |id: &usize| ids.contains(id);
        state.files = chain
            .files
            .iter()
            .filter(|(id, _)| keep(id))
            .map(|(id, f)| (*id, f.clone()))
            .collect();
        state.colors = chain
            .colors
            .iter()
            .filter(|(id, _)| keep(id))
            .map(|(id, c)| (*id, c.clone()))
            .collect();
        state
    }

    /// Rebuild from the flat plugin list of older presets
    pub fn from_plugin_configs(
        plugins: &[PluginConfig],
        host: &Lv2Host,
        stereo_mode: bool,
    ) -> Self {
        let mut state = Self {
            stereo_mode,
            ..Self::default()
        };
        for config in plugins {
            let mut values = default_parameters(host, &config.uri);
            if values.is_empty() {
                // Plugin not installed: keep its values for when it is back
                // (bounded: a damaged file must not ask for gigabytes)
                let count = config
                    .parameters
                    .keys()
                    .max()
                    .map_or(0, |m| m + 1)
                    .min(MAX_PARAMETERS);
                values = vec![0.0; count];
            }
            for (&index, &value) in &config.parameters {
                if let Some(slot) = values.get_mut(index) {
                    *slot = value;
                }
            }
            let id = state.add_plugin(&config.uri, values);
            let _ = state.set_enabled(id, !config.bypassed);
        }
        state
    }

    /// Flat plugin list, kept in presets for compatibility
    pub fn to_plugin_configs(&self, host: &Lv2Host) -> Vec<PluginConfig> {
        self.plugins()
            .into_iter()
            .filter_map(|node| match node {
                ChainNode::Plugin {
                    instance_id,
                    uri,
                    enabled,
                    ..
                } => {
                    let mut config = PluginConfig::new(
                        uri.clone(),
                        crate::plugins::lv2_host::plugin_name(host, uri),
                    );
                    config.bypassed = !enabled;
                    for (index, value) in self.parameters(*instance_id).iter().enumerate() {
                        config.set_parameter(index, *value);
                    }
                    Some(config)
                }
                _ => None,
            })
            .collect()
    }
}

/// The chain and settings in use, restored at start-up
const SESSION_FILE: &str = "session.json";
/// Session autosave: this long after the last change…
const AUTOSAVE_QUIET: Duration = Duration::from_secs(2);
/// …or after this long while changes go on
const AUTOSAVE_MAX_DELAY: Duration = Duration::from_secs(30);

/// Changes of the session saved, and when they happened
#[derive(Default)]
struct Autosave {
    saved: u64,
    seen: u64,
    last_change: Option<std::time::Instant>,
    waiting_since: Option<std::time::Instant>,
}

/// Parameters kept for a plugin that is not installed
const MAX_PARAMETERS: usize = 1024;

fn default_parameters(host: &Lv2Host, uri: &str) -> Vec<f32> {
    host.scanner()
        .find_by_uri(uri)
        .map(|info| info.default_parameters())
        .unwrap_or_default()
}

/// Link to the audio thread
#[derive(Default)]
struct AudioLink {
    control: Option<ChainControl>,
    /// Generation of the last spec sent
    generation: u64,
}

/// Global application state, shared by the UI callbacks
pub struct AppState {
    pub lv2_host: Arc<Lv2Host>,
    pub effect_chain: Arc<Mutex<EffectChainState>>,
    pub tuner: Arc<Mutex<Tuner>>,
    pub looper: Arc<Mutex<Looper>>,
    pub preset_manager: Arc<Mutex<PresetManager>>,
    pub scene_manager: Arc<Mutex<SceneManager>>,
    audio: Mutex<AudioLink>,
    data_dir: PathBuf,
    /// Session changes so far, and when to save them
    changes: AtomicU64,
    autosave: Mutex<Autosave>,
    session_file: crate::presets::BackgroundWriter,
    /// Tones already read, by block and directory, with the time their
    /// folder last changed (the rack shows them at every refresh)
    packs: Mutex<HashMap<(String, PathBuf), CachedPack>>,
    live: Mutex<LiveSettings>,
    midi: Mutex<MidiSettings>,
    tap: Mutex<TapTempo>,
}

/// What the footswitches do
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FootswitchMode {
    /// Switch n recalls scene n of the preset
    Scenes,
    /// Switch n turns a block on or off
    #[default]
    Stomp,
    /// Switch n loads preset n of the setlist
    Setlist,
}

impl FootswitchMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Scenes => "scenes",
            Self::Stomp => "stomp",
            Self::Setlist => "setlist",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Scenes, Self::Stomp, Self::Setlist]
            .into_iter()
            .find(|m| m.name() == name)
    }
}

/// Where a step through the presets lands
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetStep {
    /// Slot of the current setlist
    Slot(usize),
    /// Preset of the preset list
    Preset(String),
}

/// Footswitch settings, kept with the session
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiveSettings {
    #[serde(default)]
    pub mode: FootswitchMode,
    /// The last footswitch taps the tempo, and held, opens the tuner
    #[serde(default = "yes")]
    pub tap_switch: bool,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            mode: FootswitchMode::default(),
            tap_switch: true,
        }
    }
}

fn yes() -> bool {
    true
}

/// A tone read from disk, with the last change time of its folder
type CachedPack = (Option<std::time::SystemTime>, ModelPack);

/// Chain saved on exit and restored at startup
#[derive(serde::Serialize, serde::Deserialize)]
struct Session {
    chain: PresetChain,
    stereo_mode: bool,
    preset_id: Option<String>,
    scene_id: Option<String>,
    #[serde(default)]
    live: LiveSettings,
    #[serde(default)]
    midi: MidiSettings,
}

impl AppState {
    /// Create the state with presets and scenes stored in `data_dir()`
    pub fn new() -> Result<Self> {
        Self::with_host(Arc::new(Lv2Host::new()?), data_dir())
    }

    pub fn with_host(lv2_host: Arc<Lv2Host>, data_dir: PathBuf) -> Result<Self> {
        info!(
            "Initializing application state (data in {})",
            data_dir.display()
        );
        Ok(Self {
            lv2_host,
            effect_chain: Arc::new(Mutex::new(EffectChainState::new())),
            // Replaced with the JACK sample rate when the audio engine starts
            tuner: Arc::new(Mutex::new(Tuner::new(48000.0))),
            looper: Arc::new(Mutex::new(Looper::new(48000.0))),
            preset_manager: Arc::new(Mutex::new(PresetManager::new(data_dir.join("presets"))?)),
            scene_manager: Arc::new(Mutex::new(SceneManager::new(data_dir.join("scenes"))?)),
            audio: Mutex::new(AudioLink::default()),
            session_file: crate::presets::BackgroundWriter::new(data_dir.join(SESSION_FILE)),
            data_dir,
            changes: AtomicU64::new(0),
            autosave: Mutex::default(),
            packs: Mutex::default(),
            live: Mutex::default(),
            midi: Mutex::default(),
            tap: Mutex::default(),
        })
    }

    /// Where the files loaded by blocks (NAM models, impulse responses) are
    /// looked for
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    /// Files the block `uri` can load from the models directory
    /// (subdirectories included), as paths relative to it, sorted
    pub fn list_files(&self, uri: &str) -> Vec<String> {
        fn walk(dir: &std::path::Path, root: &std::path::Path, uri: &str, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, uri, out);
                } else if crate::plugins::scanner::accepts_file(uri, &path) {
                    if let Ok(relative) = path.strip_prefix(root) {
                        out.push(portable(relative));
                    }
                }
            }
        }
        let root = self.models_dir();
        let mut models = Vec::new();
        walk(&root, &root, uri, &mut models);
        models.sort_by_key(|m| m.to_lowercase());
        models
    }

    /// Models the block `uri` can load, grouped by tone (folder), sorted by
    /// title. TONE3000 tones of another gear than the block's are left out.
    pub fn list_packs(&self, uri: &str) -> Vec<ModelPack> {
        let gears = crate::plugins::scanner::file_block(uri).map_or(&[][..], |b| b.gears);
        let mut by_dir: Vec<(PathBuf, Vec<String>)> = Vec::new();
        for file in self.list_files(uri) {
            let dir = Path::new(&file)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            match by_dir
                .iter_mut()
                .find(|(d, _)| *d == dir && !dir.as_os_str().is_empty())
            {
                Some((_, files)) => files.push(file),
                None => by_dir.push((dir, vec![file])),
            }
        }
        let root = self.models_dir();
        let mut packs: Vec<ModelPack> = by_dir
            .into_iter()
            .map(|(dir, files)| ModelPack::new(&root, &dir, files))
            .filter(|p| p.gear.as_deref().is_none_or(|g| gears.contains(&g)))
            .collect();
        packs.sort_by_key(|p| p.title.to_lowercase());
        packs
    }

    /// The tone `file` belongs to, with its other presets
    pub fn model_pack(&self, uri: &str, file: &str) -> Option<ModelPack> {
        let dir = Path::new(file)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let files = if dir.as_os_str().is_empty() {
            vec![file.to_string()]
        } else {
            let root = self.models_dir();
            // A file added or removed, or tone.json rewritten, changes one
            // of these times
            let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let changed =
                modified(&root.join(&dir)).max(modified(&root.join(&dir).join("tone.json")));
            let key = (uri.to_string(), dir.clone());
            if let Some((time, pack)) = self.packs.lock().unwrap().get(&key) {
                if changed.is_some() && *time == changed {
                    return Some(pack.clone());
                }
            }
            let mut files: Vec<String> = std::fs::read_dir(root.join(&dir))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && crate::plugins::scanner::accepts_file(uri, p))
                .filter_map(|p| Some(portable(&dir.join(p.file_name()?))))
                .collect();
            files.sort_by_key(|f| f.to_lowercase());
            let pack = ModelPack::new(&root, &dir, files);
            self.packs
                .lock()
                .unwrap()
                .insert(key, (changed, pack.clone()));
            return Some(pack);
        };
        Some(ModelPack::new(&self.models_dir(), &dir, files))
    }

    fn session_path(&self) -> PathBuf {
        self.data_dir.join(SESSION_FILE)
    }

    /// Save the current chain so it comes back at the next start
    /// Save the session now (when quitting)
    pub fn save_session(&self) -> Result<()> {
        let changes = self.changes.load(Ordering::Relaxed);
        self.session_file.queue(self.session_json()?);
        self.session_file.flush()?;
        self.autosave.lock().unwrap().saved = changes;
        Ok(())
    }

    fn session_json(&self) -> Result<Vec<u8>> {
        let chain = self.chain();
        let session = Session {
            chain: chain.to_preset_chain(),
            stereo_mode: chain.stereo_mode,
            preset_id: self
                .preset_manager
                .lock()
                .unwrap()
                .current_preset()
                .map(|p| p.id.clone()),
            scene_id: self.current_scene_id(),
            live: self.live_settings(),
            midi: self.midi_settings(),
        };
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(serde_json::to_vec_pretty(&session)?)
    }

    /// Save the session in the background once changes settle (the
    /// pedalboard is usually switched off without quitting the app): a
    /// moment after the last one, and regularly while they go on (an
    /// expression pedal, a MIDI clock), sparing the SD card. Call it often.
    pub fn autosave(&self) -> Result<()> {
        let changes = self.changes.load(Ordering::Relaxed);
        let now = std::time::Instant::now();
        let mut autosave = self.autosave.lock().unwrap();
        if changes == autosave.saved {
            autosave.waiting_since = None;
            return Ok(());
        }
        if autosave.seen != changes {
            autosave.seen = changes;
            autosave.last_change = Some(now);
        }
        let since = |t: Option<std::time::Instant>| t.map_or(Duration::ZERO, |t| now - t);
        let waiting = since(Some(*autosave.waiting_since.get_or_insert(now)));
        if since(autosave.last_change) < AUTOSAVE_QUIET && waiting < AUTOSAVE_MAX_DELAY {
            return Ok(());
        }
        autosave.saved = changes;
        autosave.waiting_since = None;
        drop(autosave);
        self.session_file.queue(self.session_json()?);
        Ok(())
    }

    /// Restore the chain of the last session, if any
    pub fn restore_session(&self) -> Result<bool> {
        let path = self.session_path();
        if !path.exists() {
            return Ok(false);
        }
        let session: Session = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        *self.live.lock().unwrap() = session.live;
        let mut midi = session.midi;
        if midi.channel > 16 {
            midi.channel = crate::midi::OMNI;
        }
        *self.midi.lock().unwrap() = midi;
        if let Some(scene_id) = &session.scene_id {
            if let Err(e) = self.load_scene(scene_id) {
                warn!("Could not restore scene: {}", e);
            }
        }
        if let Some(preset_id) = &session.preset_id {
            // Only marks it as current; the chain below may contain unsaved edits
            let _ = self.preset_manager.lock().unwrap().load_preset(preset_id);
        }
        let state = EffectChainState::from_preset_chain(
            &session.chain,
            &self.lv2_host,
            session.stereo_mode,
        );
        self.edit(|c| {
            *c = state;
            Ok(())
        })?;
        self.autosave.lock().unwrap().saved = self.changes.load(Ordering::Relaxed);
        Ok(true)
    }

    /// Connect to the audio engine and send it the current chain
    pub fn set_audio_control(&self, control: ChainControl) {
        self.audio.lock().unwrap().control = Some(control);
        let chain = self.effect_chain.lock().unwrap();
        self.send_spec(&chain);
    }

    /// Next error reported by the audio side (plugin failing to load...)
    pub fn poll_audio_error(&self) -> Option<String> {
        let audio = self.audio.lock().unwrap();
        audio.control.as_ref()?.error_rx.try_recv().ok()
    }

    /// Send the whole chain to be rebuilt. Called with the chain locked so
    /// that specs and commands are sent in the order they were made.
    fn send_spec(&self, chain: &EffectChainState) {
        let mut audio = self.audio.lock().unwrap();
        audio.generation += 1;
        let spec = self.spec(chain, audio.generation);
        if let Some(control) = &audio.control {
            if control.spec_tx.send(spec).is_err() {
                warn!("Audio engine stopped; chain not sent");
            }
        }
    }

    fn spec(&self, chain: &EffectChainState, generation: u64) -> ChainSpec {
        ChainSpec {
            generation,
            nodes: chain.nodes.clone(),
            params: chain.params.clone(),
            files: chain
                .files
                .iter()
                .map(|(id, file)| (*id, self.models_dir().join(file)))
                .collect(),
            stereo: chain.stereo_mode,
            bpm: chain.bpm,
        }
    }

    /// Mono or stereo signal around each block of `chain`, as the audio
    /// chain routes it
    pub fn signal_map(&self, chain: &EffectChainState) -> SignalMap {
        SignalMap::new(&self.spec(chain, 0), &self.lv2_host)
    }

    /// Hand a MIDI message to the plugins that take MIDI, if it is for them
    /// (MIDI threads; never blocks)
    pub fn forward_midi(&self, bytes: &[u8]) {
        let settings = *self.midi.lock().unwrap();
        let Some(message) = crate::midi::for_plugins(bytes, &settings) else {
            return;
        };
        if let Some(control) = &self.audio.lock().unwrap().control {
            let _ = control.midi_tx.try_send(message);
        }
    }

    fn send_command(&self, command: ChainCommand) {
        let audio = self.audio.lock().unwrap();
        if let Some(control) = &audio.control {
            // Never block the UI: the queue only fills up if the audio
            // callback has stopped
            let _ = control.command_tx.try_send((audio.generation, command));
        }
    }

    /// Apply a structural edit and rebuild the audio chain
    fn edit<T>(&self, f: impl FnOnce(&mut EffectChainState) -> Result<T>) -> Result<T> {
        let mut chain = self.effect_chain.lock().unwrap();
        let result = f(&mut chain)?;
        self.send_spec(&chain);
        self.mark_dirty();
        Ok(result)
    }

    /// Apply a live edit and send the matching command
    fn tweak<T>(
        &self,
        f: impl FnOnce(&mut EffectChainState) -> Result<(T, ChainCommand)>,
    ) -> Result<T> {
        let mut chain = self.effect_chain.lock().unwrap();
        let (result, command) = f(&mut chain)?;
        self.send_command(command);
        self.mark_dirty();
        Ok(result)
    }

    /// The session changed: save it at a next autosave
    fn mark_dirty(&self) {
        self.changes.fetch_add(1, Ordering::Relaxed);
    }

    /// Parameter values of a new block: its defaults, at the current tempo
    fn defaults_for(&self, uri: &str) -> Result<Vec<f32>> {
        let info = self
            .lv2_host
            .scanner()
            .find_by_uri(uri)
            .ok_or_else(|| anyhow!("Plugin not found: {}", uri))?;
        let mut values = info.default_parameters();
        let bpm = self.effect_chain.lock().unwrap().bpm;
        for index in info.tempo_parameters() {
            if let Some(value) = values.get_mut(index) {
                *value = bpm;
            }
        }
        Ok(values)
    }

    /// Positions of the parameters of `uri` that follow the tempo
    fn tempo_parameters(&self, uri: &str) -> Vec<usize> {
        self.lv2_host
            .scanner()
            .find_by_uri(uri)
            .map(|info| info.tempo_parameters())
            .unwrap_or_default()
    }

    /// Snapshot of the chain for display
    pub fn chain(&self) -> EffectChainState {
        self.effect_chain.lock().unwrap().clone()
    }

    pub fn plugin_name(&self, uri: &str) -> String {
        crate::plugins::lv2_host::plugin_name(&self.lv2_host, uri)
    }

    pub fn plugin_category(&self, uri: &str) -> Option<crate::plugins::scanner::PluginCategory> {
        self.lv2_host
            .scanner()
            .find_by_uri(uri)
            .map(|info| info.category.clone())
    }

    // === Chain editing ===

    pub fn add_plugin(&self, uri: &str) -> Result<usize> {
        let defaults = self.defaults_for(uri)?;
        let id = self.edit(|c| Ok(c.add_plugin(uri, defaults)))?;
        info!("Added {} (#{})", self.plugin_name(uri), id);
        Ok(id)
    }

    pub fn add_split(&self, split_type: SplitType) -> Result<usize> {
        self.edit(|c| Ok(c.add_split(split_type)))
    }

    pub fn add_plugin_to_split(&self, split_id: usize, top: bool, uri: &str) -> Result<usize> {
        let defaults = self.defaults_for(uri)?;
        self.edit(|c| c.add_to_split(split_id, top, uri, defaults))
    }

    pub fn remove_node(&self, instance_id: usize) -> Result<()> {
        self.edit(|c| c.remove(instance_id))
    }

    /// Move a node anywhere in the chain (drag and drop in the rack)
    pub fn move_node_to(&self, instance_id: usize, place: ChainPlace) -> Result<()> {
        self.edit(|c| c.move_to(instance_id, place))
    }

    /// Colour of a block in the rack; None for the default look. Does not
    /// touch the audio chain.
    pub fn set_color(&self, instance_id: usize, color: Option<String>) {
        let mut chain = self.effect_chain.lock().unwrap();
        if chain.find(instance_id).is_none() {
            return;
        }
        match color {
            Some(color) => chain.colors.insert(instance_id, color),
            None => chain.colors.remove(&instance_id),
        };
        self.mark_dirty();
    }

    pub fn clear_chain(&self) {
        let _ = self.edit(|c| {
            let (stereo_mode, bpm) = (c.stereo_mode, c.bpm);
            *c = EffectChainState {
                stereo_mode,
                bpm,
                ..Default::default()
            };
            Ok(())
        });
    }

    /// Toggle bypass of a plugin or split; returns true if now bypassed
    pub fn toggle_bypass(&self, instance_id: usize) -> Result<bool> {
        self.tweak(|c| {
            let enabled = !c
                .is_enabled(instance_id)
                .ok_or_else(|| anyhow!("Node {} not found", instance_id))?;
            c.set_enabled(instance_id, enabled)?;
            Ok((
                !enabled,
                ChainCommand::SetEnabled {
                    instance_id,
                    enabled,
                },
            ))
        })
    }

    pub fn toggle_split_ab(&self, split_id: usize) -> Result<bool> {
        self.tweak(|c| {
            let select_a = c.toggle_split_ab(split_id)?;
            Ok((
                select_a,
                ChainCommand::SetSplitAb {
                    instance_id: split_id,
                    select_a,
                },
            ))
        })
    }

    pub fn set_split_type(&self, split_id: usize, kind: SplitType) -> Result<()> {
        self.edit(|c| c.set_split_type(split_id, kind))
    }

    pub fn set_split_mix(&self, split_id: usize, ratio: f32) -> Result<f32> {
        self.tweak(|c| {
            let ratio = c.set_split_mix(split_id, ratio)?;
            Ok((
                ratio,
                ChainCommand::SetSplitMix {
                    instance_id: split_id,
                    ratio,
                },
            ))
        })
    }

    /// Place the branches of a Mix split between left and right. Panning
    /// changes how the chain is routed (stereo after the split), so turning
    /// it on or off rebuilds the chain (returns true); moving it is a live
    /// command.
    pub fn set_split_pan(&self, split_id: usize, pan: [f32; 2]) -> Result<([f32; 2], bool)> {
        let mut chain = self.effect_chain.lock().unwrap();
        let (pan, was) = chain.set_split_pan(split_id, pan)?;
        let now = matches!(
            chain.find(split_id),
            Some(ChainNode::Split { split_type, .. })
                if crate::plugins::chain::is_panned(*split_type, pan)
        );
        let rebuild = chain.stereo_mode && was != now;
        if rebuild {
            self.send_spec(&chain);
        } else {
            self.send_command(ChainCommand::SetSplitPan {
                instance_id: split_id,
                pan,
            });
        }
        self.mark_dirty();
        Ok((pan, rebuild))
    }

    /// Model file of a block (relative to the models directory)
    pub fn plugin_file(&self, instance_id: usize) -> Option<String> {
        self.effect_chain
            .lock()
            .unwrap()
            .files
            .get(&instance_id)
            .cloned()
    }

    /// Choose the model of a block (None to unload it); rebuilds the chain
    /// since loading a model is not real-time safe
    pub fn set_plugin_file(&self, instance_id: usize, file: Option<String>) -> Result<()> {
        self.edit(|c| {
            match c.find(instance_id) {
                Some(ChainNode::Plugin { .. }) => {}
                _ => return Err(anyhow!("Plugin {} not found", instance_id)),
            }
            match file {
                Some(file) => c.files.insert(instance_id, file),
                None => c.files.remove(&instance_id),
            };
            Ok(())
        })
    }

    /// Set a plugin parameter (clamped to the port range); returns the value
    pub fn set_parameter(&self, instance_id: usize, index: usize, value: f32) -> Result<f32> {
        self.tweak(|c| {
            let value = match c.find(instance_id) {
                Some(ChainNode::Plugin { uri, .. }) => {
                    let port = self
                        .lv2_host
                        .scanner()
                        .find_by_uri(uri)
                        .and_then(|info| info.control_inputs().nth(index));
                    match port {
                        Some(p) => value.clamp(p.min.min(p.max), p.max.max(p.min)),
                        None => value,
                    }
                }
                _ => return Err(anyhow!("Plugin {} not found", instance_id)),
            };
            c.set_parameter(instance_id, index, value)?;
            Ok((
                value,
                ChainCommand::SetParameter {
                    instance_id,
                    index,
                    value,
                },
            ))
        })
    }

    /// Mono (both outputs play the same) or stereo outputs; rebuilds the
    /// audio chain
    pub fn set_stereo_mode(&self, stereo: bool) {
        let _ = self.edit(|c| {
            c.stereo_mode = stereo;
            Ok(())
        });
    }

    // === Scenes within the preset ===

    /// Recall scene `slot` of the preset, live (see `state::scenes`)
    pub fn select_scene(&self, slot: usize) -> Result<()> {
        let mut chain = self.effect_chain.lock().unwrap();
        let switch = chain.select_scene(slot)?;
        if switch.rebuild {
            self.send_spec(&chain);
        } else {
            for command in switch.commands {
                self.send_command(command);
            }
        }
        self.mark_dirty();
        Ok(())
    }

    pub fn rename_scene(&self, slot: usize, name: &str) -> Result<()> {
        self.effect_chain.lock().unwrap().rename_scene(slot, name)?;
        self.mark_dirty();
        Ok(())
    }

    // === Tempo ===

    pub fn tempo(&self) -> f32 {
        self.effect_chain.lock().unwrap().bpm
    }

    /// Set the tempo of the preset; returns it clamped
    pub fn set_tempo(&self, bpm: f32) -> f32 {
        let bpm = super::tempo::clamp_bpm(bpm);
        let mut chain = self.effect_chain.lock().unwrap();
        if chain.bpm != bpm {
            self.send_command(ChainCommand::SetTempo { bpm });
            for command in chain.apply_tempo(bpm, |uri| self.tempo_parameters(uri)) {
                self.send_command(command);
            }
            self.mark_dirty();
        }
        bpm
    }

    /// A tap of the tap tempo key or switch: the new tempo, once two taps
    /// are in
    pub fn tap_tempo(&self) -> Option<f32> {
        let bpm = self.tap.lock().unwrap().tap(std::time::Instant::now())?;
        Some(self.set_tempo(bpm.round()))
    }

    // === Footswitches ===

    pub fn live_settings(&self) -> LiveSettings {
        *self.live.lock().unwrap()
    }

    pub fn set_footswitch_mode(&self, mode: FootswitchMode) {
        self.live.lock().unwrap().mode = mode;
        self.mark_dirty();
    }

    pub fn set_tap_switch(&self, on: bool) {
        self.live.lock().unwrap().tap_switch = on;
        self.mark_dirty();
    }

    /// Block toggled by stomp switch `slot`: the one assigned to it, else
    /// the n-th block not assigned to another switch
    pub fn stomp_target(&self, slot: usize) -> Option<usize> {
        self.effect_chain.lock().unwrap().stomp_target(slot)
    }

    /// Assign a block to stomp switch `slot` (None: back to automatic)
    pub fn assign_stomp(&self, slot: usize, instance_id: Option<usize>) -> Result<()> {
        let mut chain = self.effect_chain.lock().unwrap();
        if let Some(id) = instance_id {
            if chain.find(id).is_none() {
                return Err(anyhow!("Node {} not found", id));
            }
        }
        if chain.stomps.len() <= slot {
            chain.stomps.resize(slot + 1, None);
        }
        chain.stomps[slot] = instance_id;
        while chain.stomps.last() == Some(&None) {
            chain.stomps.pop();
        }
        self.mark_dirty();
        Ok(())
    }

    /// Toggle the block of stomp switch `slot`: its id and whether it is now
    /// bypassed
    pub fn toggle_stomp(&self, slot: usize) -> Result<(usize, bool)> {
        let id = self
            .stomp_target(slot)
            .ok_or_else(|| anyhow!("No block on switch {}", slot + 1))?;
        Ok((id, self.toggle_bypass(id)?))
    }

    // === MIDI ===

    pub fn midi_settings(&self) -> MidiSettings {
        *self.midi.lock().unwrap()
    }

    /// Channel listened to: 1–16, or `midi::OMNI`
    pub fn set_midi_channel(&self, channel: u8) {
        self.midi.lock().unwrap().channel = channel.min(16);
        self.mark_dirty();
    }

    pub fn set_midi_clock(&self, on: bool) {
        self.midi.lock().unwrap().clock = on;
        self.mark_dirty();
    }

    /// Make a block target follow `controller`
    pub fn learn_midi(&self, controller: u8, instance_id: usize, target: MidiTarget) -> Result<()> {
        self.effect_chain
            .lock()
            .unwrap()
            .learn_midi(controller, instance_id, target)?;
        self.mark_dirty();
        Ok(())
    }

    /// Assignment of a target of a block, if any
    pub fn midi_assign(&self, instance_id: usize, target: MidiTarget) -> Option<MidiAssign> {
        self.effect_chain
            .lock()
            .unwrap()
            .midi_assign(instance_id, target)
    }

    /// Heel and toe (percent of the range) of a learned target
    pub fn set_midi_range(&self, instance_id: usize, target: MidiTarget, heel: u8, toe: u8) {
        if self
            .effect_chain
            .lock()
            .unwrap()
            .set_midi_range(instance_id, target, heel, toe)
        {
            self.mark_dirty();
        }
    }

    pub fn clear_midi(&self, instance_id: usize, target: MidiTarget) {
        if self
            .effect_chain
            .lock()
            .unwrap()
            .clear_midi(instance_id, target)
        {
            self.mark_dirty();
        }
    }

    /// Apply a learned CC to the targets following it; returns the targets
    /// that changed
    pub fn midi_controller(&self, controller: u8, value: u8) -> Vec<(usize, MidiTarget)> {
        let targets = self.effect_chain.lock().unwrap().midi_targets(controller);
        let mut changed = Vec::new();
        for assign in targets {
            let (instance_id, target) = (assign.instance_id, assign.target);
            let position = assign.position(value);
            let done = match target {
                MidiTarget::Bypass => self.set_block_enabled(instance_id, position >= 0.5),
                MidiTarget::Parameter(index) => self.midi_parameter(instance_id, index, position),
            };
            match done {
                Ok(true) => changed.push((instance_id, target)),
                Ok(false) => {}
                Err(e) => warn!("MIDI CC {}: {}", controller, e),
            }
        }
        changed
    }

    /// Turn a block on or off; returns whether it changed
    fn set_block_enabled(&self, instance_id: usize, enabled: bool) -> Result<bool> {
        let current = self
            .effect_chain
            .lock()
            .unwrap()
            .is_enabled(instance_id)
            .ok_or_else(|| anyhow!("Node {} not found", instance_id))?;
        if current == enabled {
            return Ok(false);
        }
        self.toggle_bypass(instance_id)?;
        Ok(true)
    }

    /// Set parameter `index` of a block from a CC value, over the range of
    /// the port; returns whether it changed
    /// Set a parameter to `position` (0–1) of its range; returns whether it
    /// changed
    fn midi_parameter(&self, instance_id: usize, index: usize, position: f32) -> Result<bool> {
        let (uri, current) = {
            let chain = self.effect_chain.lock().unwrap();
            let uri = match chain.find(instance_id) {
                Some(ChainNode::Plugin { uri, .. }) => uri.clone(),
                _ => return Err(anyhow!("Plugin {} not found", instance_id)),
            };
            let current = chain
                .params
                .get(&instance_id)
                .and_then(|p| p.get(index))
                .copied();
            (uri, current)
        };
        let port = self
            .lv2_host
            .scanner()
            .find_by_uri(&uri)
            .and_then(|info| info.control_inputs().nth(index).cloned())
            .ok_or_else(|| anyhow!("Parameter {} not found", index))?;
        let target = if port.toggled {
            if position >= 0.5 {
                port.max
            } else {
                port.min
            }
        } else if port.enumeration && !port.scale_points.is_empty() {
            let mut values: Vec<f32> = port.scale_points.iter().map(|(_, v)| *v).collect();
            values.sort_by(f32::total_cmp);
            let last = values.len() - 1;
            values[((position.clamp(0.0, 1.0) * last as f32).round() as usize).min(last)]
        } else {
            crate::midi::scale(
                position,
                port.min,
                port.max,
                port.integer || port.enumeration,
            )
        };
        if current == Some(target) {
            return Ok(false);
        }
        self.set_parameter(instance_id, index, target)?;
        Ok(true)
    }

    /// Leave the current setlist (Program Change then addresses the preset
    /// list)
    pub fn close_setlist(&self) {
        self.scene_manager.lock().unwrap().close_scene();
        self.mark_dirty();
    }

    /// Preset before or after the current one, in the setlist when one is
    /// loaded (its slot), else in the preset list (its id); wraps around
    pub fn preset_step(&self, step: Step) -> Option<PresetStep> {
        let next = |len: usize, from: Option<usize>| -> Option<usize> {
            if len == 0 {
                return None;
            }
            Some(match (from, step) {
                (None, Step::Next) => 0,
                (None, Step::Previous) => len - 1,
                (Some(i), Step::Next) => (i + 1) % len,
                (Some(i), Step::Previous) => (i + len - 1) % len,
            })
        };
        {
            let scenes = self.scene_manager.lock().unwrap();
            if scenes.current_scene().is_some() {
                let filled = scenes.playable_slots();
                let at = scenes
                    .active_slot()
                    .and_then(|s| filled.iter().position(|&f| f == s));
                return next(filled.len(), at).map(|i| PresetStep::Slot(filled[i]));
            }
        }
        let presets = self.list_presets();
        let current = self.current_preset_id();
        let at = presets.iter().position(|p| Some(&p.id) == current.as_ref());
        next(presets.len(), at).map(|i| PresetStep::Preset(presets[i].id.clone()))
    }

    // === Presets ===

    /// Save the current chain as a new preset
    pub fn save_preset(&self, name: String) -> Result<String> {
        let mut preset = Preset::new(name);
        self.store_chain(&mut preset);
        let id = self.preset_manager.lock().unwrap().add_preset(preset)?;
        info!("Saved preset {}", id);
        self.mark_dirty();
        Ok(id)
    }

    /// Save the current chain over the current preset, keeping its id (so
    /// its setlist slots and Program Change number); a new preset when
    /// none is current
    pub fn update_preset(&self) -> Result<String> {
        let Some(mut preset) = self
            .preset_manager
            .lock()
            .unwrap()
            .current_preset()
            .cloned()
        else {
            return self.save_preset("Untitled".into());
        };
        self.store_chain(&mut preset);
        preset.touch();
        // Lock order: presets, then scenes
        let mut presets = self.preset_manager.lock().unwrap();
        presets.save_preset(&preset)?;
        self.scene_manager.lock().unwrap().refresh_preset(&preset);
        info!("Updated preset {}", preset.id);
        self.mark_dirty();
        Ok(preset.id)
    }

    /// Put the current chain into `preset`
    fn store_chain(&self, preset: &mut Preset) {
        let chain = self.chain();
        preset.plugins = chain.to_plugin_configs(&self.lv2_host);
        preset.stereo_mode = chain.stereo_mode;
        preset.chain = Some(chain.to_preset_chain());
    }

    fn apply_preset(&self, preset: &Preset) {
        let state = match &preset.chain {
            Some(chain) => {
                EffectChainState::from_preset_chain(chain, &self.lv2_host, preset.stereo_mode)
            }
            None => EffectChainState::from_plugin_configs(
                &preset.plugins,
                &self.lv2_host,
                preset.stereo_mode,
            ),
        };
        let _ = self.edit(|c| {
            *c = state;
            Ok(())
        });
        info!("Applied preset '{}'", preset.name);
    }

    pub fn load_preset(&self, id: &str) -> Result<()> {
        let preset = self.preset_manager.lock().unwrap().load_preset(id)?;
        self.apply_preset(&preset);
        Ok(())
    }

    pub fn delete_preset(&self, id: &str) -> Result<()> {
        self.preset_manager.lock().unwrap().delete_preset(id)?;
        self.scene_manager.lock().unwrap().forget_preset(id)?;
        self.mark_dirty();
        Ok(())
    }

    pub fn list_presets(&self) -> Vec<Preset> {
        let manager = self.preset_manager.lock().unwrap();
        let mut presets: Vec<Preset> = manager.list_presets().into_iter().cloned().collect();
        presets.sort_by_key(|p| p.name.to_lowercase());
        presets
    }

    pub fn current_preset_id(&self) -> Option<String> {
        self.preset_manager
            .lock()
            .unwrap()
            .current_preset()
            .map(|p| p.id.clone())
    }

    /// Slot of the current setlist last switched to
    pub fn active_setlist_slot(&self) -> Option<usize> {
        self.scene_manager.lock().unwrap().active_slot()
    }

    pub fn current_preset_name(&self) -> String {
        self.preset_manager
            .lock()
            .unwrap()
            .current_preset()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Unsaved".to_string())
    }

    // === Scenes ===

    pub fn create_scene(&self, name: String) -> Result<String> {
        self.scene_manager.lock().unwrap().create_scene(name)
    }

    /// Make a scene current and preload all its presets in memory
    pub fn load_scene(&self, scene_id: &str) -> Result<()> {
        // Lock order: presets, then scenes
        let presets = self.preset_manager.lock().unwrap();
        self.scene_manager
            .lock()
            .unwrap()
            .load_scene(scene_id, |id| {
                presets
                    .get(id)
                    .cloned()
                    .ok_or_else(|| anyhow!("Preset not found: {}", id))
            })?;
        self.mark_dirty();
        Ok(())
    }

    /// Switch to a slot of the current scene (presets are preloaded).
    /// Returns false when the slot is empty.
    pub fn switch_to_scene_slot(&self, slot: usize) -> Result<bool> {
        let preset = self.scene_manager.lock().unwrap().switch_to_slot(slot)?;
        match preset {
            Some(preset) => {
                // The preset becomes the current one, as when loaded by hand
                let _ = self.preset_manager.lock().unwrap().load_preset(&preset.id);
                self.apply_preset(&preset);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn current_scene_id(&self) -> Option<String> {
        self.scene_manager
            .lock()
            .unwrap()
            .current_scene()
            .map(|s| s.id.clone())
    }

    /// Assign (or clear, with `None`) a slot of the current scene
    pub fn set_current_scene_slot(&self, slot: usize, preset_id: Option<String>) -> Result<()> {
        let scene_id = self
            .current_scene_id()
            .ok_or_else(|| anyhow!("No scene loaded"))?;
        self.update_scene_slot(&scene_id, slot, preset_id)?;
        // Reload so the new preset is preloaded
        self.load_scene(&scene_id)
    }

    pub fn update_scene_slot(
        &self,
        scene_id: &str,
        slot: usize,
        preset_id: Option<String>,
    ) -> Result<()> {
        self.scene_manager
            .lock()
            .unwrap()
            .update_scene_slot(scene_id, slot, preset_id)
    }

    pub fn list_scenes(&self) -> Vec<crate::presets::Scene> {
        let manager = self.scene_manager.lock().unwrap();
        let mut scenes: Vec<_> = manager.list_scenes().into_iter().cloned().collect();
        scenes.sort_by_key(|s| s.name.to_lowercase());
        scenes
    }

    pub fn delete_scene(&self, scene_id: &str) -> Result<()> {
        self.scene_manager.lock().unwrap().delete_scene(scene_id)?;
        self.mark_dirty();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OD: &str = "urn:pedalboard:demo:overdrive";
    const DELAY: &str = "urn:pedalboard:demo:delay";

    fn state() -> (AppState, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pedalboard-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        (state, dir)
    }

    #[test]
    fn edits_nested_splits_by_id() {
        let mut chain = EffectChainState::new();
        let od = chain.add_plugin(OD, vec![0.5]);
        let split = chain.add_split(SplitType::Ab);
        let a = chain.add_to_split(split, true, DELAY, vec![]).unwrap();
        let b = chain.add_to_split(split, false, OD, vec![0.1]).unwrap();
        assert!(chain.add_to_split(od, true, DELAY, vec![]).is_err());

        let ids: Vec<usize> = chain.plugins().into_iter().map(node_id).collect();
        assert_eq!(ids, vec![od, a, b]);
        assert!(!chain.toggle_split_ab(split).unwrap());
        assert_eq!(chain.set_split_mix(split, 3.0).unwrap(), 1.0);

        chain.remove(split).unwrap();
        assert_eq!(chain.plugin_count(), 1);
        assert!(!chain.params.contains_key(&b));
    }

    #[test]
    fn audio_receives_specs_and_commands() {
        let (state, dir) = state();
        let (spec_tx, spec_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (_error_tx, error_rx) = crossbeam_channel::unbounded();
        let (midi_tx, _midi_rx) = crossbeam_channel::unbounded();
        state.set_audio_control(ChainControl {
            spec_tx,
            command_tx,
            midi_tx,
            error_rx,
        });
        assert_eq!(spec_rx.try_recv().unwrap().generation, 1);

        let id = state.add_plugin(OD).unwrap();
        let spec = spec_rx.try_recv().unwrap();
        assert_eq!((spec.generation, spec.nodes.len()), (2, 1));
        assert_eq!(spec.params[&id].len(), 5);

        // Clamped to the port range
        assert_eq!(state.set_parameter(id, 0, 9.0).unwrap(), 1.0);
        assert!(state.toggle_bypass(id).unwrap());
        assert_eq!(
            command_rx.try_iter().collect::<Vec<_>>(),
            vec![
                (
                    2,
                    ChainCommand::SetParameter {
                        instance_id: id,
                        index: 0,
                        value: 1.0
                    }
                ),
                (
                    2,
                    ChainCommand::SetEnabled {
                        instance_id: id,
                        enabled: false
                    }
                ),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nodes_move_anywhere_in_the_tree() {
        let (state, _dir) = state();
        let uri = OD;
        let a = state.add_plugin(uri).unwrap();
        let b = state.add_plugin(uri).unwrap();
        let split = state.add_split(SplitType::Mix).unwrap();
        let c = state.add_plugin_to_split(split, true, uri).unwrap();
        let ids = |nodes: &[ChainNode]| nodes.iter().map(node_id).collect::<Vec<_>>();
        let main = |index| ChainPlace {
            parent: None,
            index,
        };

        // Forward in the same chain: the index counts the node itself
        state.move_node_to(a, main(2)).unwrap();
        assert_eq!(ids(&state.chain().nodes), [b, a, split]);
        // Into the empty bottom branch, then back out
        state
            .move_node_to(
                b,
                ChainPlace {
                    parent: Some((split, false)),
                    index: 0,
                },
            )
            .unwrap();
        let chain = state.chain();
        assert_eq!(ids(&chain.nodes), [a, split]);
        assert_eq!(
            chain.place_of(b),
            Some(ChainPlace {
                parent: Some((split, false)),
                index: 0
            })
        );
        state.move_node_to(c, main(0)).unwrap();
        assert_eq!(ids(&state.chain().nodes), [c, a, split]);
        // A split never goes into itself
        assert!(state
            .move_node_to(
                split,
                ChainPlace {
                    parent: Some((split, true)),
                    index: 0
                }
            )
            .is_err());
        // Colours follow the block and go with it
        state.set_color(a, Some("red".into()));
        state.remove_node(a).unwrap();
        assert!(state.chain().colors.is_empty());
    }

    #[test]
    fn models_are_grouped_by_tone() {
        use crate::plugins::scanner::{NAM_AMP_CAB_URI, NAM_PEDAL_URI, NAM_URI};
        let (state, dir) = state();
        let models = state.models_dir();
        let boss = "Tone3000/Pedals/Boss MT-2 [96210]";
        let plexi = "Tone3000/Amps/Plexi [1]";
        let combo = "Tone3000/Amps + cabs/AC30 [2]";
        for d in [boss, plexi, combo, "MyRig"] {
            std::fs::create_dir_all(models.join(d)).unwrap();
        }
        let info = |title: &str, gear: &str| {
            format!(r#"{{"title":"{title}","creator":"bob","gear":"{gear}"}}"#)
        };
        std::fs::write(
            models.join(boss).join("tone.json"),
            info("Boss Metal Zone MT-2", "pedal"),
        )
        .unwrap();
        std::fs::write(models.join(plexi).join("tone.json"), info("Plexi", "amp")).unwrap();
        std::fs::write(
            models.join(combo).join("tone.json"),
            info("AC30", "amp-cab"),
        )
        .unwrap();
        for file in [
            format!("{boss}/B.nam"),
            format!("{boss}/A.nam"),
            format!("{plexi}/Crunch.nam"),
            format!("{combo}/Top Boost.nam"),
            "mine.nam".into(),
            "MyRig/clean.nam".into(),
        ] {
            std::fs::write(models.join(file), "{}").unwrap();
        }

        let titles =
            |uri| -> Vec<String> { state.list_packs(uri).into_iter().map(|p| p.title).collect() };
        // Tones of another gear are left out; the user's own files are everywhere
        assert_eq!(titles(NAM_URI), ["mine", "MyRig", "Plexi"]);
        assert_eq!(titles(NAM_AMP_CAB_URI), ["AC30", "mine", "MyRig"]);
        assert_eq!(
            titles(NAM_PEDAL_URI),
            ["Boss Metal Zone MT-2", "mine", "MyRig"]
        );

        let pack = state
            .model_pack(NAM_PEDAL_URI, &format!("{boss}/B.nam"))
            .unwrap();
        assert_eq!(pack.creator.as_deref(), Some("bob"));
        assert_eq!(
            pack.files,
            [format!("{boss}/A.nam"), format!("{boss}/B.nam")]
        );
        assert_eq!(ModelPack::preset_name(&pack.files[0]), "A");
        assert_eq!(
            state.model_pack(NAM_URI, "MyRig/clean.nam").unwrap().title,
            "MyRig"
        );

        let labels = |files: &[&str]| {
            ModelPack {
                title: String::new(),
                creator: None,
                gear: None,
                files: files.iter().map(|f| f.to_string()).collect(),
            }
            .preset_labels()
        };
        assert_eq!(
            labels(&["a/Boss MT-2 - All Noon.nam", "a/Boss MT-2 - Doom Metal.nam"]),
            ["All Noon", "Doom Metal"]
        );
        assert_eq!(
            labels(&[
                "a/Peavey 5150 Blackletter Red.nam",
                "a/Peavey 5150 Blockletter Red.nam"
            ]),
            ["Blackletter Red", "Blockletter Red"]
        );
        assert_eq!(
            labels(&["a/TF V30 - 57.wav", "a/TF V30 545 3.wav"]),
            ["- 57", "545 3"].map(|l| l.trim_start_matches([' ', '-']))
        );
        let short = |title: &str| {
            ModelPack {
                title: title.into(),
                creator: None,
                gear: None,
                files: vec![],
            }
            .short_title()
        };
        assert_eq!(
            short("Mesa Boogie 2x12 V30 - XR IR PACK"),
            "Mesa Boogie 2x12 V30"
        );
        assert_eq!(short("Fortin 3.33 (2026)"), "Fortin 3.33");
        assert_eq!(short("Boss SD-1"), "Boss SD-1");
        // Nothing left once trimmed: keep the full names
        assert_eq!(labels(&["a/Red.nam", "a/Red 2.nam"]), ["Red", "Red 2"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn model_files_are_saved_and_cleaned() {
        let (state, dir) = state();
        std::fs::create_dir_all(state.models_dir().join("Tone3000")).unwrap();
        for name in ["Tone3000/Plexi.nam", "b.json", "cab.wav"] {
            std::fs::write(state.models_dir().join(name), "{}").unwrap();
        }
        assert_eq!(
            state.list_files(crate::plugins::scanner::NAM_URI),
            vec!["b.json", "Tone3000/Plexi.nam"]
        );
        assert_eq!(
            state.list_files(crate::plugins::scanner::IR_URI),
            vec!["cab.wav"]
        );

        let amp = state.add_plugin(OD).unwrap();
        state
            .set_plugin_file(amp, Some("Tone3000/Plexi.nam".into()))
            .unwrap();
        let id = state.save_preset("Amp".into()).unwrap();
        state.save_session().unwrap();

        state.clear_chain();
        state.load_preset(&id).unwrap();
        assert_eq!(
            state.plugin_file(amp).as_deref(),
            Some("Tone3000/Plexi.nam")
        );
        state.clear_chain();
        assert!(state.restore_session().unwrap());
        assert_eq!(
            state.plugin_file(amp).as_deref(),
            Some("Tone3000/Plexi.nam")
        );

        state.remove_node(amp).unwrap();
        assert!(state.chain().files.is_empty());
        assert!(state.set_plugin_file(amp, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn presets_round_trip_the_whole_chain() {
        let (state, dir) = state();
        let split = state.add_split(SplitType::Mix).unwrap();
        let od = state.add_plugin_to_split(split, false, OD).unwrap();
        state.set_parameter(od, 0, 0.9).unwrap();
        state.set_split_mix(split, 0.25).unwrap();
        let id = state.save_preset("Lead".into()).unwrap();

        state.clear_chain();
        assert_eq!(state.chain().nodes.len(), 0);

        state.load_preset(&id).unwrap();
        let chain = state.chain();
        assert_eq!(chain.parameters(od)[0], 0.9);
        match chain.find(split) {
            Some(ChainNode::Split {
                mix_ratio,
                bottom_chain,
                ..
            }) => {
                assert_eq!(*mix_ratio, 0.25);
                assert_eq!(bottom_chain.len(), 1);
            }
            other => panic!("unexpected {:?}", other),
        }
        // New ids never collide with loaded ones
        assert!(state.add_plugin(DELAY).unwrap() > od);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn session_is_restored() {
        let (state, dir) = state();
        let id = state.add_plugin(OD).unwrap();
        state.set_parameter(id, 1, 0.3).unwrap();
        // Not right after a change: once changes settle
        state.autosave().unwrap();
        assert!(!dir.join("session.json").exists());
        state.save_session().unwrap();
        assert!(dir.join("session.json").exists());

        let restored = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        assert!(restored.restore_session().unwrap());
        assert_eq!(restored.chain().parameters(id)[1], 0.3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_presets_load_from_flat_list() {
        let host = Lv2Host::demo();
        let mut config = PluginConfig::new(OD.into(), "Overdrive".into());
        config.bypassed = true;
        config.set_parameter(1, 0.2);
        let chain = EffectChainState::from_plugin_configs(&[config], &host, false);
        assert_eq!(chain.is_enabled(0), Some(false));
        assert_eq!(chain.parameters(0)[1], 0.2);
        assert_eq!(chain.parameters(0)[0], 0.5);
    }

    #[test]
    fn session_keeps_pan_stereo_and_split_colours() {
        let (state, dir) = state();
        state.set_stereo_mode(true);
        let split = state.add_split(SplitType::Mix).unwrap();
        assert!(state.set_split_pan(split, [-1.0, 0.5]).unwrap().1);
        // Moving a pan that stays off centre is a live change
        assert!(!state.set_split_pan(split, [-0.5, 0.5]).unwrap().1);
        state.set_color(split, Some("red".into()));
        state.set_color(99, Some("red".into())); // no such block
        state.save_session().unwrap();

        let restored = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        assert!(restored.restore_session().unwrap());
        let chain = restored.chain();
        assert!(chain.stereo_mode);
        assert!(matches!(
            chain.find(split),
            Some(ChainNode::Split { pan, .. }) if *pan == [-0.5, 0.5]
        ));
        assert_eq!(chain.colors.get(&split).map(String::as_str), Some("red"));
        assert_eq!(chain.colors.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_plugins_keep_their_values() {
        let host = Lv2Host::demo();
        let mut chain = EffectChainState::new();
        let id = chain.add_plugin("urn:not-installed", vec![0.1, 0.2]);
        let restored = EffectChainState::from_preset_chain(&chain.to_preset_chain(), &host, false);
        assert_eq!(restored.parameters(id), &[0.1, 0.2]);

        let mut config = PluginConfig::new("urn:not-installed".into(), "Gone".into());
        config.set_parameter(2, 0.7);
        let chain = EffectChainState::from_plugin_configs(&[config], &host, false);
        assert_eq!(chain.parameters(0), &[0.0, 0.0, 0.7]);
    }

    #[test]
    fn presets_with_the_same_name_are_kept_apart() {
        let (state, dir) = state();
        let a = state.save_preset("Untitled".into()).unwrap();
        state.add_plugin(OD).unwrap();
        let b = state.save_preset("Untitled".into()).unwrap();
        state.delete_preset(&a).unwrap();

        let restored = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        let ids: Vec<_> = restored.list_presets().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![b]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scene_switch_makes_the_preset_current() {
        let (state, dir) = state();
        state.add_plugin(OD).unwrap();
        let clean = state.save_preset("Clean".into()).unwrap();
        state.add_plugin(DELAY).unwrap();
        state.save_preset("Lead".into()).unwrap();
        let scene = state.create_scene("Gig".into()).unwrap();
        state.load_scene(&scene).unwrap();
        state.set_current_scene_slot(0, Some(clean)).unwrap();

        assert!(state.switch_to_scene_slot(0).unwrap());
        assert_eq!(state.current_preset_name(), "Clean");
        assert_eq!(state.chain().plugins().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_updates_the_current_preset_and_its_setlist_slot() {
        let (state, dir) = state();
        let clean = state.save_preset("Clean".into()).unwrap();
        let scene = state.create_scene("Gig".into()).unwrap();
        state.load_scene(&scene).unwrap();
        state
            .set_current_scene_slot(0, Some(clean.clone()))
            .unwrap();

        state.add_plugin(OD).unwrap();
        assert_eq!(state.update_preset().unwrap(), clean);
        assert_eq!(state.list_presets().len(), 1);
        // The setlist plays the new version, without reloading it
        state.add_plugin(DELAY).unwrap();
        assert!(state.switch_to_scene_slot(0).unwrap());
        assert_eq!(state.chain().plugins().len(), 1);

        let restored = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        restored.load_preset(&clean).unwrap();
        assert_eq!(restored.chain().plugins().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleted_presets_leave_their_setlists() {
        let (state, dir) = state();
        let clean = state.save_preset("Clean".into()).unwrap();
        let lead = state.save_preset("Lead".into()).unwrap();
        let scene = state.create_scene("Gig".into()).unwrap();
        state.load_scene(&scene).unwrap();
        state
            .set_current_scene_slot(0, Some(clean.clone()))
            .unwrap();
        state.set_current_scene_slot(1, Some(lead)).unwrap();
        assert!(state.switch_to_scene_slot(1).unwrap());

        state.delete_preset(&clean).unwrap();
        for step in [Step::Next, Step::Previous] {
            assert!(matches!(state.preset_step(step), Some(PresetStep::Slot(1))));
        }
        let restored = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        let slots = &restored.list_scenes()[0].preset_slots;
        assert_eq!(slots[0], None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tone3000_ids_are_not_shown() {
        assert_eq!(
            ModelPack::preset_name("Amps/Twin [42]/Clean [7].nam"),
            "Clean"
        );
        assert_eq!(
            ModelPack::preset_name("Amps/Clean [bright].nam"),
            "Clean [bright]"
        );
        assert_eq!(without_id("Boss MT-2 [96210]"), "Boss MT-2");
        assert_eq!(without_id("Mix []"), "Mix []");
    }
}
