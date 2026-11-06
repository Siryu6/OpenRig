//! Read-only, typed view of the preset carried by a rig. The preset stays
//! JSON in [`Rig`](crate::Rig); this view reads the fields a site or a
//! list needs to show the chain, and ignores the rest. It never fails:
//! missing or odd fields read as empty, unknown nodes as [`Block::Unknown`].

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::deps::{Dependencies, Source};

/// Splits nested deeper than this are not read (shown as `Unknown`)
const READ_DEPTH: usize = 32;

/// The chain of a rig's preset
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChainView {
    /// Name of the preset (the rig's own name is in `Meta`)
    pub preset_name: String,
    /// Stereo processing
    pub stereo: bool,
    /// Blocks in signal order
    pub blocks: Vec<Block>,
    /// Scene slots: `None` for a slot never used, else the scene name
    pub scenes: Vec<Option<String>>,
    /// Slot of the scene the preset opens on
    pub current_scene: usize,
    /// Tempo, beats per minute
    pub bpm: f32,
    /// Footswitch slots in stomp mode: instance id of the block toggled
    pub stomps: Vec<Option<u64>>,
    /// Learned MIDI CCs carried by the preset
    pub midi_mappings: usize,
}

/// A node of the chain
// Plugins are most of the nodes: boxing them would only add allocations
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// A plugin block (built-in or LV2)
    Plugin(PluginBlock),
    /// A split into two branches
    Split(SplitBlock),
    /// A node this reader does not know (newer app) or nested too deep
    Unknown,
}

/// A plugin block
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PluginBlock {
    /// Id of the block in the preset (keys of its parameters and files)
    pub instance_id: u64,
    /// Plugin URI
    pub uri: String,
    /// Plugin name from the manifest, else the end of the URI
    pub name: String,
    /// Where the plugin comes from, from the manifest
    pub source: Option<Source>,
    /// Not bypassed
    pub enabled: bool,
    /// Audio inputs (1 mono, 2 stereo)
    pub inputs: u32,
    /// Audio outputs
    pub outputs: u32,
    /// Model or impulse response loaded by a built-in block
    pub file: Option<String>,
    /// Files of an LV2 plugin: parameter URI to path
    pub param_files: BTreeMap<String, String>,
    /// Colour name of the block in the rack
    pub color: Option<String>,
}

/// How a split combines its branches
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitKind {
    /// One branch heard at a time
    Ab,
    /// Both branches blended
    Mix,
    /// Top branch left, bottom branch right
    Lr,
    /// A kind of a newer app
    Other(String),
}

/// A split
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SplitBlock {
    /// Id of the split in the preset
    pub instance_id: u64,
    /// How the branches are combined
    pub kind: SplitKind,
    /// Not bypassed
    pub enabled: bool,
    /// Branch A
    pub top: Vec<Block>,
    /// Branch B
    pub bottom: Vec<Block>,
    /// A/B: branch A selected
    pub select_a: bool,
    /// Mix: 1.0 all A, 0.0 all B
    pub mix: f32,
}

/// A plugin block with its place in the chain
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlacedBlock<'a> {
    /// `/`-separated place: top-level blocks are numbered from 1, a block
    /// inside a split adds the branch and its number in it: `"3"`,
    /// `"2/A/1"`, `"2/B/2/A/1"`
    pub location: String,
    /// Splits around it
    pub depth: usize,
    /// The block
    pub block: &'a PluginBlock,
}

/// A file loaded by a block of the preset
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRef {
    /// Block loading it
    pub instance_id: u64,
    /// Parameter URI for a file of an LV2 plugin, `None` for the model of a
    /// built-in block
    pub param: Option<String>,
    /// Path as written in the preset, relative to the models directory
    pub path: String,
}

