//! Full backup and restore of the data directory: one `.tar.gz` file with
//! a manifest, written to a USB drive (or kept on the pedal), and read back
//! to bring a pedalboard to the same state after a new card, a reinstall or
//! a move to another board.
//!
//! - **Contents**: presets, setlists, rigs, saved loops and the settings
//!   files (session, routing, hardware, network MIDI, control layouts).
//!   Models and LV2 plugins only when asked (they can be large); otherwise
//!   the manifest lists the store plugins and TONE3000 models in use, so
//!   they can be downloaded again after a restore.
//! - **Never in a backup**: secrets (remote control password, TONE3000 and
//!   openrig.eu accounts; Wi-Fi keys live outside the data directory),
//!   the store cache, update downloads and the backups themselves.
//! - **Restore**: the archive is checked (manifest version, plain relative
//!   paths under the parts it declares, regular files only, size and count
//!   caps, every listed file present) and unpacked aside (`.restore/staging`),
//!   then marked pending. The swap happens at the next start, before the
//!   app reads anything: the parts it covers move to `.restore/undo` and the
//!   restored ones take their place, by renames, in an order that a power
//!   cut can resume. Undo stages the previous data the same way.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::rigs::format::is_safe_relative_path;

/// Version of the archive layout; newer ones are refused
pub const FORMAT: u32 = 1;
/// First entry of every archive
pub const MANIFEST: &str = "openrig-backup.json";
/// Backup file names: `openrig-backup-<date>-<time>.tar.gz`
pub const PREFIX: &str = "openrig-backup";
pub const EXTENSION: &str = ".tar.gz";
/// Backups kept on the pedal when no drive is mounted
pub const LOCAL_DIR: &str = "backups";
/// Local backups kept (the oldest go first)
pub const LOCAL_KEPT: usize = 3;
/// Restore work area in the data directory
const WORK_DIR: &str = ".restore";
const STAGING: &str = "staging";
const PENDING: &str = "pending";
const UNDO: &str = "undo";
const UNDO_NEXT: &str = "undo.next";
/// Inside `undo.next`: the current data has been moved aside
const PHASE: &str = ".moved-out";
/// What the last restore listed as needed (store plugins, TONE3000 models)
const LAST: &str = "last.json";

/// Caps on what an archive may hold
const MAX_MANIFEST_BYTES: u64 = 8 << 20;
const MAX_FILES: usize = 200_000;
const MAX_FILE_BYTES: u64 = 4 << 30;
const MAX_TOTAL_BYTES: u64 = 64 << 30;
/// Largest backup sent to or received from the web remote (held in memory)
pub const MAX_REMOTE_BYTES: u64 = 512 << 20;

/// A part of the data directory a backup can hold
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Settings,
    Presets,
    Setlists,
    Rigs,
    Loops,
    Models,
    Plugins,
}

impl Part {
    pub const ALL: [Part; 7] = [
        Part::Settings,
        Part::Presets,
        Part::Setlists,
        Part::Rigs,
        Part::Loops,
        Part::Models,
        Part::Plugins,
    ];
    /// Always in a backup
    pub const DEFAULT: [Part; 5] = [
        Part::Settings,
        Part::Presets,
        Part::Setlists,
        Part::Rigs,
        Part::Loops,
    ];

    /// Names right in the data directory that make up the part
    pub fn items(self) -> &'static [&'static str] {
        match self {
            Part::Settings => &[
                "session.json",
                "param-layout.json",
                "hardware.json",
                "routing.json",
                "network-midi.json",
            ],
            Part::Presets => &["presets"],
            Part::Setlists => &["scenes"],
            Part::Rigs => &["rigs"],
            Part::Loops => &["loops"],
            Part::Models => &["models"],
            Part::Plugins => &["lv2"],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Part::Settings => "Settings",
            Part::Presets => "Presets",
            Part::Setlists => "Setlists",
            Part::Rigs => "Rigs",
            Part::Loops => "Loops",
            Part::Models => "Models",
            Part::Plugins => "Plugins",
        }
    }

    fn of(item: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|p| p.items().contains(&item))
    }
}

/// Never written to a backup, never restored: secrets, caches, downloads
pub const EXCLUDED: [&str; 3] = ["remote.json", "tone3000.json", "openrig.json"];

/// A store plugin bundle in use, to install again
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorePlugin {
    pub package: String,
    pub version: String,
    /// Folder in `lv2/`
    pub bundle: String,
}

/// A TONE3000 model in use, to download again
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tone3000File {
    /// Path in the models folder
    pub path: String,
    pub tone_id: u64,
    pub model_id: u64,
    pub name: String,
}

