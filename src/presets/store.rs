//! Presets and scenes on disk: one JSON file per item, named after its id
//! (names can repeat), written atomically so a power cut keeps the previous
//! version.
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

pub use crate::persist::write_atomic;

/// File of the item `id` in `dir`
pub fn file_for(dir: &Path, id: &str) -> PathBuf {
    let name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}.json"))
}

/// Every item of `dir` with the file it came from; unreadable files are
/// skipped with a warning
pub fn load_dir<T: DeserializeOwned>(dir: &Path) -> Result<Vec<(PathBuf, T)>> {
    fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let mut items = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let item = fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(serde_json::from_str(&json)?));
        match item {
            Ok(item) => items.push((path, item)),
            Err(e) => warn!("Skipping {}: {}", path.display(), e),
        }
    }
    Ok(items)
}

/// Save `item` as `id` in `dir`, removing the file it was loaded from if it
/// had another name (files used to be named after the item)
pub fn save<T: serde::Serialize>(
    dir: &Path,
    id: &str,
    item: &T,
    previous: Option<&Path>,
) -> Result<PathBuf> {
    let path = file_for(dir, id);
    write_atomic(&path, serde_json::to_string_pretty(item)?.as_bytes())?;
    if let Some(previous) = previous.filter(|p| *p != path) {
        let _ = fs::remove_file(previous);
    }
    Ok(path)
}

/// Writes one file from a background thread, so a slow card (fsync) never
/// holds up the caller. Only the latest contents queued are written.
pub struct BackgroundWriter {
    path: PathBuf,
    shared: std::sync::Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: std::sync::Mutex<WriterState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct WriterState {
    pending: Option<Vec<u8>>,
    writing: bool,
    started: bool,
    /// Error of the last write, if it failed
    error: Option<String>,
}

impl BackgroundWriter {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            shared: Default::default(),
        }
    }

    /// Write `contents` soon, replacing whatever is still waiting
    pub fn queue(&self, contents: Vec<u8>) {
        let mut state = self.shared.state.lock().unwrap();
        state.pending = Some(contents);
        if !state.started {
            state.started = true;
            let (path, shared) = (self.path.clone(), self.shared.clone());
            let spawned = std::thread::Builder::new()
                .name("writer".into())
                .spawn(move || write_loop(&path, &shared));
            if let Err(e) = spawned {
                warn!("Cannot start the writer for {}: {}", self.path.display(), e);
                state.started = false;
            }
        }
        self.shared.changed.notify_all();
    }

    /// Wait until everything queued is written; the error of the last write
    pub fn flush(&self) -> Result<()> {
        let mut state = self.shared.state.lock().unwrap();
        if !state.started {
            // No thread (it could not start): write here
            if let Some(contents) = state.pending.take() {
                return write_atomic(&self.path, &contents);
            }
        }
        while state.pending.is_some() || state.writing {
            state = self.shared.changed.wait(state).unwrap();
        }
        match &state.error {
            Some(e) => Err(anyhow::anyhow!("{e}")),
            None => Ok(()),
        }
    }
}

fn write_loop(path: &Path, shared: &std::sync::Arc<Shared>) {
    let mut state = shared.state.lock().unwrap();
    loop {
        let Some(contents) = state.pending.take() else {
            // Nobody else holds it: the writer is gone
            if std::sync::Arc::strong_count(shared) == 1 {
                return;
            }
            state = shared
                .changed
                .wait_timeout(state, std::time::Duration::from_secs(1))
                .unwrap()
                .0;
            continue;
        };
        state.writing = true;
        drop(state);
        let result = write_atomic(path, &contents);
        if let Err(e) = &result {
            warn!("{:#}", e);
        }
        state = shared.state.lock().unwrap();
        state.writing = false;
        state.error = result.err().map(|e| format!("{e:#}"));
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_saved_by_id_and_old_files_replaced() {
        let dir = std::env::temp_dir().join(format!("pb-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("Clean.json");
        fs::write(&old, "1").unwrap();
        fs::write(dir.join("broken.json"), "{").unwrap();

        let path = save(&dir, "a/b", &2, Some(&old)).unwrap();
        assert_eq!(path, dir.join("a_b.json"));
        assert!(!old.exists());
        let items: Vec<(PathBuf, i32)> = load_dir(&dir).unwrap();
        assert_eq!(items, vec![(path, 2)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn background_writer_writes_the_latest_contents() {
        let dir = std::env::temp_dir().join(format!("pb-writer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let writer = BackgroundWriter::new(path.clone());
        for n in 0..20 {
            writer.queue(format!("{n}").into_bytes());
        }
        writer.flush().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "19");

        // A failed write is reported by flush
        fs::remove_dir_all(&dir).unwrap();
        writer.queue(b"lost".to_vec());
        assert!(writer.flush().is_err());
    }
}
