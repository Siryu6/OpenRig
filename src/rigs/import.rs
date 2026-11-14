//! Rig to preset: what the rig needs, what this pedalboard has, then
//! installing the rest and saving the preset with this pedalboard's paths.
//!
//! Nothing from the file is ever run. Paths are used only once the format
//! crate accepts them (`is_safe_relative_path`), and downloaded models go
//! where this app decides, not where the rig says.

use anyhow::{bail, Context as _, Result};
use std::collections::HashMap;
use std::path::Path;

use super::format::{is_safe_relative_path, Rig, Source, Tone3000Model};
use super::CatalogPlugin;
use crate::plugins::scanner::FileKind;
use crate::presets::Preset;
use crate::tone_info::{model_file, tone_folder};

/// What the importing pedalboard has
pub struct Context<'a> {
    pub models_dir: &'a Path,
    /// The plugins of the store catalog (empty without one)
    pub catalog: &'a [CatalogPlugin],
    /// Whether a plugin URI is installed (or built in)
    pub installed: &'a dyn Fn(&str) -> bool,
    /// Whether this build downloads from TONE3000
    pub tone3000: bool,
}

/// What a rig item needs
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Already here
    Ready,
    /// Install catalog entry `plugin_id` from the store
    Store { plugin_id: String },
    /// Download from TONE3000
    Tone3000(Tone3000Model),
    /// Cannot be had here: the preset is imported without it
    Missing(String),
}

/// A plugin or a file of the rig
#[derive(Debug, Clone, PartialEq)]
pub enum Subject {
    Plugin { uri: String },
    File { path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub subject: Subject,
    /// Plugin or model name
    pub label: String,
    pub step: Step,
    /// For a file already here at another path than the rig's
    pub local_path: Option<String>,
}

/// A rig and what installing it takes
#[derive(Debug, Clone)]
pub struct Plan {
    pub rig: Rig,
    pub items: Vec<Item>,
}

impl Plan {
    pub fn count(&self, f: impl Fn(&Step) -> bool) -> usize {
        self.items.iter().filter(|i| f(&i.step)).count()
    }

    pub fn to_install(&self) -> usize {
        self.count(|s| matches!(s, Step::Store { .. }))
    }

    pub fn to_download(&self) -> usize {
        self.count(|s| matches!(s, Step::Tone3000(_)))
    }