/// What a backup holds, first entry of the archive
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub app_version: String,
    /// RFC 3339
    pub created: String,
    /// CPU architecture of the pedalboard ("aarch64", "x86_64"): plugins
    /// built for another one are not restored
    pub arch: String,
    pub parts: Vec<Part>,
    /// Every file of the archive, relative to the data directory
    pub files: Vec<String>,
    /// Bytes of those files
    #[serde(default)]
    pub bytes: u64,
    /// Store plugins and TONE3000 models in use, for a restore without
    /// models and plugins
    #[serde(default)]
    pub store_plugins: Vec<StorePlugin>,
    #[serde(default)]
    pub tone3000_models: Vec<Tone3000File>,
    /// Plugins copied by hand and models of one's own: lost by a restore
    /// on another board without models and plugins
    #[serde(default)]
    pub local_plugins: Vec<String>,
    #[serde(default)]
    pub local_models: Vec<String>,
    /// The data a restore replaced, kept for its undo
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undo: bool,
}

impl Manifest {
    pub fn has(&self, part: Part) -> bool {
        self.parts.contains(&part)
    }

    /// Files of `part` in the backup
    pub fn count(&self, part: Part) -> usize {
        self.files
            .iter()
            .filter(|f| top(f).and_then(Part::of) == Some(part))
            .count()
    }
}

fn top(path: &str) -> Option<&str> {
    path.split('/').next()
}

/// What goes in a backup
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub models: bool,
    pub plugins: bool,
}

impl Options {
    pub fn parts(self) -> Vec<Part> {
        let mut parts = Part::DEFAULT.to_vec();
        if self.models {
            parts.push(Part::Models);
        }
        if self.plugins {
            parts.push(Part::Plugins);
        }
        parts
    }
}

/// Every regular file under `dir` (relative, `/`-separated, sorted), with
/// sizes. Links, temporary files and unsafe names are skipped.
fn walk(root: &Path, rel: &str, out: &mut Vec<(String, u64)>) {
    let path = root.join(rel);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return;
    };
    if meta.is_file() {
        if !rel.ends_with(".tmp") && is_safe_relative_path(rel) {
            out.push((rel.to_string(), meta.len()));
        }
        return;
    }
    if !meta.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&path) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        walk(root, &format!("{rel}/{name}"), out);
    }
}

/// The files of `parts` in `data_dir`
pub fn files(data_dir: &Path, parts: &[Part]) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for part in parts {
        for item in part.items() {
            walk(data_dir, item, &mut out);
        }
    }
    out
}

/// Bytes of the models and of the plugins folder, for the UI
pub fn sizes(data_dir: &Path) -> (u64, u64) {
    let sum = |part| files(data_dir, &[part]).iter().map(|(_, n)| n).sum();
    (sum(Part::Models), sum(Part::Plugins))
}

/// Store plugins, hand-copied bundles, TONE3000 and own models in use
#[allow(clippy::type_complexity)]
fn needs(
    data_dir: &Path,
) -> (
    Vec<StorePlugin>,
    Vec<String>,
    Vec<Tone3000File>,
    Vec<String>,
) {
    let (mut store, mut local_plugins) = (Vec::new(), Vec::new());
    let lv2 = data_dir.join("lv2");
    let mut bundles: Vec<String> = fs::read_dir(&lv2)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|b| b.ends_with(".lv2") && !b.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    bundles.sort();
    #[derive(Deserialize)]
    struct Receipt {
        package: String,
        version: String,
    }
    for bundle in bundles {
        let receipt = fs::read(lv2.join(&bundle).join(".openrig.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Receipt>(&b).ok());
        match receipt {
            Some(r) => store.push(StorePlugin {
                package: r.package,
                version: r.version,
                bundle,
            }),
            None => local_plugins.push(bundle),
        }
    }

    let (mut tone3000, mut local_models) = (Vec::new(), Vec::new());
    let mut models = Vec::new();
    walk(data_dir, "models", &mut models);
    let info_file = crate::plugins::scanner::TONE_INFO_FILE;
    let picture = crate::plugins::scanner::TONE_PICTURE_FILE;
    for (path, _) in models {
        let rel = path.trim_start_matches("models/").to_string();
        let file = rel.rsplit('/').next().unwrap_or(&rel);
        if file == info_file || file == picture {
            continue;
        }
        let folder = Path::new(&path).parent().unwrap_or(Path::new(""));
        let tone = fs::read(data_dir.join(folder).join(info_file))
            .ok()
            .and_then(|b| serde_json::from_slice::<crate::tone_info::ToneInfo>(&b).ok())
            .filter(|t| t.id != 0);
        let model_id = tone.as_ref().and_then(|t| {
            t.models
                .iter()
                .find(|m| m.file == file)
                .map(|m| m.id)
                .or_else(|| crate::tone_info::model_id_in(file))
        });
        match (tone, model_id) {
            (Some(tone), Some(model_id)) => {
                let name = tone
                    .models
                    .iter()
                    .find(|m| m.id == model_id)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| {
                        let stem = Path::new(file).file_stem().and_then(|s| s.to_str());
                        let stem = stem.unwrap_or(file);
                        stem.rsplit_once(" [").map_or(stem, |(n, _)| n).to_string()
                    });
                tone3000.push(Tone3000File {
                    path: rel,
                    tone_id: tone.id,
                    model_id,
                    name,
                });
            }
            _ => local_models.push(rel),
        }
    }
    (store, local_plugins, tone3000, local_models)
}

/// The manifest of a backup of `parts` of `data_dir`, with its files
pub fn plan(data_dir: &Path, options: Options) -> (Manifest, Vec<(String, u64)>) {
    let parts = options.parts();
    let files = files(data_dir, &parts);
    let (store_plugins, local_plugins, tone3000_models, local_models) = needs(data_dir);
    let manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        arch: std::env::consts::ARCH.to_string(),
        parts,
        bytes: files.iter().map(|(_, n)| n).sum(),
        files: files.iter().map(|(f, _)| f.clone()).collect(),
        store_plugins,
        tone3000_models,
        local_plugins,
        local_models,
        undo: false,
    };
    (manifest, files)
}

