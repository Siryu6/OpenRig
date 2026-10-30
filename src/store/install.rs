//! Installing and removing bundles in the LV2 folder of the data
//! directory. Every bundle the store installs gets a receipt
//! (`.openrig.json`); bundles without one were copied by hand and are
//! left alone: the store never removes or replaces them.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::index::{valid_bundle, valid_relative, Artifact, Package};
use crate::persist::{sync_dir, write_atomic};

/// Receipt file in a bundle installed by the store
pub const RECEIPT: &str = ".openrig.json";
/// Upper bound of what one bundle may unpack to
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;

/// What the store installed in a bundle
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub package: String,
    pub version: String,
    pub origin: String,
    pub license: String,
    pub source: String,
    /// SHA-256 of the archive the bundle came from
    pub sha256: String,
    pub bundle: String,
    /// Catalog entries (plugin ids) the bundle holds
    pub plugins: Vec<String>,
    pub installed: String,
}

/// A bundle of the LV2 folder
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub bundle: String,
    /// None: copied by hand
    pub receipt: Option<Receipt>,
}

/// The bundles of `lv2_dir`, with their receipt when the store put them there
pub fn installed(lv2_dir: &Path) -> Vec<Installed> {
    let Ok(entries) = fs::read_dir(lv2_dir) else {
        return Vec::new();
    };
    let mut found: Vec<Installed> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let bundle = e.file_name().into_string().ok()?;
            if !bundle.ends_with(".lv2") || bundle.starts_with('.') {
                return None;
            }
            let receipt = fs::read(e.path().join(RECEIPT))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            Some(Installed { bundle, receipt })
        })
        .collect();
    found.sort_by(|a, b| a.bundle.cmp(&b.bundle));
    found
}

