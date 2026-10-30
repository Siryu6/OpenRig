//! Plugin store: the signed catalog index of openrig-catalog, and the LV2
//! bundles it lists, installed into `<data dir>/lv2`.
//!
//! - The index (`catalog-v1.json` + `catalog-v1.json.minisig`) is checked
//!   against the built-in catalog keys, then kept in `<data dir>/store/`
//!   so the store can be browsed offline. An index older than the kept one
//!   is refused, and past its expiry date nothing can be installed from it.
//! - An archive is downloaded from the URL the index pins (the authors'
//!   release, or OpenRig's build published with its sources), checked
//!   against its size and SHA-256, then only the bundle asked for is
//!   unpacked. Archives stay in `store/cache/` for the other plugins of
//!   the same package.
//! - Bundles copied into the LV2 folder by hand are always loaded, never
//!   checked, and never touched by the store.

pub mod fetch;
pub mod index;
pub mod install;
pub mod verify;

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use index::{Index, Package, Plugin};
use install::Installed;

/// Where the official catalog is published (a fixed release asset, so the
/// device needs no API call)
pub const INDEX_URL: &str =
    "https://github.com/Siryu6/openrig-catalog/releases/download/catalog/catalog-v1.json";
/// Another catalog (self-hosted, or a test server), signed by a key of
/// `store/trusted-keys.pub`
pub const URL_ENV: &str = "OPENRIG_CATALOG_URL";
const INDEX_FILE: &str = "catalog-v1.json";
const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PICTURE_BYTES: u64 = 2 * 1024 * 1024;

/// Architecture name used by the index ("aarch64", "x86_64")
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// The LV2 folder of the data directory: the store installs there, and
/// what is copied there by hand is loaded too
pub fn lv2_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("lv2")
}

/// Where a catalog entry stands on this machine
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Available,
    Installed,
    /// Installed from an older version of its package
    Update {
        installed: String,
    },
    /// Found on this machine, not installed by the store
    Manual,
    /// Cannot be installed here; the reason is for the user
    Unavailable(String),
}

impl Status {
    /// Name used by the UI
    pub fn name(&self) -> &'static str {
        match self {
            Status::Available => "available",
            Status::Installed => "installed",
            Status::Update { .. } => "update",
            Status::Manual => "manual",
            Status::Unavailable(_) => "unavailable",
        }
    }
}

/// Status of `plugin` of `package`, given the bundles of the LV2 folder
/// and whether its URI is loaded from somewhere else (system folder)
pub fn status(package: &Package, plugin: &Plugin, installed: &[Installed], loaded: bool) -> Status {
    if let Some(found) = installed.iter().find(|i| i.bundle == plugin.bundle) {
        return match &found.receipt {
            Some(r) if r.package == package.id && r.version == package.version => Status::Installed,
            Some(r) if r.package == package.id => Status::Update {
                installed: r.version.clone(),
            },
            _ => Status::Manual,
        };
    }
    if loaded {
        return Status::Manual;
    }
    if package.artifact().is_none() {
        return Status::Unavailable(format!("Not available for {} yet", arch()));
    }
    if !plugin.missing_features().is_empty() {
        return Status::Unavailable("Needs a newer OpenRig".into());
    }
    Status::Available
}