/// Write a backup of `data_dir` as a `.tar.gz` to `out`
pub fn write(data_dir: &Path, options: Options, out: impl Write) -> Result<Manifest> {
    let (manifest, files) = plan(data_dir, options);
    let gz = flate2::write::GzEncoder::new(out, flate2::Compression::new(6));
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    let json = serde_json::to_vec_pretty(&manifest)?;
    let mut header = file_header(json.len() as u64);
    tar.append_data(&mut header, MANIFEST, json.as_slice())?;
    for (path, _) in &files {
        // Files are replaced by renames: the open one stays whole
        let file =
            fs::File::open(data_dir.join(path)).with_context(|| format!("Cannot read {path}"))?;
        let len = file.metadata()?.len();
        let mut header = file_header(len);
        tar.append_data(&mut header, path, file.take(len))
            .with_context(|| format!("Cannot back up {path}"))?;
    }
    let gz = tar.into_inner()?;
    let mut out = gz.finish()?;
    out.flush()?;
    Ok(manifest)
}

/// A backup held in memory, for the web remote: refused past
/// `MAX_REMOTE_BYTES`
pub fn write_to_memory(data_dir: &Path, options: Options) -> Result<Vec<u8>> {
    struct Capped(Vec<u8>);
    impl Write for Capped {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if (self.0.len() + buf.len()) as u64 > MAX_REMOTE_BYTES {
                return Err(std::io::Error::other(
                    "Too large to download: back up to a USB drive instead",
                ));
            }
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Capped(Vec::new());
    write(data_dir, options, &mut out)?;
    Ok(out.0)
}

fn file_header(len: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(len);
    header.set_mode(0o644);
    header.set_mtime(0);
    header
}

/// File name of a new backup
pub fn file_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!("{PREFIX}-{}{EXTENSION}", now.format("%Y-%m-%d-%H%M%S"))
}

/// Write a backup into `dir` (a drive, or the local folder); returns its
/// path. Written aside then renamed, so a pulled drive leaves no half file
/// under the final name.
pub fn write_to_dir(data_dir: &Path, options: Options, dir: &Path) -> Result<(PathBuf, Manifest)> {
    fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let path = dir.join(file_name(chrono::Local::now()));
    let tmp = dir.join(format!(
        ".{}.part",
        path.file_name().and_then(|n| n.to_str()).unwrap_or(PREFIX)
    ));
    let result = (|| {
        let file =
            fs::File::create(&tmp).with_context(|| format!("Cannot write {}", tmp.display()))?;
        let mut writer = std::io::BufWriter::new(file);
        let manifest = write(data_dir, options, &mut writer)?;
        let file = writer
            .into_inner()
            .map_err(|e| anyhow!("Cannot write: {e}"))?;
        file.sync_all()?;
        fs::rename(&tmp, &path).with_context(|| format!("Cannot write {}", path.display()))?;
        crate::persist::sync_dir(dir);
        Ok(manifest)
    })();
    match result {
        Ok(manifest) => Ok((path, manifest)),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Keep the newest `LOCAL_KEPT` backups of the local folder
pub fn prune_local(data_dir: &Path) {
    let mut found = list_dir(&data_dir.join(LOCAL_DIR));
    found.sort();
    let extra = found.len().saturating_sub(LOCAL_KEPT);
    for old in found.into_iter().take(extra) {
        if let Err(e) = fs::remove_file(&old) {
            warn!(path = %old.display(), error = %e, "Old backup not removed");
        }
    }
}

/// Backup files right in `dir`
pub fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with(PREFIX) && n.to_ascii_lowercase().ends_with(EXTENSION)
                })
        })
        .collect()
}

/// Reads the manifest at the start of an archive and checks its version
pub fn read_manifest(archive: impl Read) -> Result<Manifest> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut entries = tar.entries().context("Not a backup file")?;
    let first = entries
        .next()
        .ok_or_else(|| anyhow!("The backup is empty"))?
        .context("Not a backup file")?;
    manifest_of(first)
}