impl ChainView {
    pub(crate) fn read(preset: &Value, deps: &Dependencies) -> Self {
        let chain = preset.get("chain").filter(|c| c.is_object());
        let mut view = ChainView {
            preset_name: text(preset.get("name")),
            stereo: preset
                .get("stereo_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            bpm: 120.0,
            ..Self::default()
        };
        let Some(chain) = chain else {
            // Presets of the first versions: a flat list of plugins
            view.blocks = list(preset.get("plugins"))
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let uri = text(p.get("uri"));
                    Block::Plugin(PluginBlock {
                        instance_id: i as u64,
                        name: name_of(&uri, deps, &text(p.get("name"))),
                        source: deps.plugin(&uri).map(|d| d.source.clone()),
                        enabled: !p.get("bypassed").and_then(Value::as_bool).unwrap_or(false),
                        inputs: 1,
                        outputs: 1,
                        uri,
                        ..PluginBlock::default()
                    })
                })
                .collect();
            return view;
        };
        let files = by_id(chain.get("files"));
        let param_files = by_id(chain.get("param_files"));
        let colors = by_id(chain.get("colors"));
        let context = Context {
            deps,
            files: &files,
            param_files: &param_files,
            colors: &colors,
        };
        view.blocks = nodes(list(chain.get("nodes")), &context, 0);
        view.scenes = list(chain.get("scenes"))
            .iter()
            .map(|s| s.is_object().then(|| text(s.get("name"))))
            .collect();
        view.current_scene = chain
            .get("scene")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(0);
        if let Some(bpm) = chain.get("bpm").and_then(Value::as_f64) {
            view.bpm = bpm as f32;
        }
        view.stomps = list(chain.get("stomps"))
            .iter()
            .map(Value::as_u64)
            .collect();
        view.midi_mappings = list(chain.get("midi")).len();
        view
    }

    /// Every plugin block, in signal order (top branch before bottom)
    pub fn plugins(&self) -> Vec<PlacedBlock<'_>> {
        let mut out = Vec::new();
        walk(&self.blocks, "", 0, &mut out);
        out
    }

    /// Plugin blocks and splits, all levels together
    pub fn node_count(&self) -> usize {
        fn count(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|b| match b {
                    Block::Split(s) => 1 + count(&s.top) + count(&s.bottom),
                    _ => 1,
                })
                .sum()
        }
        count(&self.blocks)
    }

    /// Deepest nesting of splits (0: no split)
    pub fn depth(&self) -> usize {
        fn depth(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|b| match b {
                    Block::Split(s) => 1 + depth(&s.top).max(depth(&s.bottom)),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        depth(&self.blocks)
    }

    /// Whether some node could not be read
    pub fn has_unknown(&self) -> bool {
        fn any(blocks: &[Block]) -> bool {
            blocks.iter().any(|b| match b {
                Block::Unknown => true,
                Block::Split(s) => any(&s.top) || any(&s.bottom),
                Block::Plugin(_) => false,
            })
        }
        any(&self.blocks)
    }
}

fn walk<'a>(blocks: &'a [Block], prefix: &str, depth: usize, out: &mut Vec<PlacedBlock<'a>>) {
    for (i, block) in blocks.iter().enumerate() {
        let location = format!("{prefix}{}", i + 1);
        match block {
            Block::Plugin(p) => out.push(PlacedBlock {
                location,
                depth,
                block: p,
            }),
            Block::Split(s) => {
                walk(&s.top, &format!("{location}/A/"), depth + 1, out);
                walk(&s.bottom, &format!("{location}/B/"), depth + 1, out);
            }
            Block::Unknown => {}
        }
    }
}

struct Context<'a> {
    deps: &'a Dependencies,
    files: &'a BTreeMap<u64, &'a Value>,
    param_files: &'a BTreeMap<u64, &'a Value>,
    colors: &'a BTreeMap<u64, &'a Value>,
}

fn nodes(values: &[Value], context: &Context, depth: usize) -> Vec<Block> {
    values.iter().map(|v| node(v, context, depth)).collect()
}

fn node(value: &Value, context: &Context, depth: usize) -> Block {
    if let Some(p) = value.get("Plugin") {
        let instance_id = number(p.get("instance_id"));
        let uri = text(p.get("uri"));
        let param_files = context
            .param_files
            .get(&instance_id)
            .and_then(|m| m.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        return Block::Plugin(PluginBlock {
            instance_id,
            name: name_of(&uri, context.deps, ""),
            source: context.deps.plugin(&uri).map(|d| d.source.clone()),
            enabled: p.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            inputs: number(p.get("num_inputs")).try_into().unwrap_or(1),
            outputs: number(p.get("num_outputs")).try_into().unwrap_or(1),
            file: context
                .files
                .get(&instance_id)
                .and_then(|v| v.as_str())
                .map(str::to_string),
            param_files,
            color: context
                .colors
                .get(&instance_id)
                .and_then(|v| v.as_str())
                .map(str::to_string),
            uri,
        });
    }
    if let Some(s) = value.get("Split") {
        if depth >= READ_DEPTH {
            return Block::Unknown;
        }
        let kind = match s.get("split_type").and_then(Value::as_str).unwrap_or("Ab") {
            "Ab" => SplitKind::Ab,
            "Mix" => SplitKind::Mix,
            "Lr" => SplitKind::Lr,
            other => SplitKind::Other(other.to_string()),
        };
        return Block::Split(SplitBlock {
            instance_id: number(s.get("instance_id")),
            kind,
            enabled: s.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            top: nodes(list(s.get("top_chain")), context, depth + 1),
            bottom: nodes(list(s.get("bottom_chain")), context, depth + 1),
            select_a: s
                .get("ab_select_a")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            mix: s.get("mix_ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32,
        });
    }
    Block::Unknown
}

/// Name of plugin `uri`: the manifest's, else `fallback`, else the end of
/// the URI
fn name_of(uri: &str, deps: &Dependencies, fallback: &str) -> String {
    if let Some(name) = deps.plugin(uri).map(|d| d.name.as_str()) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if !fallback.is_empty() {
        return fallback.to_string();
    }
    uri.rsplit(['/', '#', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(uri)
        .to_string()
}

pub(crate) fn file_refs(preset: &Value) -> Vec<FileRef> {
    let Some(chain) = preset.get("chain") else {
        return Vec::new();
    };
    let mut refs: Vec<FileRef> = by_id(chain.get("files"))
        .into_iter()
        .filter_map(|(id, v)| {
            Some(FileRef {
                instance_id: id,
                param: None,
                path: v.as_str()?.to_string(),
            })
        })
        .collect();
    for (id, files) in by_id(chain.get("param_files")) {
        for (param, path) in files.as_object().into_iter().flatten() {
            if let Some(path) = path.as_str() {
                refs.push(FileRef {
                    instance_id: id,
                    param: Some(param.clone()),
                    path: path.to_string(),
                });
            }
        }
    }
    refs
}

/// A JSON object keyed by instance id (`{"3": …}`); other keys are skipped
fn by_id(value: Option<&Value>) -> BTreeMap<u64, &Value> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v)))
        .collect()
}

fn list(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}
