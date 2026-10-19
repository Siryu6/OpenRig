use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

use crate::audio::ChainNode;
use crate::midi::MidiAssign;

/// Parameter values of one block by LV2 port symbol. Unlike positions,
/// symbols stay the same when a plugin update reorders or adds ports.
pub type SymbolValues = BTreeMap<String, f32>;

/// Preset - Complete pedalboard configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_at: u64, // Unix timestamp
    pub modified_at: u64,

    /// Plugin chain configuration
    pub plugins: Vec<PluginConfig>,

    /// Global settings
    pub stereo_mode: bool,

    /// Tags for organization
    pub tags: Vec<String>,

    /// Full chain (splits included) with parameter values. Older presets
    /// only have the flat `plugins` list.
    #[serde(default)]
    pub chain: Option<PresetChain>,
}

/// Chain structure saved in a preset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetChain {
    pub nodes: Vec<ChainNode>,
    /// Parameter values per plugin instance_id, by position (control
    /// inputs in port order). Still written for older versions of the app
    /// (an A/B update rolled back); `params_by_symbol` wins when present.
    pub params: HashMap<usize, Vec<f32>>,
    /// Parameter values per plugin instance_id, by port symbol
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params_by_symbol: HashMap<usize, SymbolValues>,
    /// Model file per block instance_id, relative to the models directory
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<usize, String>,
    /// Colour name per block instance_id, for the rack drawing
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub colors: HashMap<usize, String>,
    /// Scenes of the preset (None: slot never used); `nodes` and `params`
    /// hold the state of the current one
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<Option<SceneSnapshot>>,
    #[serde(default)]
    pub scene: usize,
    /// Tempo of the preset, for delays and modulations synced to it
    #[serde(default = "default_bpm")]
    pub bpm: f32,
    /// Block toggled by each footswitch in stomp mode
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stomps: Vec<Option<usize>>,
    /// MIDI CCs learned by the blocks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub midi: Vec<SavedMidiAssign>,
}

/// A learned MIDI CC as saved: a parameter target is also named by its
/// port symbol, which wins over the position when present
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedMidiAssign {
    #[serde(flatten)]
    pub assign: MidiAssign,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

impl Default for PresetChain {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            params: HashMap::new(),
            params_by_symbol: HashMap::new(),
            files: HashMap::new(),
            colors: HashMap::new(),
            scenes: Vec::new(),
            scene: 0,
            bpm: DEFAULT_BPM,
            stomps: Vec::new(),
            midi: Vec::new(),
        }
    }
}

pub const DEFAULT_BPM: f32 = 120.0;

fn default_bpm() -> f32 {
    DEFAULT_BPM
}

/// A scene: what changes between the parts of a song within one preset
/// (blocks on or off, knob positions, split settings), switched without
/// rebuilding the chain so that nothing drops out
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneSnapshot {
    pub name: String,
    /// On / off of every block and split, by instance_id
    pub enabled: HashMap<usize, bool>,
    /// Parameter values by instance_id, by position (as in `PresetChain`)
    pub params: HashMap<usize, Vec<f32>>,
    /// Parameter values by instance_id, by port symbol. In memory, only
    /// for blocks whose plugin is not installed (kept as loaded).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params_by_symbol: HashMap<usize, SymbolValues>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub splits: HashMap<usize, SplitSnapshot>,
}

/// Live settings of a split in a scene
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SplitSnapshot {
    pub select_a: bool,
    pub mix: f32,
    pub pan: [f32; 2],
}

/// Individual plugin configuration in chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub uri: String,
    pub name: String,
    pub bypassed: bool,
    pub routing_mode: String, // legacy flat-list field, always "Serial"

    /// Plugin parameter values (index -> value)
    pub parameters: HashMap<usize, f32>,
}

impl Preset {
    pub fn new(name: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description: String::new(),
            created_at: now,
            modified_at: now,
            plugins: Vec::new(),
            stereo_mode: false,
            tags: Vec::new(),
            chain: None,
        }
    }

    pub fn touch(&mut self) {
        self.modified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }
}

impl PluginConfig {
    pub fn new(uri: String, name: String) -> Self {
        Self {
            uri,
            name,
            bypassed: false,
            routing_mode: "Serial".to_string(),
            parameters: HashMap::new(),
        }
    }

    pub fn set_parameter(&mut self, index: usize, value: f32) {
        self.parameters.insert(index, value);
    }
}
