//! Plain view models built from `AppState`, converted to Slint structs by
//! the binary (the generated Slint types only exist there).

use chrono::{Local, TimeZone};

use crate::audio::ChainNode;
use crate::monitor::Levels;
use crate::plugins::scanner::{self, PluginCategory, PortInfo};
use crate::state::AppState;

/// One entry of the plugin browser
#[derive(Clone, Debug)]
pub struct PluginView {
    pub uri: String,
    pub name: String,
    pub author: String,
    pub category: String,
    /// Built-in gear drawn in the rack ("amp", "cab"…), "" otherwise
    pub kind: String,
    /// Built-in blocks: what to load in it
    pub hint: String,
    /// Colour of its family (RGB), as drawn in the rack
    pub color: u32,
}

/// How a parameter is edited
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterKind {
    Float,
    Int,
    Toggle,
    Enum,
}

impl ParameterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ParameterKind::Float => "float",
            ParameterKind::Int => "int",
            ParameterKind::Toggle => "toggle",
            ParameterKind::Enum => "enum",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParameterView {
    pub index: usize,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub unit: String,
    pub kind: ParameterKind,
    pub options: Vec<String>,
    pub option_values: Vec<f32>,
    /// Value shown as "Off"
    pub off_value: Option<f32>,
}

impl ParameterView {
    fn new(index: usize, port: &PortInfo, value: f32) -> Self {
        let kind = if port.toggled {
            ParameterKind::Toggle
        } else if port.enumeration && !port.scale_points.is_empty() {
            ParameterKind::Enum
        } else if port.integer {
            ParameterKind::Int
        } else {
            ParameterKind::Float
        };
        Self {
            index,
            name: port.name.clone(),
            min: port.min,
            max: port.max,
            default: port.default,
            value,
            unit: port.unit.clone().unwrap_or_default(),
            kind,
            options: port
                .scale_points
                .iter()
                .map(|(label, _)| label.clone())
                .collect(),
            option_values: port.scale_points.iter().map(|(_, v)| *v).collect(),
            off_value: port.off_value,
        }
    }
}

/// The node shown in the editor panel
#[derive(Clone, Debug, Default)]
pub struct SelectedNodeView {
    pub instance_id: usize,
    pub name: String,
    pub category: String,
    pub bypassed: bool,
    pub parameters: Vec<ParameterView>,
    /// The block loads a model file chosen in the editor
    pub uses_file: bool,
    /// Current model, relative to the models directory ("" if none)
    pub file: String,
    /// What the file is ("Model", "Impulse response")
    pub file_label: String,
    /// Accepted extensions, for the empty list hint (".nam, .json")
    pub file_types: String,
    /// Colour of the block when the user has not chosen one (RGB)
    pub auto_color: u32,
}

#[derive(Clone, Debug)]
pub struct PresetView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub plugin_count: usize,
    pub created: String,
    pub modified: String,
    pub timestamp: u64,
}

#[derive(Clone, Debug)]
pub struct SceneView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub preset_count: usize,
}

#[derive(Clone, Debug)]
pub struct SceneSlotView {
    pub slot: usize,
    pub preset_id: String,
    pub preset_name: String,
    pub is_empty: bool,
    /// Slot last switched to (by a footswitch or on screen)
    pub active: bool,
}

