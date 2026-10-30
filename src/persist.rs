//! Files written so a power cut keeps either the previous or the new
//! version: through a temporary file, synced, then renamed.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Write `contents` to `path` through a temporary file and a rename
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let file = (|| {
        let mut file = fs::File::create(tmp)?;
        std::io::Write::write_all(&mut file, contents)?;
        file.sync_all()
    })();
    if let Err(e) = file {
        let _ = fs::remove_file(tmp);
        return Err(e).with_context(|| format!("Cannot write {}", tmp.display()));
    }
    fs::rename(tmp, path).with_context(|| format!("Cannot write {}", path.display()))?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Make a new, renamed or deleted file in `dir` survive a power cut (best
/// effort: directories cannot be synced on Windows)
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Save `value` as pretty JSON, creating its directory
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    }
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}

/// The JSON file at `path`; `None` if there is none
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(json) => Ok(Some(serde_json::from_slice(&json)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("pedalboard-persist-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sub").join("value.json");
        assert_eq!(load_json::<Vec<u32>>(&path).unwrap(), None);
        save_json(&path, &vec![1u32, 2]).unwrap();
        save_json(&path, &vec![3u32]).unwrap();
        assert_eq!(load_json::<Vec<u32>>(&path).unwrap(), Some(vec![3]));
        assert!(!dir.join("sub").join("value.json.tmp").exists());
        fs::write(&path, "not json").unwrap();
        assert!(load_json::<Vec<u32>>(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