fn manifest_of<R: Read>(mut entry: tar::Entry<R>) -> Result<Manifest> {
    if entry.path_bytes().as_ref() != MANIFEST.as_bytes() {
        bail!("Not an OpenRig backup");
    }
    if entry.size() > MAX_MANIFEST_BYTES {
        bail!("The backup's manifest is too large");
    }
    let mut json = Vec::new();
    entry
        .by_ref()
        .take(MAX_MANIFEST_BYTES)
        .read_to_end(&mut json)
        .context("Not a backup file")?;
    let manifest: Manifest =
        serde_json::from_slice(&json).map_err(|e| anyhow!("Unreadable manifest: {e}"))?;
    check_manifest(&manifest)?;
    Ok(manifest)
}

fn check_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.format == 0 || manifest.format > FORMAT {
        bail!(
            "Made by a newer OpenRig ({}): update this one first",
            manifest.app_version
        );
    }
    if manifest.files.len() > MAX_FILES {
        bail!("The backup holds too many files");
    }
    if manifest.parts.is_empty() {
        bail!("The backup holds nothing");
    }
    for path in &manifest.files {
        allowed(path, &manifest.parts)?;
    }
    Ok(())
}

/// A path of the archive: plain, relative, in one of the parts declared,
/// never a secret
fn allowed(path: &str, parts: &[Part]) -> Result<()> {
    if !is_safe_relative_path(path) {
        bail!("Unsafe path in the backup: {}", path.escape_debug());
    }
    let first = top(path).unwrap_or_default();
    if EXCLUDED.contains(&first) {
        bail!("The backup holds a file it must not: {first}");
    }
    match Part::of(first) {
        Some(part) if parts.contains(&part) => Ok(()),
        _ => bail!("Unexpected file in the backup: {}", path.escape_debug()),
    }
}

fn work(data_dir: &Path) -> PathBuf {
    data_dir.join(WORK_DIR)
}

fn remove_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Cannot remove {}", dir.display())),
    }
}

/// Check and unpack `archive` aside, then mark it to be swapped in at the
/// next start. Nothing of the current data changes. Plugins built for
/// another CPU are left out (they are installed again from the store).
pub fn stage(data_dir: &Path, archive: impl Read) -> Result<Manifest> {
    // One at a time (the pedal and the web remote)
    static STAGING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = STAGING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let work = work(data_dir);
    let staging = work.join(STAGING);
    remove_dir(&staging)?;
    fs::create_dir_all(&staging).with_context(|| format!("Cannot create {}", staging.display()))?;
    match unpack(&staging, archive) {
        Ok(mut manifest) => {
            if manifest.has(Part::Plugins) && manifest.arch != std::env::consts::ARCH {
                info!(arch = %manifest.arch, "Backup plugins built for another CPU: not restored");
                manifest.parts.retain(|p| *p != Part::Plugins);
                manifest.files.retain(|f| top(f) != Some("lv2"));
                remove_dir(&staging.join("lv2"))?;
            }
            crate::persist::write_atomic(
                &staging.join(MANIFEST),
                &serde_json::to_vec_pretty(&manifest)?,
            )?;
            let pending = work.join(PENDING);
            remove_dir(&pending)?;
            fs::rename(&staging, &pending).context("Cannot prepare the restore")?;
            crate::persist::sync_dir(&work);
            info!(
                created = %manifest.created,
                files = manifest.files.len(),
                "Backup ready to restore at the next start"
            );
            Ok(manifest)
        }
        Err(e) => {
            let _ = remove_dir(&staging);
            Err(e)
        }
    }
}

fn unpack(staging: &Path, archive: impl Read) -> Result<Manifest> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut entries = tar.entries().context("Not a backup file")?;
    let first = entries
        .next()
        .ok_or_else(|| anyhow!("The backup is empty"))?
        .context("Not a backup file")?;
    let manifest = manifest_of(first)?;
    let expected: BTreeSet<&str> = manifest.files.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for entry in entries {
        let mut entry = entry.context("The backup file is damaged")?;
        let raw = entry.path_bytes().into_owned();
        let path = std::str::from_utf8(&raw)
            .map_err(|_| anyhow!("Unreadable file name in the backup"))?
            .trim_end_matches('/')
            .to_string();
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Directory => {
                allowed(&path, &manifest.parts)?;
                continue;
            }
            other => bail!(
                "The backup holds a link or a special file ({other:?}): {}",
                path.escape_debug()
            ),
        }
        allowed(&path, &manifest.parts)?;
        if !expected.contains(path.as_str()) {
            bail!("File not in the manifest: {}", path.escape_debug());
        }
        if !seen.insert(path.clone()) {
            bail!("File twice in the backup: {}", path.escape_debug());
        }
        let size = entry.size();
        total += size;
        if size > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
            bail!("The backup is too large");
        }
        let target = staging.join(&path);
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
        }
        let mut out = fs::File::create(&target)
            .with_context(|| format!("Cannot write {}", target.display()))?;
        let copied = std::io::copy(&mut (&mut entry).take(size), &mut out)
            .with_context(|| format!("The backup file is damaged ({path})"))?;
        if copied != size {
            bail!("The backup file is cut short ({path})");
        }
        out.sync_all()?;
    }
    if seen.len() != expected.len() {
        let missing = expected.iter().find(|f| !seen.contains(**f)).copied();
        bail!(
            "The backup file is incomplete{}",
            missing
                .map(|m| format!(" ({m} missing)"))
                .unwrap_or_default()
        );
    }
    Ok(manifest)
}

