//! Preset to rig: every plugin and file of the chain is resolved to where
//! another pedalboard can get it.

use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
use std::path::Path;

use super::format::{
    is_safe_relative_path, BlockDisplay, Dependencies, FileDependency, Meta, PluginDependency,
    RackDisplay, Report, Rig, Source, Tone3000Model,
};
use super::CatalogPlugin;
use crate::plugins::scanner::{self, PluginCategory};
use crate::presets::Preset;
use crate::tone_info::ToneInfo;
use crate::ui::slint_bridge::rack_color;

/// What the exporting pedalboard knows
pub struct Context<'a> {
    /// The models directory: preset paths are relative to it
    pub models_dir: &'a Path,
    /// The plugins of the store catalog
    pub catalog: &'a [CatalogPlugin],
    /// Display name of an installed plugin
    pub plugin_name: &'a dyn Fn(&str) -> Option<String>,
    /// Family and LV2 class of an installed plugin
    pub plugin_class: &'a dyn Fn(&str) -> Option<(PluginCategory, String)>,
    /// Who exports it, as the user set it once (may be empty)
    pub author: String,
    /// Version of this app
    pub app_version: &'a str,
    /// Export time, RFC 3339 UTC (empty when unknown)
    pub created: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Keep the MIDI CCs learned by the blocks (they fit one controller
    /// setup, so they are left out by default)
    pub include_midi: bool,
}

/// A rig, with what keeps it from being shared
#[derive(Debug, Clone)]
pub struct Exported {
    pub rig: Rig,
    pub report: Report,
    /// Files left out because their path leaves the models directory
    pub dropped: Vec<String>,
}

impl Exported {
    /// Plugins and files only this pedalboard has, by name
    pub fn local_items(&self) -> Vec<String> {
        let deps = &self.rig.dependencies;
        let plugins = deps.plugins.iter().filter_map(|p| match &p.source {
            Source::Local { name, .. } => Some(name.clone()),
            _ => None,
        });
        let files = deps.files.iter().filter_map(|f| match &f.source {
            Source::Local { name, .. } => Some(name.clone()),
            _ => None,
        });
        plugins.chain(files).collect()
    }
}

/// Build the rig of `preset`
pub fn export(preset: &Preset, context: &Context, options: Options) -> Result<Exported> {
    let mut value = serde_json::to_value(preset).context("Preset not serializable")?;
    let mut dropped = Vec::new();
    if let Some(chain) = value.get_mut("chain").and_then(|c| c.as_object_mut()) {
        if !options.include_midi {
            chain.remove("midi");
        }
        dropped = drop_unsafe_paths(chain);
    }
    if let Some(preset) = value.as_object_mut() {
        // Kept for exports to come; the manifest below carries them
        preset.remove(RIG_MODELS_FIELD);
    }
    let meta = Meta {
        name: preset.name.trim().chars().take(80).collect(),
        description: preset.description.chars().take(4000).collect(),
        tags: preset
            .tags
            .iter()
            .map(|t| t.trim().to_lowercase().chars().take(32).collect::<String>())
            .filter(|t| !t.is_empty())
            .take(16)
            .collect(),
        author: context
            .author
            .trim()
            .chars()
            .take(super::format::limits::SHORT_TEXT)
            .collect(),
        app_version: context.app_version.to_string(),
        created: context.created.clone(),
    };
    let mut rig = Rig::new(meta, value, Dependencies::default());
    if rig.meta.name.is_empty() {
        rig.meta.name = "Untitled".into();
    }

    let view = rig.chain();
    let mut plugins: Vec<PluginDependency> = Vec::new();
    for placed in view.plugins() {
        let uri = &placed.block.uri;
        if plugins.iter().any(|p| &p.uri == uri) {
            continue;
        }
        plugins.push(resolve_plugin(uri, context));
    }

    let mut files: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for file in rig.file_refs() {
        let blocks = files.entry(file.path).or_default();
        if !blocks.contains(&file.instance_id) {
            blocks.push(file.instance_id);
        }
    }
    let files = files
        .into_iter()
        .map(|(path, blocks)| FileDependency {
            source: resolve_file(&path, context.models_dir)
                .or_else(|| {
                    preset
                        .rig_models
                        .get(&path)
                        .map(|m| Source::Tone3000(m.clone()))
                })
                .unwrap_or_else(|| local_file(&path)),
            path,
            blocks,
        })
        .collect();

    rig.dependencies = Dependencies { plugins, files };
    rig.display = display(&rig, context);
    let report = rig.validate();
    Ok(Exported {
        rig,
        report,
        dropped,
    })
}