/// The store's files under the data directory
#[derive(Debug, Clone)]
pub struct Store {
    /// `<data dir>/store`: index, cache, pictures
    pub dir: PathBuf,
    /// `<data dir>/lv2`
    pub lv2_dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("store"),
            lv2_dir: lv2_dir(data_dir),
        }
    }

    pub fn index_url() -> String {
        std::env::var(URL_ENV).unwrap_or_else(|_| INDEX_URL.to_string())
    }

    fn keys(&self) -> Vec<String> {
        verify::trusted_keys(&self.dir)
    }

    /// The index kept from the last refresh, checked again
    pub fn cached(&self) -> Option<Index> {
        let data = fs::read(self.dir.join(INDEX_FILE)).ok()?;
        let signature = fs::read_to_string(self.dir.join(format!("{INDEX_FILE}.minisig"))).ok()?;
        match verify::verify(&data, &signature, &self.keys()).and_then(|_| Index::parse(&data)) {
            Ok(index) => Some(index),
            Err(e) => {
                tracing::warn!("Ignoring the kept plugin catalog: {e:#}");
                None
            }
        }
    }

    /// Download the index and its signature, check them and keep them
    pub fn refresh(&self) -> Result<Index> {
        let url = Self::index_url();
        let data = fetch::get(&url, MAX_INDEX_BYTES)?;
        let signature = fetch::get(&format!("{url}.minisig"), 4096)?;
        let signature = String::from_utf8(signature).context("Unreadable catalog signature")?;
        self.accept(&data, &signature)
    }

    /// Check a downloaded index against the keys and the kept one, and keep it
    pub fn accept(&self, data: &[u8], signature: &str) -> Result<Index> {
        verify::verify(data, signature, &self.keys())?;
        let index = Index::parse(data)?;
        if let Some(kept) = self.cached() {
            if index.serial < kept.serial {
                bail!("The plugin catalog online is older than the one on this pedalboard");
            }
        }
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Cannot create {}", self.dir.display()))?;
        crate::persist::write_atomic(
            &self.dir.join(format!("{INDEX_FILE}.minisig")),
            signature.as_bytes(),
        )?;
        crate::persist::write_atomic(&self.dir.join(INDEX_FILE), data)?;
        self.prune_cache(&index);
        Ok(index)
    }

    /// The bundles of the LV2 folder
    pub fn installed(&self) -> Vec<Installed> {
        install::installed(&self.lv2_dir)
    }

    /// Download (or take from the cache) the archive of `package` for this
    /// machine and install `plugin`'s bundle. `progress` gets the bytes
    /// received and the total. Not to be called on the UI thread.
    pub fn install(
        &self,
        index: &Index,
        plugin_id: &str,
        progress: impl FnMut(u64, u64),
    ) -> Result<()> {
        if index.is_stale() {
            bail!("The plugin catalog is out of date: refresh it first");
        }
        let (package, plugin) = index
            .find(plugin_id)
            .with_context(|| format!("{plugin_id} is not in the catalog"))?;
        let missing = plugin.missing_features();
        if !missing.is_empty() {
            bail!(
                "{} needs a newer OpenRig ({})",
                plugin.name,
                missing.join(", ")
            );
        }
        let artifact = package
            .artifact()
            .with_context(|| format!("{} is not available for {} yet", plugin.name, arch()))?;
        let cache = self.dir.join("cache");
        fs::create_dir_all(&cache)?;
        let archive = cache.join(format!("{}.{}", artifact.sha256, artifact.format));
        let cached =
            archive.is_file() && fetch::sha256_file(&archive).is_ok_and(|s| s == artifact.sha256);
        if !cached {
            fetch::download(
                &artifact.url,
                artifact.size,
                &artifact.sha256,
                &archive,
                progress,
            )?;
        }
        // Every entry of the catalog that lives in this bundle
        let plugins = package
            .plugins
            .iter()
            .filter(|p| p.bundle == plugin.bundle)
            .map(|p| p.id.clone())
            .collect();
        install::install(
            &self.lv2_dir,
            package,
            artifact,
            &plugin.bundle,
            plugins,
            &archive,
        )
    }

    /// Remove the bundle of catalog entry `plugin_id`
    pub fn remove(&self, index: &Index, plugin_id: &str) -> Result<()> {
        let (_, plugin) = index
            .find(plugin_id)
            .with_context(|| format!("{plugin_id} is not in the catalog"))?;
        install::remove(&self.lv2_dir, &plugin.bundle)
    }

    /// The picture of an entry, downloaded and checked on first use
    pub fn picture(&self, plugin: &Plugin) -> Option<PathBuf> {
        let picture = plugin.media.picture.as_ref()?;
        let path = self.dir.join("media").join(format!("{}.png", plugin.id));
        if path.is_file() && fetch::sha256_file(&path).is_ok_and(|s| s == picture.sha256) {
            return Some(path);
        }
        let result = (|| {
            let bytes = fetch::get(&picture.url, MAX_PICTURE_BYTES)?;
            if fetch::sha256(&bytes) != picture.sha256 {
                bail!("checksum mismatch");
            }
            fs::create_dir_all(path.parent().unwrap())?;
            crate::persist::write_atomic(&path, &bytes)
        })();
        match result {
            Ok(()) => Some(path),
            Err(e) => {
                tracing::warn!("Picture of {} not downloaded: {e:#}", plugin.id);
                None
            }
        }
    }

    /// Drop cached archives and pictures the index no longer lists
    fn prune_cache(&self, index: &Index) {
        let keep: Vec<String> = index
            .packages
            .iter()
            .flat_map(|p| p.artifacts.values())
            .map(|a| format!("{}.{}", a.sha256, a.format))
            .collect();
        prune(&self.dir.join("cache"), |name| {
            keep.iter().any(|k| k == name)
        });
        let pictures: Vec<String> = index
            .entries()
            .map(|(_, p)| format!("{}.png", p.id))
            .collect();
        prune(&self.dir.join("media"), |name| {
            pictures.iter().any(|k| k == name)
        });
    }
}

fn prune(dir: &Path, keep: impl Fn(&str) -> bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !keep(&entry.file_name().to_string_lossy()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use install::Receipt;

    fn index() -> Index {
        let text = include_str!("test_index.json");
        Index::parse(text.as_bytes()).unwrap()
    }

    fn receipt(version: &str) -> Receipt {
        Receipt {
            package: "zam-plugins".into(),
            version: version.into(),
            origin: "upstream".into(),
            license: "GPL-2.0-or-later".into(),
            source: String::new(),
            sha256: String::new(),
            bundle: "ZamComp.lv2".into(),
            plugins: vec!["zam-comp".into()],
            installed: String::new(),
        }
    }

    #[test]
    fn statuses() {
        let index = index();
        let (package, plugin) = index.find("zam-comp").unwrap();
        let here = package.artifact().is_some();
        let installed = |r: Option<Receipt>| {
            vec![Installed {
                bundle: "ZamComp.lv2".into(),
                receipt: r,
            }]
        };
        assert_eq!(
            status(package, plugin, &installed(Some(receipt("4.5"))), true),
            Status::Installed
        );
        assert_eq!(
            status(package, plugin, &installed(Some(receipt("4.4"))), true),
            Status::Update {
                installed: "4.4".into()
            }
        );
        assert_eq!(
            status(package, plugin, &installed(None), true),
            Status::Manual
        );
        assert_eq!(status(package, plugin, &[], true), Status::Manual);
        let fresh = status(package, plugin, &[], false);
        if here {
            assert_eq!(fresh, Status::Available);
        } else {
            assert!(matches!(fresh, Status::Unavailable(_)));
        }
    }
}