/// What the last restore listed, to get again
pub fn last_restore(data_dir: &Path) -> Option<Manifest> {
    crate::persist::load_json(&work(data_dir).join(LAST))
        .ok()
        .flatten()
}

/// Store plugins and TONE3000 models the last restore listed that are not
/// on this pedalboard
pub fn missing(data_dir: &Path) -> (Vec<StorePlugin>, Vec<Tone3000File>) {
    let Some(last) = last_restore(data_dir) else {
        return (Vec::new(), Vec::new());
    };
    let plugins = last
        .store_plugins
        .into_iter()
        .filter(|p| {
            is_safe_relative_path(&p.bundle) && !data_dir.join("lv2").join(&p.bundle).is_dir()
        })
        .collect();
    let models = last
        .tone3000_models
        .into_iter()
        .filter(|m| {
            is_safe_relative_path(&m.path) && !data_dir.join("models").join(&m.path).is_file()
        })
        .collect();
    (plugins, models)
}

/// A restore waits for the next start
pub fn pending(data_dir: &Path) -> bool {
    work(data_dir).join(PENDING).join(MANIFEST).is_file()
}

/// The previous data of the last restore can be brought back
pub fn can_undo(data_dir: &Path) -> bool {
    work(data_dir).join(UNDO).join(MANIFEST).is_file()
}

/// Created date of the undo's data (when it was moved aside)
pub fn undo_manifest(data_dir: &Path) -> Option<Manifest> {
    crate::persist::load_json(&work(data_dir).join(UNDO).join(MANIFEST))
        .ok()
        .flatten()
}

/// Mark the previous data of the last restore to be swapped back in at the
/// next start (the data restored goes to the undo in turn)
pub fn stage_undo(data_dir: &Path) -> Result<()> {
    let work = work(data_dir);
    if !can_undo(data_dir) {
        bail!("Nothing to undo");
    }
    let pending = work.join(PENDING);
    remove_dir(&pending)?;
    fs::rename(work.join(UNDO), &pending).context("Cannot prepare the undo")?;
    crate::persist::sync_dir(&work);
    Ok(())
}

/// Drop a restore not applied yet
pub fn cancel_pending(data_dir: &Path) -> Result<()> {
    remove_dir(&work(data_dir).join(PENDING))
}

/// At start, before anything is read: swap a pending restore in. The parts
/// it covers move to the undo folder, then the restored ones take their
/// place. Resumes after a power cut at any point.
pub fn apply_pending(data_dir: &Path) -> Result<Option<Manifest>> {
    let work = work(data_dir);
    let pending = work.join(PENDING);
    let next = work.join(UNDO_NEXT);
    if !pending.join(MANIFEST).is_file() {
        // Cut after the restore, before the undo folder took its name
        if next.join(PHASE).is_file() && next.join(MANIFEST).is_file() {
            finish_undo(&work)?;
        }
        return Ok(None);
    }
    let manifest: Manifest = crate::persist::load_json(&pending.join(MANIFEST))?
        .ok_or_else(|| anyhow!("Restore manifest missing"))?;
    check_manifest(&manifest)?;
    let items: Vec<&str> = manifest
        .parts
        .iter()
        .flat_map(|p| p.items().iter().copied())
        .collect();
    fs::create_dir_all(&next).with_context(|| format!("Cannot create {}", next.display()))?;
    if !next.join(PHASE).is_file() {
        for item in &items {
            let (from, to) = (data_dir.join(item), next.join(item));
            if fs::symlink_metadata(&from).is_ok() && fs::symlink_metadata(&to).is_err() {
                fs::rename(&from, &to)
                    .with_context(|| format!("Cannot move {} aside", from.display()))?;
            }
        }
        crate::persist::sync_dir(data_dir);
        crate::persist::write_atomic(&next.join(PHASE), b"")?;
    }
    for item in &items {
        let (from, to) = (pending.join(item), data_dir.join(item));
        if fs::symlink_metadata(&from).is_ok() {
            fs::rename(&from, &to).with_context(|| format!("Cannot restore {item}"))?;
        }
    }
    crate::persist::sync_dir(data_dir);
    // The undo brings back the parts this restore replaced
    let kept = files(&next, &manifest.parts);
    let undo = Manifest {
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        bytes: kept.iter().map(|(_, n)| n).sum(),
        files: kept.into_iter().map(|(f, _)| f).collect(),
        store_plugins: Vec::new(),
        tone3000_models: Vec::new(),
        local_plugins: Vec::new(),
        local_models: Vec::new(),
        undo: true,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        ..manifest.clone()
    };
    crate::persist::write_atomic(&next.join(MANIFEST), &serde_json::to_vec_pretty(&undo)?)?;
    // What is left to download: from this restore only
    if !manifest.store_plugins.is_empty() || !manifest.tone3000_models.is_empty() {
        crate::persist::save_json(&work.join(LAST), &manifest)?;
    } else {
        let _ = fs::remove_file(work.join(LAST));
    }
    remove_dir(&pending)?;
    finish_undo(&work)?;
    info!(
        created = %manifest.created,
        parts = ?manifest.parts,
        "Backup restored; the previous data is kept for one undo"
    );
    Ok(Some(manifest))
}