/// Remove the files whose path is not a plain relative one (they could not
/// be found again elsewhere); returns them
fn drop_unsafe_paths(chain: &mut serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut dropped = Vec::new();
    let mut keep = |path: &serde_json::Value| match path.as_str() {
        Some(p) if is_safe_relative_path(p) => true,
        other => {
            dropped.push(other.unwrap_or_default().to_string());
            false
        }
    };
    if let Some(files) = chain.get_mut("files").and_then(|f| f.as_object_mut()) {
        files.retain(|_, path| keep(path));
    }
    if let Some(blocks) = chain.get_mut("param_files").and_then(|f| f.as_object_mut()) {
        for files in blocks.values_mut() {
            if let Some(files) = files.as_object_mut() {
                files.retain(|_, path| keep(path));
            }
        }
    }
    dropped
}

/// Kind of a built-in block: the end of its URI
fn builtin_kind(uri: &str) -> Option<&str> {
    if scanner::file_block(uri).is_none() && !scanner::is_routing(uri) {
        return None;
    }
    uri.strip_prefix("urn:pedalboard:")
}

fn resolve_plugin(uri: &str, context: &Context) -> PluginDependency {
    let name = (context.plugin_name)(uri)
        .or_else(|| scanner::file_block(uri).map(|b| b.name.to_string()))
        .unwrap_or_else(|| {
            uri.rsplit(['/', '#', ':'])
                .next()
                .unwrap_or(uri)
                .to_string()
        });
    let source = if let Some(kind) = builtin_kind(uri) {
        Source::Builtin { kind: kind.into() }
    } else if let Some(entry) = context.catalog.iter().find(|c| c.uri == uri) {
        Source::Store {
            package: entry.package.clone(),
            version: entry
                .installed_version
                .clone()
                .unwrap_or_else(|| entry.version.clone()),
            plugin_id: entry.plugin_id.clone(),
        }
    } else {
        Source::Local {
            what: "plugin".into(),
            name: name.clone(),
        }
    };
    PluginDependency {
        uri: uri.to_string(),
        name,
        source,
    }
}

/// How the rack draws each block, by the app's own rules
fn display(rig: &Rig, context: &Context) -> RackDisplay {
    let view = rig.chain();
    let blocks = view
        .plugins()
        .iter()
        .map(|placed| {
            let block = placed.block;
            let shape = scanner::shape(&block.uri);
            let (category, class) = (context.plugin_class)(&block.uri).unzip();
            let (color, rgb) = rack_color(block.color.as_deref(), shape, category.as_ref());
            let look = BlockDisplay {
                kind: if shape.is_empty() { "plugin" } else { shape }.to_string(),
                category: category
                    .as_ref()
                    .and_then(category_name)
                    .unwrap_or_default()
                    .to_string(),
                lv2_class: class.unwrap_or_default(),
                color: color.to_string(),
                color_hex: format!("#{rgb:06x}"),
            };
            (block.instance_id.to_string(), look)
        })
        .collect();
    RackDisplay { blocks }
}

/// Name of a plugin family in rigs; none for an unknown one
fn category_name(category: &PluginCategory) -> Option<&'static str> {
    Some(match category {
        PluginCategory::Dynamics => "dynamics",
        PluginCategory::Distortion => "distortion",
        PluginCategory::Filter => "filter",
        PluginCategory::Modulation => "modulation",
        PluginCategory::Delay => "delay",
        PluginCategory::Reverb => "reverb",
        PluginCategory::Simulator => "simulator",
        PluginCategory::Utility => "utility",
        PluginCategory::Generator => "generator",
        PluginCategory::Routing => "routing",
        PluginCategory::Unknown => return None,
    })
}

/// Preset field holding the TONE3000 source of the models a rig import
/// could not get, by path (see `Preset::rig_models`)
const RIG_MODELS_FIELD: &str = "rig_models";

/// A TONE3000 model downloaded here: its tone and model ids from the
/// `tone.json` of its folder
fn resolve_file(path: &str, models_dir: &Path) -> Option<Source> {
    let relative = Path::new(path);
    let file = relative
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let folder = models_dir.join(relative.parent().unwrap_or(Path::new("")));
    let info = ToneInfo::read(&folder).filter(|i| i.id > 0)?;
    let (model_id, model_name) = match info.model_of(&file) {
        Some((id, name)) => (Some(id), name),
        None => (
            None,
            relative
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
    };
    Some(Source::Tone3000(Tone3000Model {
        tone_id: info.id,
        model_id,
        tone_title: info.title,
        model_name,
        url: info.url,
        license: info.license,
        creator: info.creator,
        gear: info.gear,
        format: info.format,
    }))
}

/// A file only this pedalboard has
fn local_file(path: &str) -> Source {
    let relative = Path::new(path);
    let file = relative
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let extension = relative
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let what = if scanner::FileKind::Neural
        .extensions()
        .contains(&extension.as_str())
    {
        "model"
    } else if scanner::FileKind::ImpulseResponse
        .extensions()
        .contains(&extension.as_str())
    {
        "impulse response"
    } else {
        "file"
    };
    Source::Local {
        what: what.into(),
        name: file,
    }
}
