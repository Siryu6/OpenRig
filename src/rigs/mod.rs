//! Shareable rigs (`.openrig` files, format in the `openrig-rig` crate):
//! a preset with the manifest of the plugins and models it needs.
//!
//! - `export`: from a preset, resolve every plugin (built-in block, store
//!   package, else local) and every model file (TONE3000 tone and model
//!   ids from `tone.json`, else local) and build the rig.
//! - `import`: read a rig, plan what is there and what must be installed
//!   or downloaded, then install it and turn it into a preset with paths
//!   of this pedalboard.
//!
//! Rigs live in `<data>/rigs`; removable drives mounted by the desktop
//! are listed too.

pub mod export;
pub mod import;

pub use openrig_rig as format;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tracing::warn;

use format::{Rig, FILE_EXTENSION, MAX_RIG_BYTES};

/// Where rigs are exported to and imported from
pub fn rigs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("rigs")
}

/// A plugin of the store catalog, as rigs need it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPlugin {
    pub uri: String,
    /// Catalog package id
    pub package: String,
    /// Package version of the catalog
    pub version: String,
    /// Package version installed by the store here, if it is
    pub installed_version: Option<String>,
    /// Catalog entry id
    pub plugin_id: String,
}

/// The plugins of a store index, with the versions the store installed
#[cfg(feature = "store")]
pub fn catalog_plugins(
    index: &crate::store::index::Index,
    installed: &[crate::store::install::Installed],
) -> Vec<CatalogPlugin> {
    index
        .entries()
        .map(|(package, plugin)| CatalogPlugin {
            uri: plugin.uri.clone(),
            package: package.id.clone(),
            version: package.version.clone(),
            installed_version: installed
                .iter()
                .filter(|i| i.bundle == plugin.bundle)
                .filter_map(|i| i.receipt.as_ref())
                .find(|r| r.package == package.id)
                .map(|r| r.version.clone()),
            plugin_id: plugin.id.clone(),
        })
        .collect()
}

/// Removable drives mounted by the system, where rigs can be copied to and
/// read from: `/media/<user>/<drive>`, `/run/media/<user>/<drive>` (Linux
/// desktops), `/Volumes/<drive>` (macOS). Empty elsewhere (Windows, the
/// OpenRig OS, which mounts no drive by itself).
pub fn removable_drives() -> Vec<PathBuf> {
    let mut drives = Vec::new();
    if cfg!(target_os = "macos") {
        drives.extend(subdirs(Path::new("/Volumes")));
        // The system disk is listed there as a link to /
        drives.retain(|d| std::fs::canonicalize(d).is_ok_and(|c| c != Path::new("/")));
    } else if cfg!(target_os = "linux") {
        for root in ["/media", "/run/media"] {
            for dir in subdirs(Path::new(root)) {
                // /media/<drive> on some systems, /media/<user>/<drive> on others
                if is_mount_point(&dir) {
                    drives.push(dir);
                } else {
                    drives.extend(subdirs(&dir).into_iter().filter(|d| is_mount_point(d)));
                }
            }
        }
    }
    drives.sort();
    drives
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// A folder on another device than its parent
#[cfg(unix)]
fn is_mount_point(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(own), Some(Ok(parent))) = (std::fs::metadata(dir), dir.parent().map(std::fs::metadata))
    else {
        return false;
    };
    own.dev() != parent.dev()
}

#[cfg(not(unix))]
fn is_mount_point(_dir: &Path) -> bool {
    false
}

/// A rig file found in a folder
#[derive(Debug, Clone)]
pub struct RigFile {
    pub path: PathBuf,
    /// On a removable drive
    pub removable: bool,
    /// The rig, or why it cannot be read
    pub rig: Result<Rig, String>,
}

impl RigFile {
    /// Rig name, else the file name
    pub fn name(&self) -> String {
        match &self.rig {
            Ok(rig) if !rig.meta.name.trim().is_empty() => rig.meta.name.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// Read a rig file (size checked before reading)
pub fn read(path: &Path) -> Result<Rig> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Cannot read {}", path.display()))?
        .len();
    if size > MAX_RIG_BYTES as u64 {
        bail!("{} is too large to be a rig", path.display());
    }
    let bytes = std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
    Ok(Rig::from_slice(&bytes)?)
}

/// The rig files right in `dir` (not in subfolders), sorted by name
pub fn list(dir: &Path, removable: bool) -> Vec<RigFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<RigFile> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(FILE_EXTENSION))
        })
        .map(|path| {
            let rig = read(&path).map_err(|e| {
                warn!(path = %path.display(), error = format_args!("{e:#}"), "Rig unreadable");
                format!("{e:#}")
            });
            RigFile {
                path,
                removable,
                rig,
            }
        })
        .collect();
    files.sort_by_key(|f| f.name().to_lowercase());
    files
}

/// Write `rig` into `dir` as `<name>.openrig`, `<name> (2).openrig`… when
/// the name is taken; returns the path
pub fn write(dir: &Path, rig: &Rig) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let base = crate::tone_info::file_name(&rig.meta.name);
    let mut path = dir.join(format!("{base}.{FILE_EXTENSION}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base} ({n}).{FILE_EXTENSION}"));
        n += 1;
    }
    crate::persist::write_atomic(&path, &rig.to_vec_pretty()?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use format::Meta;

    #[test]
    fn writes_and_lists_rigs() {
        let dir = std::env::temp_dir().join(format!("pb-rigs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rig = Rig::new(
            Meta::named("Lead / Solo"),
            serde_json::json!({"name": "x"}),
            Default::default(),
        );
        let first = write(&dir, &rig).unwrap();
        let second = write(&dir, &rig).unwrap();
        assert_eq!(first.file_name().unwrap(), "Lead - Solo.openrig");
        assert_eq!(second.file_name().unwrap(), "Lead - Solo (2).openrig");
        std::fs::write(dir.join("broken.openrig"), "{").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let files = list(&dir, false);
        assert_eq!(files.len(), 3);
        assert_eq!(files.iter().filter(|f| f.rig.is_ok()).count(), 2);
        assert_eq!(files[0].name(), "broken");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
