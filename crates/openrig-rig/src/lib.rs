//! The shareable OpenRig **rig** format (`.openrig` files).
//!
//! A rig is one OpenRig preset (the whole chain: blocks, splits, settings,
//! scenes, tempo, footswitch assignments) together with a **dependency
//! manifest** saying where every plugin and every model file it uses comes
//! from, so that another pedalboard can rebuild it: plugins from the OpenRig
//! store, models from TONE3000, blocks built into OpenRig. Anything that
//! cannot be fetched again (a plugin copied by hand, a model file of one's
//! own) is listed as [`Source::Local`]: such a rig can still be imported,
//! but cannot be published.
//!
//! The format is plain JSON, described in the crate's `README.md`. This
//! crate is the reference reader and writer; it has no platform code and
//! builds for any target, so the pedalboard and the community site share
//! it.
//!
//! ```
//! use openrig_rig::{Rig, Meta, Dependencies};
//!
//! let preset = serde_json::json!({
//!     "name": "Crunch",
//!     "chain": { "nodes": [], "params": {} }
//! });
//! let rig = Rig::new(Meta::named("Crunch"), preset, Dependencies::default());
//! let bytes = rig.to_vec_pretty().unwrap();
//! let back = Rig::from_slice(&bytes).unwrap();
//! assert_eq!(back.meta.name, "Crunch");
//! assert!(back.validate().is_valid());
//! ```

mod deps;
mod path;
mod validate;
mod view;

pub use deps::{Dependencies, FileDependency, PluginDependency, Source, Tone3000Model};
pub use path::is_safe_relative_path;
pub use validate::{Code, Issue, Level, Report};
pub use view::{Block, ChainView, FileRef, PlacedBlock, PluginBlock, SplitBlock, SplitKind};

use serde::{Deserialize, Serialize};

/// Version of the format this crate writes, and the newest it reads.
///
/// Bumped only for changes an older reader would misread; fields added in
/// a backward compatible way keep it (readers ignore unknown fields).
pub const FORMAT_VERSION: u32 = 1;

/// File extension of a rig, without the dot
pub const FILE_EXTENSION: &str = "openrig";

/// Media type of a rig, for HTTP downloads and uploads
pub const MEDIA_TYPE: &str = "application/vnd.openrig.rig+json";

/// Largest rig file accepted, in bytes. Rigs are a few kilobytes: they
/// reference models and plugins, they never embed them.
pub const MAX_RIG_BYTES: usize = 1024 * 1024;

/// Upper bounds checked by [`Rig::validate`]
pub mod limits {
    /// Characters of the rig name
    pub const NAME: usize = 80;
    /// Characters of the description
    pub const DESCRIPTION: usize = 4000;
    /// Number of tags
    pub const TAGS: usize = 16;
    /// Characters of one tag
    pub const TAG: usize = 32;
    /// Characters of the author and of other short texts
    pub const SHORT_TEXT: usize = 200;
    /// Plugin blocks and splits in the chain, all levels together
    pub const BLOCKS: usize = 128;
    /// Splits nested in splits
    pub const DEPTH: usize = 8;
    /// Entries of the dependency manifest (plugins and files together)
    pub const DEPENDENCIES: usize = 512;
    /// Bytes of a file path
    pub const PATH: usize = 512;
}

/// Why a rig could not be read
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Over [`MAX_RIG_BYTES`]
    #[error("The rig is too large ({0} bytes, at most {MAX_RIG_BYTES})")]
    TooLarge(usize),
    /// Not JSON, or not shaped like a rig
    #[error("Not a valid rig: {0}")]
    Json(#[from] serde_json::Error),
    /// No `format` number at the top
    #[error("Not a rig: no format number")]
    NotARig,
    /// Written by a newer OpenRig, in a format this reader does not know
    #[error("This rig needs a newer OpenRig (format {0}, this one reads up to {FORMAT_VERSION})")]
    UnsupportedFormat(u64),
}

/// A rig file
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rig {
    /// Format version, [`FORMAT_VERSION`] when written by this crate
    pub format: u32,
    /// What the rig is, for people
    #[serde(default)]
    pub meta: Meta,
    /// The OpenRig preset, as the app saves it (see the README for the
    /// fields this crate reads). Carried as JSON so that the app can add
    /// preset fields without a new version of this crate.
    pub preset: serde_json::Value,
    /// Where every plugin and file of the preset comes from
    #[serde(default)]
    pub dependencies: Dependencies,
}

/// Description of a rig
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    /// Name shown in lists, and of the preset created on import
    pub name: String,
    /// Free text
    pub description: String,
    /// Lower-case keywords ("metal", "blues", "ambient"…)
    pub tags: Vec<String>,
    /// Who made it, free text (the site shows its own account name)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Version of the OpenRig that exported it ("0.11.0")
    pub app_version: String,
    /// When it was exported, RFC 3339 in UTC ("2026-11-06T18:30:00Z"),
    /// empty when the clock was not set
    pub created: String,
}

impl Meta {
    /// A description with only a name
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

impl Rig {
    /// A rig of the current format
    pub fn new(meta: Meta, preset: serde_json::Value, dependencies: Dependencies) -> Self {
        Self {
            format: FORMAT_VERSION,
            meta,
            preset,
            dependencies,
        }
    }

    /// Read a rig: size, format version, then the fields. Does not
    /// [`validate`](Self::validate) it.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_RIG_BYTES {
            return Err(Error::TooLarge(bytes.len()));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let format = value
            .get("format")
            .and_then(serde_json::Value::as_u64)
            .ok_or(Error::NotARig)?;
        if format == 0 || format > u64::from(FORMAT_VERSION) {
            return Err(Error::UnsupportedFormat(format));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// Read a rig from a string, as [`from_slice`](Self::from_slice)
    pub fn from_json(text: &str) -> Result<Self, Error> {
        Self::from_slice(text.as_bytes())
    }

    /// The rig as indented JSON, as it is written to a file
    pub fn to_vec_pretty(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Typed, read-only view of the chain of the preset, with the names of
    /// the blocks taken from the manifest
    pub fn chain(&self) -> ChainView {
        ChainView::read(&self.preset, &self.dependencies)
    }

    /// Every file the preset loads (block models and plugin files), as
    /// written in the preset: paths relative to the models directory
    pub fn file_refs(&self) -> Vec<FileRef> {
        view::file_refs(&self.preset)
    }

    /// Check the rig; see [`Report`]
    pub fn validate(&self) -> Report {
        validate::validate(self)
    }
}

#[cfg(test)]
mod tests;