/// Unpack `bundle` of `package` from `archive` (already checked against
/// the index) into `lv2_dir`. The bundle is unpacked aside and moved in
/// place at once, so a failed install leaves the previous one untouched.
/// `plugins` are the catalog entries the bundle holds.
pub fn install(
    lv2_dir: &Path,
    package: &Package,
    artifact: &Artifact,
    bundle: &str,
    plugins: Vec<String>,
    archive: &Path,
) -> Result<()> {
    if !valid_bundle(bundle) {
        bail!("Invalid bundle name: {bundle}");
    }
    let target = lv2_dir.join(bundle);
    if target.exists() && read_receipt(&target).is_none() {
        bail!("{bundle} was copied into the LV2 folder by hand: remove it first");
    }
    fs::create_dir_all(lv2_dir).with_context(|| format!("Cannot create {}", lv2_dir.display()))?;
    let staging = lv2_dir.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let unpacked = staging.join(bundle);
        fs::create_dir_all(&unpacked)?;
        let prefix = match artifact.root.as_str() {
            "" => bundle.to_string(),
            root => format!("{root}/{bundle}"),
        };
        let files = unpack(archive, &artifact.format, &prefix, &unpacked)?;
        if files == 0 || !unpacked.join("manifest.ttl").is_file() {
            bail!("{bundle} is not in the downloaded archive");
        }
        let receipt = Receipt {
            package: package.id.clone(),
            version: package.version.clone(),
            origin: package.origin.clone(),
            license: package.license.clone(),
            source: package.source.url.clone(),
            sha256: artifact.sha256.clone(),
            bundle: bundle.to_string(),
            plugins,
            installed: chrono::Utc::now().to_rfc3339(),
        };
        write_atomic(
            &unpacked.join(RECEIPT),
            &serde_json::to_vec_pretty(&receipt)?,
        )?;
        // Replace: the old bundle steps aside, the new one moves in
        let old = lv2_dir.join(format!(".old-{}", uuid::Uuid::new_v4()));
        if target.exists() {
            fs::rename(&target, &old)?;
        }
        if let Err(e) = fs::rename(&unpacked, &target) {
            let _ = fs::rename(&old, &target);
            return Err(e.into());
        }
        sync_dir(lv2_dir);
        let _ = fs::remove_dir_all(&old);
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Remove a bundle the store installed
pub fn remove(lv2_dir: &Path, bundle: &str) -> Result<()> {
    if !valid_bundle(bundle) {
        bail!("Invalid bundle name: {bundle}");
    }
    let target = lv2_dir.join(bundle);
    if read_receipt(&target).is_none() {
        bail!("{bundle} was not installed by the store: remove it by hand");
    }
    // Out of the LV2 path at once, then deleted
    let doomed = lv2_dir.join(format!(".removed-{}", uuid::Uuid::new_v4()));
    fs::rename(&target, &doomed).with_context(|| format!("Cannot remove {bundle}"))?;
    sync_dir(lv2_dir);
    fs::remove_dir_all(&doomed).with_context(|| format!("Cannot remove {bundle}"))?;
    Ok(())
}

/// Leftovers of an interrupted install or removal
pub fn clean(lv2_dir: &Path) {
    let Ok(entries) = fs::read_dir(lv2_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if [".staging-", ".old-", ".removed-"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn read_receipt(bundle: &Path) -> Option<Receipt> {
    serde_json::from_slice(&fs::read(bundle.join(RECEIPT)).ok()?).ok()
}

/// Unpack the files under `prefix/` of an archive into `dest`; returns how
/// many. Only plain files and directories are taken, and only with clean
/// relative paths: no links, devices, absolute paths or `..`.
fn unpack(archive: &Path, format: &str, prefix: &str, dest: &Path) -> Result<usize> {
    let file = BufReader::new(File::open(archive)?);
    match format {
        "tar.gz" => unpack_tar(flate2::read::GzDecoder::new(file), prefix, dest),
        "tar.xz" => unpack_tar_xz(file, prefix, dest),
        // Some projects only publish Debian packages: their files are in
        // the data.tar.* member (`root` is then usr/lib/lv2)
        "deb" => {
            let (member, data) = deb_data(file)?;
            match member.as_str() {
                "data.tar.gz" => unpack_tar(flate2::read::GzDecoder::new(data), prefix, dest),
                "data.tar.xz" => unpack_tar_xz(BufReader::new(data), prefix, dest),
                "data.tar" => unpack_tar(data, prefix, dest),
                other => bail!("Unsupported compression in the Debian package: {other}"),
            }
        }
        other => bail!("Unsupported archive format: {other}"),
    }
}

fn unpack_tar_xz(xz: impl std::io::BufRead, prefix: &str, dest: &Path) -> Result<usize> {
    let mut xz = xz;
    // lzma-rs decodes into a writer: through a temporary file
    let tar_path = dest.with_extension("tar");
    let result = (|| {
        let mut tar = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tar_path)?;
        lzma_rs::xz_decompress(&mut xz, &mut tar)
            .map_err(|e| anyhow::anyhow!("The downloaded archive is damaged: {e}"))?;
        tar.seek(SeekFrom::Start(0))?;
        unpack_tar(BufReader::new(tar), prefix, dest)
    })();
    let _ = fs::remove_file(&tar_path);
    result
}

/// The data.tar.* member of a Debian package (an `ar` archive): its name
/// and its bytes
fn deb_data<R: Read>(mut deb: R) -> Result<(String, std::io::Take<R>)> {
    let damaged = || anyhow::anyhow!("The downloaded Debian package is damaged");
    let mut magic = [0u8; 8];
    deb.read_exact(&mut magic).map_err(|_| damaged())?;
    if &magic != b"!<arch>\n" {
        return Err(damaged());
    }
    loop {
        // name[16] mtime[12] uid[6] gid[6] mode[8] size[10] magic[2]
        let mut header = [0u8; 60];
        match deb.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                bail!("No files in the Debian package")
            }
            Err(e) => return Err(e.into()),
        }
        if &header[58..60] != b"`\n" {
            return Err(damaged());
        }
        let name = String::from_utf8_lossy(&header[..16])
            .trim_end()
            .trim_end_matches('/')
            .to_string();
        let size: u64 = std::str::from_utf8(&header[48..58])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(damaged)?;
        if name.starts_with("data.tar") {
            return Ok((name, deb.take(size)));
        }
        // Members are padded to an even size
        let skip = size + size % 2;
        if std::io::copy(&mut (&mut deb).take(skip), &mut std::io::sink())? != skip {
            return Err(damaged());
        }
    }
}

fn unpack_tar(reader: impl Read, prefix: &str, dest: &Path) -> Result<usize> {
    let mut archive = tar::Archive::new(reader);
    let mut files = 0;
    let mut total = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let path = path.trim_start_matches("./").trim_end_matches('/');
        let Some(rest) = path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        if !valid_relative(rest) {
            bail!("Unsafe path in the archive: {path}");
        }
        let out = dest.join(rest);
        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&out)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                total += entry.header().size()?;
                if total > MAX_BUNDLE_BYTES {
                    bail!("The bundle is too large");
                }
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = File::create(&out)?;
                std::io::copy(&mut entry, &mut file)?;
                file.sync_all()?;
                set_mode(&out, rest.ends_with(".so"));
                files += 1;
            }
            // Links and special files are not taken
            kind => tracing::warn!("Skipping {path} in the archive ({kind:?})"),
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) {}