fn format_date(timestamp: u64) -> String {
    Local
        .timestamp_opt(timestamp as i64, 0)
        .single()
        .map(|d| d.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_default()
}

/// Available plugins sorted by category then name
pub fn plugin_views(state: &AppState) -> Vec<PluginView> {
    let mut plugins: Vec<PluginView> = state
        .lv2_host
        .scanner()
        .plugins()
        .values()
        .map(|info| PluginView {
            uri: info.uri.clone(),
            name: info.name.clone(),
            author: info.author.clone(),
            category: format!("{:?}", info.category),
            kind: scanner::shape(&info.uri).to_string(),
            hint: scanner::hint(&info.uri).to_string(),
            color: default_block_color(scanner::shape(&info.uri), Some(&info.category)),
        })
        .collect();
    // Built-in blocks first: they are what the pedalboard is made for; the
    // send and return blocks last, few rigs need them
    let builtin = |uri: &str| {
        let others = scanner::FILE_BLOCKS.len();
        match scanner::FILE_BLOCKS.iter().position(|b| b.uri == uri) {
            Some(i) => i,
            None if scanner::is_routing(uri) => others + 1,
            None => others,
        }
    };
    plugins.sort_by(|a, b| {
        (builtin(&a.uri), &a.category, a.name.to_lowercase()).cmp(&(
            builtin(&b.uri),
            &b.category,
            b.name.to_lowercase(),
        ))
    });
    plugins
}

/// True if the model file holds several sizes (see
/// `nam::file_has_quality`), cached while the file is unchanged
fn has_quality(path: &std::path::Path) -> bool {
    #[cfg(feature = "nam")]
    {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::time::SystemTime;
        static KNOWN: Mutex<Option<HashMap<std::path::PathBuf, (SystemTime, bool)>>> =
            Mutex::new(None);
        let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            return false;
        };
        let mut known = KNOWN.lock().unwrap();
        let known = known.get_or_insert_with(HashMap::new);
        if let Some(&(time, quality)) = known.get(path) {
            if time == modified {
                return quality;
            }
        }
        let quality = crate::plugins::nam::file_has_quality(path);
        known.insert(path.to_path_buf(), (modified, quality));
        quality
    }
    #[cfg(not(feature = "nam"))]
    {
        let _ = path;
        false
    }
}

