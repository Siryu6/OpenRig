//! Checks of a rig: what makes it unusable, what keeps it from being
//! published, and what is only worth a warning.

use serde::Serialize;
use std::collections::{BTreeSet, HashSet};

use crate::deps::Source;
use crate::path::is_safe_relative_path;
use crate::{limits, Rig, FORMAT_VERSION};

/// How serious an issue is
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The rig must not be used: refuse the file
    Invalid,
    /// Fine to import on a pedalboard, but not to publish: something in it
    /// cannot be fetched by others
    Unpublishable,
    /// Worth telling, changes nothing
    Warning,
}

/// What an issue is about; stable names for a server to match on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Code {
    /// `format` is not one this crate reads
    UnsupportedFormat,
    /// The rig has no name
    MissingName,
    /// A text is over its limit
    TooLong,
    /// Too many blocks, tags, dependencies, or splits too deep
    TooMany,
    /// The preset is not a JSON object, or has nodes this reader does not
    /// know
    InvalidPreset,
    /// A file path that could leave the models directory
    UnsafePath,
    /// A plugin or file of the preset is missing from the manifest
    MissingDependency,
    /// A manifest entry the preset does not use
    UnusedDependency,
    /// Two manifest entries for the same plugin or file
    DuplicateDependency,
    /// A source that does not fit (a TONE3000 plugin, a store file), or
    /// with empty required fields
    BadSource,
    /// A plugin or file only the exporting pedalboard has
    LocalDependency,
    /// A source of a newer format version
    UnknownSource,
    /// A TONE3000 model without its model id: found again by name
    MissingModelId,
    /// No block at all
    EmptyChain,
}

/// One finding
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// How serious it is
    pub level: Level,
    /// What it is about
    pub code: Code,
    /// Where, as a JSON pointer into the rig ("/dependencies/files/2/path")
    pub at: String,
    /// For people, in English
    pub message: String,
}

/// Result of [`Rig::validate`](crate::Rig::validate)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Every finding, in the order found
    pub issues: Vec<Issue>,
}

impl Report {
    /// Usable: no [`Level::Invalid`] issue
    pub fn is_valid(&self) -> bool {
        !self.issues.iter().any(|i| i.level == Level::Invalid)
    }

    /// Can be published: no [`Level::Invalid`] nor
    /// [`Level::Unpublishable`] issue
    pub fn is_publishable(&self) -> bool {
        self.issues.iter().all(|i| i.level == Level::Warning)
    }

    /// Issues of one level
    pub fn at_level(&self, level: Level) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(move |i| i.level == level)
    }

    fn push(&mut self, level: Level, code: Code, at: impl Into<String>, message: String) {
        self.issues.push(Issue {
            level,
            code,
            at: at.into(),
            message,
        });
    }
}

fn too_long(text: &str, max: usize) -> bool {
    text.chars().count() > max
}

pub(crate) fn validate(rig: &Rig) -> Report {
    let mut report = Report::default();
    if rig.format == 0 || rig.format > FORMAT_VERSION {
        report.push(
            Level::Invalid,
            Code::UnsupportedFormat,
            "/format",
            format!("Format {} is not supported", rig.format),
        );
    }
    meta(rig, &mut report);
    if !rig.preset.is_object() {
        report.push(
            Level::Invalid,
            Code::InvalidPreset,
            "/preset",
            "The preset is not a JSON object".into(),
        );
        return report;
    }
    chain(rig, &mut report);
    dependencies(rig, &mut report);
    report
}

fn meta(rig: &Rig, report: &mut Report) {
    let meta = &rig.meta;
    if meta.name.trim().is_empty() {
        report.push(
            Level::Invalid,
            Code::MissingName,
            "/meta/name",
            "The rig has no name".into(),
        );
    }
    for (field, text, max) in [
        ("name", &meta.name, limits::NAME),
        ("description", &meta.description, limits::DESCRIPTION),
        ("author", &meta.author, limits::SHORT_TEXT),
        ("app_version", &meta.app_version, limits::SHORT_TEXT),
        ("created", &meta.created, limits::SHORT_TEXT),
    ] {
        if too_long(text, max) {
            report.push(
                Level::Invalid,
                Code::TooLong,
                format!("/meta/{field}"),
                format!("The {field} is longer than {max} characters"),
            );
        }
    }
    if meta.tags.len() > limits::TAGS {
        report.push(
            Level::Invalid,
            Code::TooMany,
            "/meta/tags",
            format!("More than {} tags", limits::TAGS),
        );
    }
    for (i, tag) in meta.tags.iter().enumerate() {
        if tag.trim().is_empty() || too_long(tag, limits::TAG) {
            report.push(
                Level::Invalid,
                Code::TooLong,
                format!("/meta/tags/{i}"),
                format!("Tags are 1 to {} characters", limits::TAG),
            );
        }
    }
}

