//! The dependency manifest: where each plugin and file of the preset comes
//! from.

use serde::{Deserialize, Serialize};

/// Everything the preset needs besides itself
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dependencies {
    /// One entry per plugin URI used by the chain (a plugin used by two
    /// blocks is listed once)
    pub plugins: Vec<PluginDependency>,
    /// One entry per file path loaded by the chain (block models, impulse
    /// responses, files of LV2 plugins)
    pub files: Vec<FileDependency>,
}

impl Dependencies {
    /// The entry of plugin `uri`
    pub fn plugin(&self, uri: &str) -> Option<&PluginDependency> {
        self.plugins.iter().find(|p| p.uri == uri)
    }

    /// The entry of file `path`
    pub fn file(&self, path: &str) -> Option<&FileDependency> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Every source, plugins first
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.plugins
            .iter()
            .map(|p| &p.source)
            .chain(self.files.iter().map(|f| &f.source))
    }

    /// Whether every plugin and file can be fetched by another pedalboard
    pub fn shareable(&self) -> bool {
        self.sources().all(Source::shareable)
    }
}

/// A plugin used by the chain
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDependency {
    /// Plugin URI, as in the chain (`urn:pedalboard:nam`, an LV2 URI)
    pub uri: String,
    /// Display name of the plugin ("Amp", "ZamComp")
    #[serde(default)]
    pub name: String,
    /// Where it comes from: [`Source::Builtin`], [`Source::Store`] or
    /// [`Source::Local`]
    pub source: Source,
}

/// A file loaded by the chain
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDependency {
    /// Path relative to the models directory, `/`-separated, as written in
    /// the preset ("Tone3000/Amps/Plexi [123]/Crunch [456].nam")
    pub path: String,
    /// Instance ids of the blocks that load it
    #[serde(default)]
    pub blocks: Vec<u64>,
    /// Where it comes from: [`Source::Tone3000`] or [`Source::Local`]
    pub source: Source,
}

/// Where a plugin or a file comes from. Serialized with a `type` field:
/// `{"type": "store", "package": "zam-plugins", …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// A block built into OpenRig: nothing to install
    Builtin {
        /// What the block is: the last part of its URI
        /// (`urn:pedalboard:nam-amp-cab` → `nam-amp-cab`)
        kind: String,
    },
    /// An LV2 plugin of the OpenRig store (catalog index of openrig-catalog)
    Store {
        /// Catalog package id ("zam-plugins")
        package: String,
        /// Package version installed on the exporting pedalboard; another
        /// version of the package is accepted on import (settings are kept
        /// by port symbol)
        version: String,
        /// Catalog entry id of the plugin ("zam-comp"), when known
        #[serde(default, skip_serializing_if = "String::is_empty")]
        plugin_id: String,
    },
    /// A model or impulse response downloaded from TONE3000
    Tone3000(Tone3000Model),
    /// Something only the exporting pedalboard has (a plugin copied by
    /// hand, a model of one's own): the rig imports without it and cannot
    /// be published
    Local {
        /// "plugin", "model", "impulse response", "file"
        what: String,
        /// Plugin name or file name, for people
        name: String,
    },
    /// A source of a newer version of the format: treated as not
    /// available
    #[serde(other)]
    Unknown,
}

impl Source {
    /// Whether another pedalboard can get it
    pub fn shareable(&self) -> bool {
        !matches!(self, Source::Local { .. } | Source::Unknown)
    }

    /// The `type` name: "builtin", "store", "tone3000", "local", "unknown"
    pub fn type_name(&self) -> &'static str {
        match self {
            Source::Builtin { .. } => "builtin",
            Source::Store { .. } => "store",
            Source::Tone3000(_) => "tone3000",
            Source::Local { .. } => "local",
            Source::Unknown => "unknown",
        }
    }
}

/// One model of a TONE3000 tone. `tone_id` and `model_id` are the ids of
/// the TONE3000 API (`GET /api/v1/tones/{tone_id}`,
/// `GET /api/v1/models/{model_id}`); the model is downloaded from the
/// `model_url` of the latter, with the importing user's account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tone3000Model {
    /// TONE3000 tone id (the capture pack)
    pub tone_id: u64,
    /// TONE3000 model id (the file in the pack); `None` when the exporting
    /// pedalboard did not know it: the importer then looks the model up
    /// by `model_name` among the models of the tone
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<u64>,
    /// Title of the tone
    pub tone_title: String,
    /// Name of the model in the tone
    pub model_name: String,
    /// Page of the tone on tone3000.com
    pub url: String,
    /// License of the tone, as TONE3000 gives it ("CC BY 4.0")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// TONE3000 username of the creator
    pub creator: String,
    /// TONE3000 gear type: "amp", "amp-cab", "pedal", "outboard", "cab",
    /// "space", "experimental"
    pub gear: String,
    /// TONE3000 format: "nam", "ir"
    pub format: String,
}
