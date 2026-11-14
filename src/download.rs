//! Saving a download whose size and SHA-256 are known in advance (store
//! archives, update bundles): written next to its place as `<file>.part`,
//! checked, flushed to disk, then renamed. Nothing is left behind on failure.

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Save `body` to `path`, failing unless it is `size` bytes with SHA-256
/// `sha256` (hex); `progress` gets the bytes received and `size`. `what`
/// names the download in errors ("the plugin", "the update").
pub fn save_verified(
    body: impl Read,
    size: u64,
    sha256: &str,
    path: &Path,
    what: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let part = part_path(path);
    let result = (|| {
        let mut body = body.take(size.saturating_add(1));
        let mut file =
            File::create(&part).with_context(|| format!("Cannot write {}", part.display()))?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let n = body.read(&mut buffer).context("Download interrupted")?;
            if n == 0 {
                break;
            }
            received += n as u64;
            if received > size {
                bail!("The download is larger than announced");
            }
            file.write_all(&buffer[..n])
                .with_context(|| format!("Not enough space for {what}"))?;
            hash.update(&buffer[..n]);
            progress(received, size);
        }
        if received != size {
            bail!("Download incomplete ({received} of {size} bytes)");
        }
        if hex(hash.finish().as_ref()) != sha256.to_ascii_lowercase() {
            bail!("The download is damaged or was replaced (checksum mismatch)");
        }
        file.sync_all()
            .with_context(|| format!("Cannot write {}", part.display()))?;
        fs::rename(&part, path).with_context(|| format!("Cannot write {}", path.display()))
    })();
    if result.is_err() {
        // Not there when the download failed before creating it
        let _ = fs::remove_file(&part);
    }
    result
}

/// Where a download is written until it is checked: `<path>.part`
fn part_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".part");
    PathBuf::from(name)
}

/// Lowercase hex of `bytes`
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = b"OpenRig bundle";

    fn sha(bytes: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    }

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pedalboard-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saves_a_checked_download() {
        let dir = dir();
        let path = dir.join("bundle.tar.xz");
        let mut seen = Vec::new();
        let size = BODY.len() as u64;
        let hash = sha(BODY).to_ascii_uppercase();
        save_verified(BODY, size, &hash, &path, "the plugin", |r, t| {
            seen.push((r, t))
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), BODY);
        assert_eq!(seen.last(), Some(&(size, size)));
        assert!(!dir.join("bundle.tar.xz.part").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_a_wrong_download_and_leaves_nothing() {
        let dir = dir();
        let path = dir.join("update.raucb");
        let size = BODY.len() as u64;
        let cases = [
            (size - 1, sha(BODY), "larger"),
            (size + 1, sha(BODY), "incomplete"),
            (size, sha(b"other"), "checksum"),
        ];
        for (size, hash, error) in cases {
            let e = save_verified(BODY, size, &hash, &path, "the update", |_, _| {}).unwrap_err();
            assert!(e.to_string().contains(error), "{e}");
            assert!(!path.exists());
            assert!(!dir.join("update.raucb.part").exists());
        }
        let _ = fs::remove_dir_all(dir);
    }
}