/// Editor content for a node; `None` if it no longer exists
pub fn selected_node_view(state: &AppState, instance_id: usize) -> Option<SelectedNodeView> {
    let chain = state.effect_chain.lock().unwrap();
    match chain.find(instance_id)? {
        ChainNode::Plugin { uri, enabled, .. } => {
            let info = state.lv2_host.scanner().find_by_uri(uri);
            let values = chain.parameters(instance_id);
            let file = chain.files.get(&instance_id);
            // Quality only does something on a model holding several sizes
            let quality = file.is_some_and(|f| has_quality(&state.models_dir().join(f)));
            let parameters = info
                .map(|info| {
                    info.control_inputs()
                        .enumerate()
                        .filter(|(_, port)| quality || port.name != scanner::NAM_QUALITY)
                        .map(|(i, port)| {
                            ParameterView::new(
                                i,
                                port,
                                values.get(i).copied().unwrap_or(port.default),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(SelectedNodeView {
                instance_id,
                name: state.plugin_name(uri),
                category: info
                    .map(|i| format!("{:?}", i.category))
                    .unwrap_or_default(),
                bypassed: !enabled,
                parameters,
                uses_file: scanner::uses_file(uri),
                file: chain.files.get(&instance_id).cloned().unwrap_or_default(),
                file_label: scanner::file_block(uri)
                    .map_or("Model", |b| b.file_label)
                    .to_string(),
                file_types: scanner::file_extensions(uri)
                    .iter()
                    .map(|e| format!(".{e}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                auto_color: default_block_color(scanner::shape(uri), info.map(|i| &i.category)),
            })
        }
        ChainNode::Split {
            split_type,
            enabled,
            ..
        } => Some(SelectedNodeView {
            instance_id,
            name: split_type.label().to_string(),
            category: "Split".to_string(),
            bypassed: !enabled,
            parameters: Vec::new(),
            uses_file: false,
            file: String::new(),
            file_label: String::new(),
            file_types: String::new(),
            auto_color: default_block_color("", None),
        }),
    }
}

pub fn preset_views(state: &AppState) -> Vec<PresetView> {
    state
        .list_presets()
        .into_iter()
        .map(|p| PresetView {
            plugin_count: p
                .chain
                .as_ref()
                .map(|c| {
                    let mut count = 0;
                    count_plugins(&c.nodes, &mut count);
                    count
                })
                .unwrap_or(p.plugins.len()),
            created: format_date(p.created_at),
            modified: format_date(p.modified_at),
            timestamp: p.modified_at,
            id: p.id,
            name: p.name,
            description: p.description,
        })
        .collect()
}

fn count_plugins(nodes: &[ChainNode], count: &mut usize) {
    for node in nodes {
        match node {
            ChainNode::Plugin { .. } => *count += 1,
            ChainNode::Split {
                top_chain,
                bottom_chain,
                ..
            } => {
                count_plugins(top_chain, count);
                count_plugins(bottom_chain, count);
            }
        }
    }
}

pub fn scene_views(state: &AppState) -> Vec<SceneView> {
    state
        .list_scenes()
        .into_iter()
        .map(|s| SceneView {
            preset_count: s.preset_slots.iter().flatten().count(),
            id: s.id,
            name: s.name,
            description: s.description,
        })
        .collect()
}

/// Slots of the current scene (empty if no scene is loaded)
pub fn scene_slot_views(state: &AppState) -> Vec<SceneSlotView> {
    let (slots, active) = {
        let scenes = state.scene_manager.lock().unwrap();
        match scenes.current_scene() {
            Some(scene) => (scene.preset_slots.clone(), scenes.active_slot()),
            None => return Vec::new(),
        }
    };
    let presets = state.list_presets();
    slots
        .into_iter()
        .enumerate()
        .map(|(slot, preset_id)| SceneSlotView {
            slot,
            preset_name: preset_id
                .as_ref()
                .map(|id| {
                    presets
                        .iter()
                        .find(|p| &p.id == id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "Missing preset".to_string())
                })
                .unwrap_or_default(),
            is_empty: preset_id.is_none(),
            active: active == Some(slot) && preset_id.is_some(),
            preset_id: preset_id.unwrap_or_default(),
        })
        .collect()
}

/// Block colours offered in the editor: (name saved in presets, label, RGB).
/// Mid tones, so white text stays readable on them.
pub const BLOCK_COLORS: [(&str, &str, u32); 10] = [
    ("", "Auto", 0x23262d),
    ("red", "Red", 0x9a3a33),
    ("orange", "Orange", 0xa35a2c),
    ("yellow", "Yellow", 0x9a8136),
    ("green", "Green", 0x4e7c4a),
    ("teal", "Teal", 0x30706e),
    ("blue", "Blue", 0x3e5a87),
    ("purple", "Purple", 0x644f86),
    ("pink", "Pink", 0x8c4a64),
    ("silver", "Silver", 0x696c72),
];

/// Colour of a block the user has not coloured: its family, like the
/// category colours of hardware modelers (amps red, delays green...)
pub fn default_block_color(shape: &str, category: Option<&PluginCategory>) -> u32 {
    let name = match (shape, category) {
        ("amp" | "combo", _) => "red",
        ("cab", _) => "orange",
        ("pedal", _) | (_, Some(PluginCategory::Distortion)) => "yellow",
        ("space", _) | (_, Some(PluginCategory::Reverb)) => "teal",
        (_, Some(PluginCategory::Simulator)) => "red",
        (_, Some(PluginCategory::Delay)) => "green",
        (_, Some(PluginCategory::Modulation)) => "blue",
        (_, Some(PluginCategory::Filter)) => "purple",
        (_, Some(PluginCategory::Dynamics)) => "pink",
        _ => "silver",
    };
    block_color(name).unwrap_or(BLOCK_COLORS[0].2)
}

/// RGB of a colour name; None for the default look or an unknown name
pub fn block_color(name: &str) -> Option<u32> {
    BLOCK_COLORS
        .iter()
        .skip(1)
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, rgb)| *rgb)
}

/// Bottom of the level meters, dBFS
pub const METER_FLOOR_DB: f32 = -48.0;
/// Fall speed of the meters
const METER_FALL_DB_PER_S: f32 = 24.0;
/// Peak at which a clip LED lights, and for how long
const CLIP_PEAK: f32 = 0.99;
const CLIP_HOLD_S: f32 = 1.5;

/// The level meters as shown: instant rise, slow fall, latched clip LEDs
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeterView {
    /// Input, left and right output, 0..1 over [METER_FLOOR_DB, 0 dBFS]
    pub bars: [f32; 3],
    pub clip_in: bool,
    pub clip_out: bool,
    clip_in_left: f32,
    clip_out_left: f32,
}

/// LED segments per bar, as in ui/components/level_meter.slint
const METER_SEGMENTS: usize = 24;

impl MeterView {
    /// What the display shows: lit segments per bar and clip LEDs. The UI
    /// is only updated when this changes, since every update repaints the
    /// whole screen.
    pub fn lit(&self) -> ([usize; 3], bool, bool) {
        let segments = |bar: f32| {
            (0..METER_SEGMENTS)
                .filter(|&k| bar * METER_SEGMENTS as f32 > k as f32)
                .count()
        };
        (self.bars.map(segments), self.clip_in, self.clip_out)
    }

    /// Fold the peaks of the last `dt` seconds in
    pub fn update(&mut self, levels: Levels, dt: f32) {
        let fall = METER_FALL_DB_PER_S * dt / -METER_FLOOR_DB;
        for (bar, peak) in self
            .bars
            .iter_mut()
            .zip([levels.input, levels.left, levels.right])
        {
            *bar = meter_position(peak).max(*bar - fall).max(0.0);
        }
        let hold = |left: &mut f32, peak: f32| {
            *left = if peak >= CLIP_PEAK {
                CLIP_HOLD_S
            } else {
                (*left - dt).max(0.0)
            };
            *left > 0.0
        };
        self.clip_in = hold(&mut self.clip_in_left, levels.input);
        self.clip_out = hold(&mut self.clip_out_left, levels.left.max(levels.right));
    }
}

/// Position of a peak amplitude on a meter: 0 at METER_FLOOR_DB, 1 at 0 dBFS
pub fn meter_position(peak: f32) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    (1.0 - 20.0 * peak.log10() / METER_FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_rise_at_once_and_fall_slowly() {
        assert_eq!(meter_position(0.0), 0.0);
        assert_eq!(meter_position(1.0), 1.0);
        assert!((meter_position(10f32.powf(-24.0 / 20.0)) - 0.5).abs() < 1e-4);

        let mut view = MeterView::default();
        let loud = Levels {
            input: 1.0,
            left: 0.5,
            right: 0.0,
        };
        view.update(loud, 0.05);
        assert_eq!(view.bars[0], 1.0);
        assert!(view.clip_in && !view.clip_out);
        view.update(Levels::default(), 0.05);
        // 24 dB/s over 48 dB: 2.5 % per 50 ms
        assert!((view.bars[0] - 0.975).abs() < 1e-4);
        // The clip LED stays lit for a while
        assert!(view.clip_in);
        for _ in 0..40 {
            view.update(Levels::default(), 0.05);
        }
        assert!(!view.clip_in && view.bars == [0.0; 3]);
    }
    use crate::plugins::Lv2Host;
    use std::sync::Arc;

    #[test]
    fn blocks_take_the_colour_of_their_family() {
        let color = |name| block_color(name).unwrap();
        assert_eq!(default_block_color("amp", None), color("red"));
        assert_eq!(default_block_color("cab", None), color("orange"));
        assert_eq!(
            default_block_color("", Some(&PluginCategory::Delay)),
            color("green")
        );
        assert_eq!(
            default_block_color("", Some(&PluginCategory::Unknown)),
            color("silver")
        );
        assert_eq!(default_block_color("", None), color("silver"));
    }

    #[test]
    fn editor_exposes_parameter_kinds() {
        let dir = std::env::temp_dir().join(format!("pedalboard-view-{}", uuid::Uuid::new_v4()));
        let state = AppState::with_host(Arc::new(Lv2Host::demo()), dir.clone()).unwrap();
        let id = state.add_plugin("urn:pedalboard:demo:overdrive").unwrap();
        state.set_parameter(id, 0, 0.8).unwrap();

        let view = selected_node_view(&state, id).unwrap();
        let kinds: Vec<_> = view.parameters.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ParameterKind::Float,
                ParameterKind::Float,
                ParameterKind::Float,
                ParameterKind::Enum,
                ParameterKind::Toggle
            ]
        );
        assert_eq!(view.parameters[0].value, 0.8);
        assert_eq!(view.parameters[3].options.len(), 3);
        assert!(selected_node_view(&state, 99).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