fn finish_undo(work: &Path) -> Result<()> {
    let (next, undo) = (work.join(UNDO_NEXT), work.join(UNDO));
    remove_dir(&undo)?;
    fs::rename(&next, &undo).context("Cannot keep the previous data")?;
    let _ = fs::remove_file(undo.join(PHASE));
    crate::persist::sync_dir(work);
    Ok(())
}

/// Short size for people: "820 KB", "1.4 GB"
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openrig-backup-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, path: &str, contents: &[u8]) {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A data directory with a bit of everything
    fn sample(dir: &Path) {
        put(dir, "session.json", br#"{"preset":"Lead"}"#);
        put(dir, "routing.json", b"{}");
        put(dir, "hardware.json", b"{}");
        put(dir, "presets/Lead.json", b"{\"name\":\"Lead\"}");
        put(dir, "presets/Clean.json", b"{\"name\":\"Clean\"}");
        put(dir, "scenes/gig.json", b"{}");
        put(dir, "rigs/Lead.openrig", b"{}");
        put(dir, "loops/l1.wav", &[1u8; 5000]);
        put(dir, "loops/l1.json", b"{}");
        put(dir, "remote.json", b"{\"password_hash\":\"secret\"}");
        put(dir, "tone3000.json", b"{\"token\":\"secret\"}");
        put(dir, "openrig.json", b"{\"token\":\"secret\"}");
        put(dir, "store/catalog-v1.json", b"{}");
        put(dir, "update/app.raucb", b"x");
        put(
            dir,
            "models/Tone3000/Amps/Plexi [12]/tone.json",
            br#"{"id":12,"title":"Plexi","models":[{"id":34,"name":"Crunch","file":"Crunch [34].nam"}]}"#,
        );
        put(
            dir,
            "models/Tone3000/Amps/Plexi [12]/Crunch [34].nam",
            b"nam",
        );
        put(dir, "models/Mine/own.nam", b"own");
        put(
            dir,
            "lv2/zam.lv2/.openrig.json",
            br#"{"package":"zam-plugins","version":"4.4","origin":"","license":"","source":"","bundle":"zam.lv2","plugins":[],"installed":""}"#,
        );
        put(dir, "lv2/zam.lv2/manifest.ttl", b"ttl");
        put(dir, "lv2/hand.lv2/manifest.ttl", b"ttl");
    }

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for part in Part::ALL {
            for item in part.items() {
                let mut found = Vec::new();
                walk(dir, item, &mut found);
                for (f, _) in found {
                    let bytes = fs::read(dir.join(&f)).unwrap();
                    out.push((f, bytes));
                }
            }
        }
        out.sort();
        out
    }

    fn backup(dir: &Path, options: Options) -> Vec<u8> {
        let mut bytes = Vec::new();
        write(dir, options, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn round_trip_into_a_fresh_data_dir() {
        let (src, dst) = (temp("src"), temp("dst"));
        sample(&src);
        let options = Options {
            models: true,
            plugins: true,
        };
        let bytes = backup(&src, options);
        let manifest = read_manifest(bytes.as_slice()).unwrap();
        assert_eq!(manifest.parts, Part::ALL.to_vec());
        assert!(manifest.files.iter().all(|f| !f.contains("secret")));
        for secret in EXCLUDED {
            assert!(!manifest.files.iter().any(|f| f == secret));
        }
        assert!(!manifest
            .files
            .iter()
            .any(|f| f.starts_with("store/") || f.starts_with("update/")));

        stage(&dst, bytes.as_slice()).unwrap();
        assert!(pending(&dst));
        assert!(tree(&dst).is_empty(), "nothing changes before the restart");
        let applied = apply_pending(&dst).unwrap().unwrap();
        assert_eq!(applied.files, manifest.files);
        assert_eq!(tree(&src), tree(&dst));
        assert!(!pending(&dst));
        assert!(!dst.join("remote.json").exists());
        fs::remove_dir_all(src).unwrap();
        fs::remove_dir_all(dst).unwrap();
    }

    #[test]
    fn default_backup_lists_what_to_download_again() {
        let dir = temp("needs");
        sample(&dir);
        let bytes = backup(&dir, Options::default());
        let manifest = read_manifest(bytes.as_slice()).unwrap();
        assert_eq!(manifest.parts, Part::DEFAULT.to_vec());
        assert!(!manifest
            .files
            .iter()
            .any(|f| f.starts_with("models/") || f.starts_with("lv2/")));
        assert_eq!(
            manifest.store_plugins,
            vec![StorePlugin {
                package: "zam-plugins".into(),
                version: "4.4".into(),
                bundle: "zam.lv2".into()
            }]
        );
        assert_eq!(manifest.local_plugins, vec!["hand.lv2".to_string()]);
        assert_eq!(
            manifest.tone3000_models,
            vec![Tone3000File {
                path: "Tone3000/Amps/Plexi [12]/Crunch [34].nam".into(),
                tone_id: 12,
                model_id: 34,
                name: "Crunch".into()
            }]
        );
        assert_eq!(manifest.local_models, vec!["Mine/own.nam".to_string()]);
        assert_eq!(manifest.count(Part::Presets), 2);

        // Restored on a fresh pedalboard: those are missing until downloaded
        let fresh = temp("needs-fresh");
        stage(&fresh, bytes.as_slice()).unwrap();
        apply_pending(&fresh).unwrap();
        let (plugins, models) = missing(&fresh);
        assert_eq!(plugins.len(), 1);
        assert_eq!(models.len(), 1);
        put(
            &fresh,
            "models/Tone3000/Amps/Plexi [12]/Crunch [34].nam",
            b"nam",
        );
        assert!(missing(&fresh).1.is_empty());
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(fresh).unwrap();
    }

    #[test]
    fn restore_replaces_its_parts_keeps_the_rest_and_undoes() {
        let (src, dst) = (temp("src2"), temp("dst2"));
        sample(&src);
        put(&dst, "presets/Old.json", b"old");
        put(&dst, "session.json", b"old session");
        put(&dst, "remote.json", b"mine");
        put(&dst, "models/keep.nam", b"keep");
        let bytes = backup(&src, Options::default());
        stage(&dst, bytes.as_slice()).unwrap();
        apply_pending(&dst).unwrap();
        assert!(!dst.join("presets/Old.json").exists());
        assert!(dst.join("presets/Lead.json").exists());
        assert_eq!(fs::read(dst.join("remote.json")).unwrap(), b"mine");
        assert_eq!(fs::read(dst.join("models/keep.nam")).unwrap(), b"keep");
        assert!(can_undo(&dst));
        assert!(last_restore(&dst).is_some());

        stage_undo(&dst).unwrap();
        let undone = apply_pending(&dst).unwrap().unwrap();
        assert!(undone.undo);
        // Nothing left to download from the restore that was undone
        assert!(last_restore(&dst).is_none());
        assert_eq!(fs::read(dst.join("presets/Old.json")).unwrap(), b"old");
        assert_eq!(fs::read(dst.join("session.json")).unwrap(), b"old session");
        assert!(!dst.join("presets/Lead.json").exists());
        assert!(!dst.join("loops").exists());
        // The undo of the undo is the restore again
        assert!(can_undo(&dst));
        fs::remove_dir_all(src).unwrap();
        fs::remove_dir_all(dst).unwrap();
    }

    #[test]
    fn a_cut_during_the_swap_resumes() {
        let (src, dst) = (temp("src3"), temp("dst3"));
        sample(&src);
        put(&dst, "presets/Old.json", b"old");
        let bytes = backup(&src, Options::default());
        stage(&dst, bytes.as_slice()).unwrap();
        // Cut after the current data went aside, before the new came in
        let next = dst.join(WORK_DIR).join(UNDO_NEXT);
        fs::create_dir_all(&next).unwrap();
        fs::rename(dst.join("presets"), next.join("presets")).unwrap();
        fs::write(next.join(PHASE), b"").unwrap();
        apply_pending(&dst).unwrap();
        assert!(dst.join("presets/Lead.json").exists());
        let undo = dst.join(WORK_DIR).join(UNDO);
        assert_eq!(fs::read(undo.join("presets/Old.json")).unwrap(), b"old");
        fs::remove_dir_all(src).unwrap();
        fs::remove_dir_all(dst).unwrap();
    }

    /// An archive with the given entries after a manifest naming `files`
    fn crafted(files: &[&str], entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let manifest = Manifest {
            format: FORMAT,
            app_version: "0".into(),
            created: String::new(),
            arch: std::env::consts::ARCH.into(),
            parts: Part::DEFAULT.to_vec(),
            files: files.iter().map(|f| f.to_string()).collect(),
            bytes: 0,
            store_plugins: Vec::new(),
            tone3000_models: Vec::new(),
            local_plugins: Vec::new(),
            local_models: Vec::new(),
            undo: false,
        };
        let mut out = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let json = serde_json::to_vec(&manifest).unwrap();
            tar.append_data(
                &mut file_header(json.len() as u64),
                MANIFEST,
                json.as_slice(),
            )
            .unwrap();
            for (path, kind, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(*kind);
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                // Written raw: the builder refuses some of these names
                let name = header.as_old_mut();
                name.name[..path.len()].copy_from_slice(path.as_bytes());
                if *kind == tar::EntryType::Symlink {
                    header.set_link_name("/etc/passwd").unwrap();
                }
                header.set_cksum();
                tar.append(&header, *data).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        out
    }

    fn refused(bytes: &[u8]) -> String {
        let dst = temp("refused");
        put(&dst, "presets/Mine.json", b"mine");
        let err = stage(&dst, bytes).unwrap_err();
        assert!(!pending(&dst));
        assert!(!dst.join(WORK_DIR).join(STAGING).exists());
        assert_eq!(fs::read(dst.join("presets/Mine.json")).unwrap(), b"mine");
        fs::remove_dir_all(dst).unwrap();
        format!("{err:#}")
    }

    #[test]
    fn unsafe_and_damaged_archives_are_refused() {
        use tar::EntryType::{Regular, Symlink};
        let e = refused(&crafted(&["../evil"], &[("../evil", Regular, b"x")]));
        assert!(e.contains("Unsafe path"), "{e}");
        let e = refused(&crafted(&["presets/a.json"], &[("/etc/x", Regular, b"x")]));
        assert!(e.contains("Unsafe path"), "{e}");
        let e = refused(&crafted(
            &["presets/a.json"],
            &[("presets/../../x", Regular, b"x")],
        ));
        assert!(e.contains("Unsafe path"), "{e}");
        let e = refused(&crafted(
            &["remote.json"],
            &[("remote.json", Regular, b"x")],
        ));
        assert!(e.contains("must not"), "{e}");
        let e = refused(&crafted(
            &["presets/a.json"],
            &[("presets/a.json", Symlink, b"")],
        ));
        assert!(e.contains("link"), "{e}");
        let e = refused(&crafted(
            &["presets/a.json"],
            &[("models/x.nam", Regular, b"x")],
        ));
        assert!(e.contains("Unexpected"), "{e}");
        let e = refused(&crafted(
            &["presets/a.json", "presets/b.json"],
            &[("presets/a.json", Regular, b"x")],
        ));
        assert!(e.contains("incomplete"), "{e}");
        let e = refused(&crafted(
            &["presets/a.json"],
            &[("presets/b.json", Regular, b"x")],
        ));
        assert!(e.contains("not in the manifest"), "{e}");

        // Not gzip, cut short, newer format
        let e = refused(b"hello");
        assert!(e.contains("Not"), "{e}");
        let dir = temp("cut");
        sample(&dir);
        let whole = backup(&dir, Options::default());
        refused(&whole[..whole.len() / 2]);
        let mut newer = crafted(&[], &[]);
        newer.clear();
        {
            let gz = flate2::write::GzEncoder::new(&mut newer, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let json = br#"{"format":99,"app_version":"9.0","created":"","arch":"x","parts":["presets"],"files":[]}"#;
            tar.append_data(&mut file_header(json.len() as u64), MANIFEST, &json[..])
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let e = refused(&newer);
        assert!(e.contains("newer OpenRig"), "{e}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plugins_of_another_cpu_are_left_out() {
        let (src, dst) = (temp("src4"), temp("dst4"));
        sample(&src);
        let bytes = backup(
            &src,
            Options {
                models: false,
                plugins: true,
            },
        );
        // Rewrite the manifest's arch
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        let mut out = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            for entry in tar.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                if path == MANIFEST {
                    let mut m: Manifest = serde_json::from_slice(&data).unwrap();
                    m.arch = "other".into();
                    data = serde_json::to_vec(&m).unwrap();
                }
                builder
                    .append_data(&mut file_header(data.len() as u64), &path, data.as_slice())
                    .unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        put(&dst, "lv2/mine.lv2/manifest.ttl", b"mine");
        let manifest = stage(&dst, out.as_slice()).unwrap();
        assert!(!manifest.has(Part::Plugins));
        apply_pending(&dst).unwrap();
        assert!(dst.join("lv2/mine.lv2/manifest.ttl").exists());
        assert!(!dst.join("lv2/zam.lv2").exists());
        fs::remove_dir_all(src).unwrap();
        fs::remove_dir_all(dst).unwrap();
    }

    #[test]
    fn local_backups_keep_the_newest() {
        let dir = temp("local");
        for day in 1..=5 {
            put(
                &dir,
                &format!("backups/{PREFIX}-2026-11-0{day}-120000{EXTENSION}"),
                b"x",
            );
        }
        put(&dir, "backups/other.tar.gz", b"x");
        prune_local(&dir);
        let mut left = list_dir(&dir.join(LOCAL_DIR));
        left.sort();
        assert_eq!(left.len(), LOCAL_KEPT);
        assert!(left[0].to_string_lossy().contains("11-03"));
        assert!(dir.join("backups/other.tar.gz").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_400_000), "1.4 MB");
        assert_eq!(human_size(820_000), "820 KB");
        assert_eq!(human_size(2_300_000_000), "2.3 GB");
    }
}