fn chain(rig: &Rig, report: &mut Report) {
    let view = rig.chain();
    if view.node_count() > limits::BLOCKS {
        report.push(
            Level::Invalid,
            Code::TooMany,
            "/preset/chain/nodes",
            format!("More than {} blocks", limits::BLOCKS),
        );
    }
    if view.depth() > limits::DEPTH {
        report.push(
            Level::Invalid,
            Code::TooMany,
            "/preset/chain/nodes",
            format!("Splits nested more than {} deep", limits::DEPTH),
        );
    }
    if view.has_unknown() {
        report.push(
            Level::Invalid,
            Code::InvalidPreset,
            "/preset/chain/nodes",
            "The chain has nodes this version does not know".into(),
        );
    }
    if view.blocks.is_empty() {
        report.push(
            Level::Warning,
            Code::EmptyChain,
            "/preset/chain/nodes",
            "The chain is empty".into(),
        );
    }
    let plugins = view.plugins();
    let uris: BTreeSet<&str> = plugins.iter().map(|p| p.block.uri.as_str()).collect();
    for uri in uris {
        if rig.dependencies.plugin(uri).is_none() {
            report.push(
                Level::Invalid,
                Code::MissingDependency,
                "/dependencies/plugins",
                format!("Plugin {uri} is not in the manifest"),
            );
        }
    }
    for file in rig.file_refs() {
        if !is_safe_relative_path(&file.path) {
            report.push(
                Level::Invalid,
                Code::UnsafePath,
                "/preset/chain",
                format!(
                    "Block {} loads a file with an unsafe path: {:?}",
                    file.instance_id, file.path
                ),
            );
        } else if rig.dependencies.file(&file.path).is_none() {
            report.push(
                Level::Invalid,
                Code::MissingDependency,
                "/dependencies/files",
                format!("File {} is not in the manifest", file.path),
            );
        }
    }
}

fn dependencies(rig: &Rig, report: &mut Report) {
    let deps = &rig.dependencies;
    if deps.plugins.len() + deps.files.len() > limits::DEPENDENCIES {
        report.push(
            Level::Invalid,
            Code::TooMany,
            "/dependencies",
            format!("More than {} dependencies", limits::DEPENDENCIES),
        );
    }
    let view = rig.chain();
    let used_uris: HashSet<&str> = view
        .plugins()
        .iter()
        .map(|p| p.block.uri.as_str())
        .collect();
    let used_paths: HashSet<String> = rig.file_refs().into_iter().map(|f| f.path).collect();

    let mut seen = HashSet::new();
    for (i, plugin) in deps.plugins.iter().enumerate() {
        let at = format!("/dependencies/plugins/{i}");
        if !seen.insert(plugin.uri.as_str()) {
            report.push(
                Level::Invalid,
                Code::DuplicateDependency,
                &at,
                format!("Plugin {} is listed twice", plugin.uri),
            );
        }
        if !used_uris.contains(plugin.uri.as_str()) {
            report.push(
                Level::Warning,
                Code::UnusedDependency,
                &at,
                format!("Plugin {} is not used by the chain", plugin.uri),
            );
        }
        if too_long(&plugin.uri, limits::SHORT_TEXT * 2)
            || too_long(&plugin.name, limits::SHORT_TEXT)
        {
            report.push(
                Level::Invalid,
                Code::TooLong,
                &at,
                "Plugin URI or name too long".into(),
            );
        }
        let label = if plugin.name.is_empty() {
            &plugin.uri
        } else {
            &plugin.name
        };
        match &plugin.source {
            Source::Builtin { kind } if kind.is_empty() => {
                bad(report, &at, format!("Built-in block {label} has no kind"))
            }
            Source::Store {
                package, version, ..
            } if package.is_empty() || version.is_empty() => bad(
                report,
                &at,
                format!("Store plugin {label} has no package or version"),
            ),
            Source::Tone3000(_) => bad(
                report,
                &at,
                format!("Plugin {label} cannot come from TONE3000"),
            ),
            source => common(report, &at, source, label),
        }
    }

    let mut seen = HashSet::new();
    for (i, file) in deps.files.iter().enumerate() {
        let at = format!("/dependencies/files/{i}");
        if !is_safe_relative_path(&file.path) {
            report.push(
                Level::Invalid,
                Code::UnsafePath,
                format!("{at}/path"),
                format!("Unsafe file path: {:?}", file.path),
            );
            continue;
        }
        if !seen.insert(file.path.as_str()) {
            report.push(
                Level::Invalid,
                Code::DuplicateDependency,
                &at,
                format!("File {} is listed twice", file.path),
            );
        }
        if !used_paths.contains(&file.path) {
            report.push(
                Level::Warning,
                Code::UnusedDependency,
                &at,
                format!("File {} is not used by the chain", file.path),
            );
        }
        match &file.source {
            Source::Builtin { .. } | Source::Store { .. } => bad(
                report,
                &at,
                format!("File {} must come from TONE3000 or be local", file.path),
            ),
            Source::Tone3000(model) => {
                if model.tone_id == 0 || model.model_name.is_empty() {
                    bad(
                        report,
                        &at,
                        format!("TONE3000 file {} has no tone id or model name", file.path),
                    );
                } else if model.model_id.is_none() {
                    report.push(
                        Level::Warning,
                        Code::MissingModelId,
                        &at,
                        format!(
                            "TONE3000 file {} has no model id: it is found again by name",
                            file.path
                        ),
                    );
                }
                for text in [
                    &model.tone_title,
                    &model.model_name,
                    &model.url,
                    &model.creator,
                    &model.gear,
                    &model.format,
                ] {
                    if too_long(text, limits::SHORT_TEXT * 2) {
                        report.push(
                            Level::Invalid,
                            Code::TooLong,
                            &at,
                            "TONE3000 text too long".into(),
                        );
                        break;
                    }
                }
            }
            source => common(report, &at, source, &file.path),
        }
    }
}

fn bad(report: &mut Report, at: &str, message: String) {
    report.push(Level::Invalid, Code::BadSource, at, message);
}

/// Local and unknown sources, of plugins and files alike
fn common(report: &mut Report, at: &str, source: &Source, label: &str) {
    match source {
        Source::Local { what, .. } => report.push(
            Level::Unpublishable,
            Code::LocalDependency,
            at,
            format!(
                "{} {label} is only on the exporting pedalboard",
                capitalized(if what.is_empty() { "item" } else { what })
            ),
        ),
        Source::Unknown => report.push(
            Level::Unpublishable,
            Code::UnknownSource,
            at,
            format!("{label} comes from a source this version does not know"),
        ),
        _ => {}
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