    pub fn missing(&self) -> usize {
        self.count(|s| matches!(s, Step::Missing(_)))
    }
}

/// What `rig` needs on this pedalboard. Refuses a rig the format crate
/// finds invalid (unsafe paths, unknown nodes, incomplete manifest).
pub fn plan(rig: Rig, context: &Context) -> Result<Plan> {
    let report = rig.validate();
    if !report.is_valid() {
        let reasons: Vec<&str> = report
            .at_level(super::format::Level::Invalid)
            .map(|i| i.message.as_str())
            .take(3)
            .collect();
        bail!("This rig cannot be used: {}", reasons.join("; "));
    }
    let mut items = Vec::new();
    for plugin in &rig.dependencies.plugins {
        let label = if plugin.name.is_empty() {
            plugin.uri.clone()
        } else {
            plugin.name.clone()
        };
        let step = if (context.installed)(&plugin.uri) {
            Step::Ready
        } else {
            match &plugin.source {
                Source::Builtin { .. } => {
                    Step::Missing("Built-in block missing from this OpenRig build".into())
                }
                Source::Store { .. } => {
                    match context.catalog.iter().find(|c| c.uri == plugin.uri) {
                        Some(entry) => Step::Store {
                            plugin_id: entry.plugin_id.clone(),
                        },
                        None if context.catalog.is_empty() => {
                            Step::Missing("Plugin catalog not downloaded".into())
                        }
                        None => Step::Missing("No longer in the plugin store".into()),
                    }
                }
                Source::Local { .. } => Step::Missing("Only on the pedalboard it came from".into()),
                Source::Tone3000(_) | Source::Unknown => {
                    Step::Missing("Unknown source, needs a newer OpenRig".into())
                }
            }
        };
        items.push(Item {
            subject: Subject::Plugin {
                uri: plugin.uri.clone(),
            },
            label,
            step,
            local_path: None,
        });
    }
    for file in &rig.dependencies.files {
        // Checked by validate(); again here, as the path is joined below
        if !is_safe_relative_path(&file.path) {
            bail!("Unsafe file path in the rig: {:?}", file.path);
        }
        let name = Path::new(&file.path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.path.clone());
        let here = context.models_dir.join(&file.path).is_file();
        let (label, step, local_path) = match &file.source {
            _ if here => (name, Step::Ready, None),
            Source::Tone3000(model) => {
                let label = format!("{} · {}", model.tone_title, model.model_name);
                match downloaded_path(context.models_dir, model) {
                    Some(path) => (label, Step::Ready, Some(path)),
                    None if !context.tone3000 => (
                        label,
                        Step::Missing("This OpenRig has no TONE3000 support".into()),
                        None,
                    ),
                    None if FileKind::of_format(&model.format).is_none() => (
                        label,
                        Step::Missing(format!("Unsupported format {}", model.format)),
                        None,
                    ),
                    None => (label, Step::Tone3000(model.clone()), None),
                }
            }
            Source::Local { .. } => (
                name,
                Step::Missing("Only on the pedalboard it came from".into()),
                None,
            ),
            _ => (
                name,
                Step::Missing("Unknown source, needs a newer OpenRig".into()),
                None,
            ),
        };
        items.push(Item {
            subject: Subject::File {
                path: file.path.clone(),
            },
            label,
            step,
            local_path,
        });
    }
    Ok(Plan { rig, items })
}

/// Where the TONE3000 library would have put `model`, if it is there
fn downloaded_path(models_dir: &Path, model: &Tone3000Model) -> Option<String> {
    let id = model.model_id?;
    let extension = FileKind::of_format(&model.format)?.extension();
    let path = tone_folder(&model.gear, &model.tone_title, model.tone_id).join(model_file(
        &model.model_name,
        id,
        extension,
    ));
    models_dir.join(&path).is_file().then(|| portable(&path))
}

/// `/`-separated, as presets save paths
fn portable(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Installs what a plan needs; the app's store and TONE3000 account
pub trait Installer {
    /// Install catalog entry `plugin_id`
    fn install_plugin(&mut self, plugin_id: &str) -> Result<()>;
    /// Download a TONE3000 model; its path relative to the models
    /// directory
    fn download_model(&mut self, model: &Tone3000Model) -> Result<std::path::PathBuf>;
    /// Before each step: `done` of `total`, and what comes next
    fn progress(&mut self, _done: usize, _total: usize, _label: &str) {}
}

/// Result of an install
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The new preset (not saved yet)
    pub preset: Preset,
    /// Installed or downloaded now
    pub done: Vec<String>,
    /// What could not be installed or downloaded, with the reason
    pub failed: Vec<(String, String)>,
    /// What the plan already knew it could not get
    pub missing: Vec<(String, String)>,
    /// Some plugin was installed: the plugins must be scanned again
    pub plugins_installed: bool,
}

/// Install what `plan` needs, going on after a failure, and build the
/// preset with this pedalboard's paths
pub fn install(plan: &Plan, installer: &mut dyn Installer) -> Result<Outcome> {
    let mut outcome = Outcome {
        preset: preset_of(&plan.rig)?,
        done: Vec::new(),
        failed: Vec::new(),
        missing: Vec::new(),
        plugins_installed: false,
    };
    let mut paths: HashMap<String, String> = HashMap::new();
    let total = plan.to_install() + plan.to_download();
    let mut done = 0;
    for item in &plan.items {
        if let (Some(local), Subject::File { path }) = (&item.local_path, &item.subject) {
            paths.insert(path.clone(), local.clone());
        }
        match &item.step {
            Step::Ready => {}
            Step::Missing(reason) => outcome.missing.push((item.label.clone(), reason.clone())),
            Step::Store { plugin_id } => {
                installer.progress(done, total, &item.label);
                done += 1;
                match installer.install_plugin(plugin_id) {
                    Ok(()) => {
                        outcome.plugins_installed = true;
                        outcome.done.push(item.label.clone());
                    }
                    Err(e) => outcome.failed.push((item.label.clone(), format!("{e:#}"))),
                }
            }
            Step::Tone3000(model) => {
                installer.progress(done, total, &item.label);
                done += 1;
                let result = installer.download_model(model).and_then(|path| {
                    let path = portable(&path);
                    if !is_safe_relative_path(&path) {
                        bail!("Downloaded to an unexpected place: {path}");
                    }
                    Ok(path)
                });
                match (result, &item.subject) {
                    (Ok(new), Subject::File { path }) => {
                        paths.insert(path.clone(), new);
                        outcome.done.push(item.label.clone());
                    }
                    (Ok(_), Subject::Plugin { .. }) => {}
                    (Err(e), _) => outcome.failed.push((item.label.clone(), format!("{e:#}"))),
                }
            }
        }
    }
    installer.progress(done, total, "");
    remap(&mut outcome.preset, &paths);
    // Models not got: the preset keeps where they come from, for its next
    // export
    for item in &plan.items {
        let Subject::File { path } = &item.subject else {
            continue;
        };
        if item.step == Step::Ready || paths.contains_key(path) {
            continue;
        }
        if let Some(Source::Tone3000(model)) = plan.rig.dependencies.file(path).map(|f| &f.source) {
            outcome
                .preset
                .rig_models
                .insert(path.clone(), model.clone());
        }
    }
    Ok(outcome)
}

/// The rig's preset as a new preset of this pedalboard: new id, the rig's
/// name, today's dates
fn preset_of(rig: &Rig) -> Result<Preset> {
    let mut value = rig.preset.clone();
    let Some(object) = value.as_object_mut() else {
        bail!("The rig holds no preset");
    };
    // Fields a preset must have; the rest have defaults
    let fresh = Preset::new(rig.meta.name.trim().to_string());
    let defaults = serde_json::to_value(&fresh)?;
    if let Some(defaults) = defaults.as_object() {
        for (key, default) in defaults {
            if key != "chain" {
                object.entry(key.clone()).or_insert_with(|| default.clone());
            }
        }
    }
    let mut preset: Preset =
        serde_json::from_value(value).context("The rig's preset cannot be read")?;
    preset.id = fresh.id;
    preset.name = if fresh.name.is_empty() {
        "Imported rig".into()
    } else {
        fresh.name
    };
    preset.created_at = fresh.created_at;
    preset.modified_at = fresh.modified_at;
    // Only `install` fills it, from the manifest
    preset.rig_models.clear();
    Ok(preset)
}

/// Point the blocks at the files where they are on this pedalboard
fn remap(preset: &mut Preset, paths: &HashMap<String, String>) {
    let Some(chain) = preset.chain.as_mut() else {
        return;
    };
    for path in chain.files.values_mut() {
        if let Some(new) = paths.get(path.as_str()) {
            path.clone_from(new);
        }
    }
    for files in chain.param_files.values_mut() {
        for path in files.values_mut() {
            if let Some(new) = paths.get(path.as_str()) {
                path.clone_from(new);
            }
        }
    }
}
