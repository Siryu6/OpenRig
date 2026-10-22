//! The catalog index (`catalog-v1.json`, built by the openrig-catalog
//! repository): packages, the files to download per architecture, and
//! the plugins each one holds, with their store texts and pictures.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The only index layout this app reads
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub schema: u32,
    /// Grows with every published index: an older one is refused, so a
    /// replayed index cannot bring back a withdrawn plugin
    pub serial: u64,
    pub generated: String,
    /// Past this date the index is stale: browsing works, installing does not
    pub expires: String,
    pub packages: Vec<Package>,
}

/// One upstream project at one version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub homepage: String,
    /// "upstream": the authors' own file; "openrig-build": built by OpenRig
    /// from `source` and published with it
    pub origin: String,
    pub source: Source,
    /// File per architecture ("aarch64", "x86_64")
    #[serde(default)]
    pub artifacts: HashMap<String, Artifact>,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// "tar.xz", "tar.gz" or "deb"
    pub format: String,
    /// Directory of the archive the bundles are in ("" or absent: its top)
    #[serde(default)]
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub id: String,
    pub uri: String,
    /// The `.lv2` directory holding the plugin, in `root` of the archive
    pub bundle: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub family: String,
    pub subcategory: String,
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub inspired_by: String,
    pub summary: String,
    pub description: String,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub required_features: Vec<String>,
    #[serde(default)]
    pub ports_hash: String,
    #[serde(default)]
    pub controls: Vec<Control>,
    #[serde(default)]
    pub media: Media,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Control {
    pub index: usize,
    pub symbol: String,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub picture: Option<Picture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Picture {
    pub url: String,
    pub sha256: String,
    pub w: u32,
    pub h: u32,
    pub source: String,
    pub license: String,
    pub credit: String,
    /// The picture may stand for the block in the rack
    #[serde(default)]
    pub block_face: bool,
}

impl Index {
    /// Read an index (already verified) and check it is one this app can use
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let index: Index = serde_json::from_slice(bytes).context("Unreadable plugin catalog")?;
        if index.schema != SCHEMA {
            bail!("This plugin catalog needs a newer OpenRig");
        }
        index.expiry()?;
        for package in &index.packages {
            if !valid_name(&package.id) {
                bail!("Invalid package id in the catalog: {}", package.id);
            }
            for plugin in &package.plugins {
                if !valid_name(&plugin.id) || !valid_bundle(&plugin.bundle) {
                    bail!("Invalid plugin entry in the catalog: {}", plugin.id);
                }
            }
            for artifact in package.artifacts.values() {
                if !artifact.root.is_empty() && !valid_relative(&artifact.root) {
                    bail!("Invalid archive root in the catalog: {}", package.id);
                }
            }
        }
        Ok(index)
    }

    /// When the index stops being trusted
    pub fn expiry(&self) -> Result<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(&self.expires)
            .map(|d| d.with_timezone(&chrono::Utc))
            .context("Invalid expiry date in the plugin catalog")
    }

    /// True once the expiry date is past
    pub fn is_stale(&self) -> bool {
        self.expiry().map_or(true, |e| e < chrono::Utc::now())
    }

    /// Package and plugin of entry `id`
    pub fn find(&self, id: &str) -> Option<(&Package, &Plugin)> {
        self.packages
            .iter()
            .find_map(|p| p.plugins.iter().find(|e| e.id == id).map(|e| (p, e)))
    }

    /// Every plugin with its package
    pub fn entries(&self) -> impl Iterator<Item = (&Package, &Plugin)> {
        self.packages
            .iter()
            .flat_map(|p| p.plugins.iter().map(move |e| (p, e)))
    }
}

impl Package {
    /// The file for this machine, if the package has one
    pub fn artifact(&self) -> Option<&Artifact> {
        self.artifacts.get(super::arch())
    }
}

impl Plugin {
    /// LV2 features the plugin needs that this host does not provide
    pub fn missing_features(&self) -> Vec<&str> {
        #[cfg(feature = "lv2")]
        let supported = crate::plugins::lv2_features::SUPPORTED_FEATURES;
        #[cfg(not(feature = "lv2"))]
        let supported: &[&str] = &[];
        self.required_features
            .iter()
            .map(String::as_str)
            .filter(|f| !supported.contains(f))
            .collect()
    }
}

/// Package and plugin ids: lowercase letters, digits and dashes
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// A bundle directory name: one path component ending in `.lv2`
pub fn valid_bundle(bundle: &str) -> bool {
    bundle.ends_with(".lv2")
        && bundle.len() > 4
        && !bundle.starts_with('.')
        && bundle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// A relative path without `..`, `.`, empty or absolute components
pub fn valid_relative(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample(expires: &str) -> String {
        format!(
            r#"{{"schema":1,"serial":2026101913,"generated":"2026-10-19T14:31:15Z","expires":"{expires}",
            "packages":[{{"id":"zam-plugins","name":"ZAM Plugins","version":"4.5","license":"GPL-2.0-or-later",
            "homepage":"https://example.org","origin":"upstream","source":{{"url":"https://example.org/s.tar.gz","sha256":null}},
            "artifacts":{{"x86_64":{{"url":"https://example.org/x.tar.xz","sha256":"00","size":10,"format":"tar.xz","root":"zam-plugins-4.5"}}}},
            "plugins":[{{"id":"zam-comp","uri":"urn:zamaudio:ZamComp","bundle":"ZamComp.lv2","name":"ZamComp",
            "author":"Damien Zammit","license":"GPL-2.0-or-later","family":"Dynamics","subcategory":"Compressor",
            "kind":"pedal","tags":["transparent"],"inspired_by":"","summary":"s","description":"d",
            "required_features":["http://lv2plug.in/ns/ext/urid#map","urn:unknown"],"media":{{}}}}]}}]}}"#
        )
    }

    #[test]
    fn parses_an_index() {
        let index = Index::parse(sample("2099-01-01T00:00:00Z").as_bytes()).unwrap();
        assert!(!index.is_stale());
        let (package, plugin) = index.find("zam-comp").unwrap();
        assert_eq!(package.id, "zam-plugins");
        assert_eq!(plugin.bundle, "ZamComp.lv2");
        #[cfg(feature = "lv2")]
        assert_eq!(plugin.missing_features(), vec!["urn:unknown"]);
    }

    #[test]
    fn stale_index_is_reported() {
        let index = Index::parse(sample("2020-01-01T00:00:00Z").as_bytes()).unwrap();
        assert!(index.is_stale());
    }

    #[test]
    fn hostile_names_are_refused() {
        let bad = sample("2099-01-01T00:00:00Z").replace("ZamComp.lv2", "../x.lv2");
        assert!(Index::parse(bad.as_bytes()).is_err());
        let bad = sample("2099-01-01T00:00:00Z").replace("zam-plugins-4.5", "../..");
        assert!(Index::parse(bad.as_bytes()).is_err());
        assert!(valid_relative("a/b"));
        assert!(!valid_relative("/a"));
        assert!(!valid_relative("a//b"));
        assert!(valid_bundle("gx_sd1.lv2"));
        assert!(!valid_bundle(".lv2"));
    }
}
