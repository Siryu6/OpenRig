//! HTTPS downloads of the store: the index, its signature, pictures and
//! archives. Direct file URLs only, never the GitHub API (rate limited per
//! IP, and a pedalboard has no account).

use anyhow::{anyhow, bail, Context, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// A download slower than this on average is given up
const MIN_RATE: u64 = 16 * 1024;

fn agent(body_timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // A catalog set by the owner (OPENRIG_CATALOG_URL) may be plain HTTP
        .https_only(std::env::var_os(super::URL_ENV).is_none())
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(body_timeout))
        .http_status_as_error(false)
        .user_agent(concat!("openrig/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn open(url: &str, body_timeout: Duration) -> Result<ureq::Body> {
    let response = agent(body_timeout)
        .get(url)
        .call()
        .map_err(|e| anyhow!("Cannot reach the plugin store: {e}"))?;
    match response.status().as_u16() {
        200..=299 => Ok(response.into_body()),
        404 => bail!("Not found on the plugin store: {url}"),
        403 | 429 => bail!("The download server limits requests for now: try again later"),
        status => bail!("The download server answered HTTP {status}"),
    }
}

/// A small file (index, signature, picture), at most `max` bytes
pub fn get(url: &str, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open(url, Duration::from_secs(30))?
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut bytes)
        .context("Download interrupted")?;
    if bytes.len() as u64 > max {
        bail!("Unexpectedly large file: {url}");
    }
    Ok(bytes)
}

/// Download `url` to `path`, checking its size and SHA-256; `progress`
/// gets the bytes received and the total
pub fn download(
    url: &str,
    size: u64,
    sha256: &str,
    path: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let part = path.with_extension("part");
    let result = (|| {
        let timeout = 60 + size / MIN_RATE;
        let mut body = open(url, Duration::from_secs(timeout))?
            .into_reader()
            .take(size + 1);
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
                bail!("The download is larger than the catalog says");
            }
            file.write_all(&buffer[..n])
                .context("Not enough space for the plugin")?;
            hash.update(&buffer[..n]);
            progress(received, size);
        }
        if received != size {
            bail!("Download incomplete ({received} of {size} bytes)");
        }
        if hex(hash.finish().as_ref()) != sha256.to_ascii_lowercase() {
            bail!("The download is damaged or was replaced (checksum mismatch)");
        }
        file.sync_all()?;
        fs::rename(&part, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// SHA-256 of a file, in hex
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(hash.finish().as_ref()))
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