/// Where a bundle of the LV2 folder is
pub fn bundle_path(lv2_dir: &Path, bundle: &str) -> PathBuf {
    lv2_dir.join(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::index::Source;
    use std::collections::HashMap;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openrig-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn package() -> Package {
        Package {
            id: "demo".into(),
            name: "Demo".into(),
            version: "1.0".into(),
            license: "MIT".into(),
            homepage: "https://example.org".into(),
            origin: "upstream".into(),
            source: Source {
                url: "https://example.org/src.tar.gz".into(),
                sha256: None,
            },
            artifacts: HashMap::new(),
            plugins: Vec::new(),
        }
    }

    fn artifact() -> Artifact {
        Artifact {
            url: "https://example.org/demo.tar.gz".into(),
            sha256: "00".into(),
            size: 0,
            format: "tar.gz".into(),
            root: "demo-1.0".into(),
        }
    }

    /// A tar.gz with `files` (path, content) and an optional symlink
    fn archive(dir: &Path, files: &[(&str, &str)], link: Option<&str>) -> PathBuf {
        let path = dir.join("demo.tar.gz");
        let gz = flate2::write::GzEncoder::new(File::create(&path).unwrap(), Default::default());
        let mut builder = tar::Builder::new(gz);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            // Raw name: the tar crate would refuse to write `..`
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, content.as_bytes()).unwrap();
        }
        if let Some(name) = link {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder
                .append_link(&mut header, name, "/etc/passwd")
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn installs_only_the_bundle_and_removes_it() {
        let dir = temp();
        let lv2 = dir.join("lv2");
        let archive = archive(
            &dir,
            &[
                ("demo-1.0/Demo.lv2/manifest.ttl", "@prefix lv2: <x> ."),
                ("demo-1.0/Demo.lv2/demo.so", "elf"),
                ("demo-1.0/Other.lv2/manifest.ttl", "other"),
                ("demo-1.0/README", "readme"),
            ],
            Some("demo-1.0/Demo.lv2/link"),
        );
        install(
            &lv2,
            &package(),
            &artifact(),
            "Demo.lv2",
            vec!["demo".into()],
            &archive,
        )
        .unwrap();
        assert!(lv2.join("Demo.lv2/manifest.ttl").is_file());
        assert!(lv2.join("Demo.lv2/demo.so").is_file());
        assert!(!lv2.join("Demo.lv2/link").exists());
        assert!(!lv2.join("Other.lv2").exists());
        let found = installed(&lv2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].receipt.as_ref().unwrap().package, "demo");

        // Installing again replaces it
        install(
            &lv2,
            &package(),
            &artifact(),
            "Demo.lv2",
            vec!["demo".into()],
            &archive,
        )
        .unwrap();
        assert_eq!(installed(&lv2).len(), 1);

        remove(&lv2, "Demo.lv2").unwrap();
        assert!(installed(&lv2).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn leaves_hand_copied_bundles_alone() {
        let dir = temp();
        let lv2 = dir.join("lv2");
        fs::create_dir_all(lv2.join("Demo.lv2")).unwrap();
        fs::write(lv2.join("Demo.lv2/manifest.ttl"), "mine").unwrap();
        let archive = archive(&dir, &[("demo-1.0/Demo.lv2/manifest.ttl", "x")], None);
        assert!(install(&lv2, &package(), &artifact(), "Demo.lv2", vec![], &archive).is_err());
        assert!(remove(&lv2, "Demo.lv2").is_err());
        assert_eq!(
            fs::read_to_string(lv2.join("Demo.lv2/manifest.ttl")).unwrap(),
            "mine"
        );
        assert_eq!(installed(&lv2)[0].receipt, None);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A Debian package: an odd-sized control member first, then
    /// data.tar.gz with `files` under ./
    fn deb(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let mut data = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut data, Default::default());
            let mut builder = tar::Builder::new(gz);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("./{name}"), content.as_bytes())
                    .unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        let mut deb = b"!<arch>\n".to_vec();
        for (name, body) in [
            ("debian-binary", &b"2.0\n"[..]),
            ("control.tar.gz", &b"odd"[..]),
            ("data.tar.gz", &data[..]),
        ] {
            deb.extend(
                format!(
                    "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                    name,
                    0,
                    0,
                    0,
                    100644,
                    body.len()
                )
                .bytes(),
            );
            deb.extend(body);
            if body.len() % 2 == 1 {
                deb.push(b'\n');
            }
        }
        let path = dir.join("demo.deb");
        fs::write(&path, deb).unwrap();
        path
    }

    #[test]
    fn installs_a_bundle_from_a_debian_package() {
        let dir = temp();
        let lv2 = dir.join("lv2");
        let archive = deb(
            &dir,
            &[
                ("usr/lib/lv2/Demo.lv2/manifest.ttl", "@prefix lv2: <x> ."),
                ("usr/lib/lv2/Demo.lv2/bin/demo-a76.so", "elf"),
                ("usr/share/doc/demo/copyright", "MIT"),
            ],
        );
        let artifact = Artifact {
            format: "deb".into(),
            root: "usr/lib/lv2".into(),
            ..artifact()
        };
        install(&lv2, &package(), &artifact, "Demo.lv2", vec![], &archive).unwrap();
        assert!(lv2.join("Demo.lv2/manifest.ttl").is_file());
        assert!(lv2.join("Demo.lv2/bin/demo-a76.so").is_file());
        assert!(!dir.join("lv2/usr").exists());

        // Not a Debian package
        fs::write(&archive, b"<html>").unwrap();
        assert!(install(&lv2, &package(), &artifact, "Demo.lv2", vec![], &archive).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_escaping_paths() {
        let dir = temp();
        let lv2 = dir.join("lv2");
        let archive = archive(
            &dir,
            &[
                ("demo-1.0/Demo.lv2/manifest.ttl", "x"),
                ("demo-1.0/Demo.lv2/../../../escape", "x"),
            ],
            None,
        );
        assert!(install(&lv2, &package(), &artifact(), "Demo.lv2", vec![], &archive).is_err());
        assert!(!lv2.join("Demo.lv2").exists());
        assert!(!dir.join("escape").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_bundle_fails_cleanly() {
        let dir = temp();
        let lv2 = dir.join("lv2");
        let archive = archive(&dir, &[("demo-1.0/Other.lv2/manifest.ttl", "x")], None);
        assert!(install(&lv2, &package(), &artifact(), "Demo.lv2", vec![], &archive).is_err());
        assert!(fs::read_dir(&lv2).unwrap().next().is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
