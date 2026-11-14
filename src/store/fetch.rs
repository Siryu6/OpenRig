//! HTTPS downloads of the store: the index, its signature, pictures and
//! archives. Direct file URLs only, never the GitHub API (rate limited per
//! IP, and a pedalboard has no account).

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::download::{hex, save_verified};
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
        .take(max.saturating_add(1))
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
    progress: impl FnMut(u64, u64),
) -> Result<()> {
    let timeout = 60 + size / MIN_RATE;
    let body = open(url, Duration::from_secs(timeout))?.into_reader();
    save_verified(body, size, sha256, path, "the plugin", progress)
}

/// SHA-256 of a file, in hex
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .with_context(|| format!("Cannot read {}", path.display()))?;
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
