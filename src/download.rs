//! Saving a download: written next to its place as `<file>.part`, checked,
//! flushed to disk, then renamed. Nothing is left behind on failure, and a
//! power cut never leaves a truncated file under the real name.

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// What a download must be to be kept
#[derive(Debug, Clone, Copy)]
pub enum Expected<'a> {
    /// Exactly `size` bytes with SHA-256 `sha256` (hex): store archives,
    /// update bundles
    Verified { size: u64, sha256: &'a str },
    /// Not empty and at most this many bytes, when nothing more is known in
    /// advance (TONE3000 models)
    AtMost(u64),
}

impl Expected<'_> {
    /// Most bytes the download may have
    fn limit(self) -> u64 {
        match self {
            Expected::Verified { size, .. } | Expected::AtMost(size) => size,
        }
    }
}

/// Save `body` to `path`, failing unless it is what `expected` says;
/// `progress` gets the bytes received and the most expected. `what` names
/// the download in errors ("the plugin", "the update").
pub fn save(
    body: impl Read,
    expected: Expected,
    path: &Path,
    what: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let part = part_path(path);
    let limit = expected.limit();
    let result = (|| {
        let mut body = body.take(limit.saturating_add(1));
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
            if received > limit {
                bail!("The download is larger than expected");
            }
            file.write_all(&buffer[..n])
                .with_context(|| format!("Not enough space for {what}"))?;
            hash.update(&buffer[..n]);
            progress(received, limit);
        }
        match expected {
            Expected::Verified { size, sha256 } => {
                if received != size {
                    bail!("Download incomplete ({received} of {size} bytes)");
                }
                if hex(hash.finish().as_ref()) != sha256.to_ascii_lowercase() {
                    bail!("The download is damaged or was replaced (checksum mismatch)");
                }
            }
            Expected::AtMost(_) if received == 0 => bail!("The download is empty"),
            Expected::AtMost(_) => {}
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
        let expected = Expected::Verified {
            size,
            sha256: &hash,
        };
        save(BODY, expected, &path, "the plugin", |r, t| {
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
            (size - 1, sha(BODY), "larger than expected"),
            (size + 1, sha(BODY), "incomplete"),
            (size, sha(b"other"), "checksum"),
        ];
        for (size, hash, error) in cases {
            let expected = Expected::Verified {
                size,
                sha256: &hash,
            };
            let e = save(BODY, expected, &path, "the update", |_, _| {}).unwrap_err();
            assert!(e.to_string().contains(error), "{e}");
            assert!(!path.exists());
            assert!(!dir.join("update.raucb.part").exists());
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_a_bounded_download_under_its_full_name() {
        let dir = dir();
        let path = dir.join("amp.nam");
        let limit = BODY.len() as u64;
        save(BODY, Expected::AtMost(limit), &path, "the model", |_, _| {}).unwrap();
        assert_eq!(fs::read(&path).unwrap(), BODY);
        assert!(!dir.join("amp.nam.part").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_an_empty_or_too_large_bounded_download() {
        let dir = dir();
        let path = dir.join("amp.nam");
        let cases: [(&[u8], u64, &str); 2] = [
            (b"", 64, "empty"),
            (BODY, BODY.len() as u64 - 1, "larger than expected"),
        ];
        for (body, limit, error) in cases {
            let e = save(body, Expected::AtMost(limit), &path, "the model", |_, _| {}).unwrap_err();
            assert!(e.to_string().contains(error), "{e}");
            assert!(!path.exists());
            assert!(!dir.join("amp.nam.part").exists());
        }
        let _ = fs::remove_dir_all(dir);
    }
}
