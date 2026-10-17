//! "Check for updates" in Settings: compares the app with the latest
//! release on GitHub and, on the pedalboard OS, downloads and installs it.
//!
//! Each release publishes, per board, a manifest
//! (`openrig-os-<version>-<board>.json`) naming two signed RAUC bundles: the
//! whole system and the app alone. The app alone is enough when the release
//! was built on the same system as the running one (same `OS_ID` in
//! `/etc/pedalboard-release`); otherwise the whole system is installed, on
//! the other A/B slot. The pedalboard runs the app as an unprivileged user:
//! a root service installs what it downloaded (`/usr/libexec/pedalboard/updater`,
//! see docs/OS.md, "Updates"). Elsewhere (desktop), a newer release is only
//! reported.
//!
//! Every call here blocks on the network or on the installer: call them off
//! the UI thread.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

/// Latest release of the official repository
pub const RELEASES_URL: &str = "https://api.github.com/repos/Siryu6/OpenRig/releases/latest";
/// Another "latest release" address (a test server, which may be plain HTTP)
pub const URL_ENV: &str = "PEDALBOARD_UPDATE_URL";
/// Directory of the root installer (FIFO `request`, file `status`), set on
/// the pedalboard OS
pub const DIR_ENV: &str = "PEDALBOARD_UPDATE_DIR";
/// The running system, on the pedalboard OS
pub const RELEASE_FILE: &str = "/etc/pedalboard-release";

/// Upper bound of a download (a whole x86_64 system is ~130 MB)
const MAX_DOWNLOAD_BYTES: u64 = 1 << 30;
/// A download slower than this on average (bytes/s) is given up
const MIN_DOWNLOAD_RATE: u64 = 50_000;

/// The pedalboard OS running this app
#[derive(Debug, Clone, PartialEq)]
pub struct System {
    /// Version of the system image
    pub version: String,
    pub board: String,
    pub os_id: String,
}

impl System {
    /// From `/etc/pedalboard-release`; `None` off the pedalboard OS, or on
    /// an image from before app updates (no `OS_ID`)
    pub fn detect() -> Option<Self> {
        Self::parse(&fs::read_to_string(RELEASE_FILE).ok()?)
    }

    fn parse(text: &str) -> Option<Self> {
        let field = |name: &str| {
            text.lines().find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix('=')?;
                let value = value.trim().trim_matches('"');
                (!value.is_empty()).then(|| value.to_string())
            })
        };
        Some(Self {
            version: field("VERSION")?,
            board: field("BOARD")?,
            os_id: field("OS_ID")?,
        })
    }
}

/// A release on GitHub
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    /// Release notes (Markdown)
    #[serde(default, rename = "body")]
    pub notes: Option<String>,
    /// Release page
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

impl Release {
    /// Version without the "v" of the tag
    pub fn version(&self) -> &str {
        self.tag.strip_prefix('v').unwrap_or(&self.tag)
    }

    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Name of the manifest for a board
    pub fn manifest_name(&self, board: &str) -> String {
        format!("openrig-os-{}-{board}.json", self.version())
    }
}

/// Per-board manifest of a release (written by os/release-assets.sh)
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub board: String,
    pub os_id: String,
    pub system: Bundle,
    pub app: Bundle,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Bundle {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// What to install
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The app alone, on the data partition; restarts the app
    App,
    /// The whole system, on the other slot; restarts the pedalboard
    System,
}

impl Kind {
    fn request(self) -> &'static str {
        match self {
            Kind::App => "app",
            Kind::System => "system",
        }
    }
}

/// A bundle to download and install
#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub kind: Kind,
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// Outcome of a check
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    UpToDate,
    /// Newer, nothing to install here: not the pedalboard OS
    Available,
    /// Newer, but this board's files are not published yet (still building)
    Preparing,
    Install(Download),
}

/// The latest release and what to do with it
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub release: Release,
    pub plan: Plan,
}

/// True if `candidate` is a newer version than `current` (x.y.z, a
/// pre-release such as 1.0.0-rc.1 comes before 1.0.0)
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// A version, in the order of semantic versioning: a pre-release comes
/// before its release, pre-releases by their identifiers (numbers before
/// words, numbers by value)
type Version = (u64, u64, u64, bool, Vec<(u64, String)>);

fn parse_version(v: &str) -> Option<Version> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    let identifiers = pre
        .unwrap_or_default()
        .split('.')
        .filter(|i| !i.is_empty())
        .map(|i| match i.parse::<u64>() {
            Ok(n) => (n, String::new()),
            Err(_) => (u64::MAX, i.to_string()),
        })
        .collect();
    Some((major, minor, patch, pre.is_none(), identifiers))
}

/// Compare with the latest release and pick what to install. `system` is
/// the running pedalboard OS (`None` elsewhere); `manifest` reads a
/// release's manifest for it.
pub fn plan(
    release: &Release,
    app_version: &str,
    system: Option<&System>,
    manifest: impl FnOnce(&Asset) -> Result<Manifest>,
) -> Result<Plan> {
    if !is_newer(release.version(), app_version) {
        return Ok(Plan::UpToDate);
    }
    let Some(system) = system else {
        return Ok(Plan::Available);
    };
    let Some(asset) = release.asset(&release.manifest_name(&system.board)) else {
        return Ok(Plan::Preparing);
    };
    let manifest = manifest(asset)?;
    if manifest.board != system.board || manifest.version != release.version() {
        bail!("The update description of {} does not match", release.tag);
    }
    let (kind, bundle) = if manifest.os_id == system.os_id {
        (Kind::App, &manifest.app)
    } else {
        (Kind::System, &manifest.system)
    };
    if !valid_file_name(&bundle.file) || !bundle.file.ends_with(".raucb") {
        bail!("Invalid update file name in {}", release.tag);
    }
    if bundle.size == 0 || bundle.size > MAX_DOWNLOAD_BYTES {
        bail!("The update of {} is too large", release.tag);
    }
    let Some(asset) = release.asset(&bundle.file) else {
        return Ok(Plan::Preparing);
    };
    Ok(Plan::Install(Download {
        kind,
        file: bundle.file.clone(),
        url: asset.browser_download_url.clone(),
        size: bundle.size,
        sha256: bundle.sha256.to_ascii_lowercase(),
    }))
}

/// A plain file name: letters, digits, `.`, `_`, `-`, not starting with `.`
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Ask GitHub for the latest release and what to install from it
pub fn check(app_version: &str, system: Option<&System>) -> Result<Check> {
    let url = std::env::var(URL_ENV).unwrap_or_else(|_| RELEASES_URL.to_string());
    let release: Release = get(&url)?
        .into_body()
        .read_json()
        .context("Unexpected answer from GitHub")?;
    let plan = plan(&release, app_version, system, |asset| {
        get(&asset.browser_download_url)?
            .into_body()
            .read_json()
            .context("Unreadable update description")
    })?;
    Ok(Check { release, plan })
}

fn agent(body_timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // A test server set by the system may be plain HTTP
        .https_only(std::env::var_os(URL_ENV).is_none())
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(body_timeout))
        .http_status_as_error(false)
        .user_agent(concat!("openrig/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    fetch(url, Duration::from_secs(20))
}

fn fetch(url: &str, body_timeout: Duration) -> Result<ureq::http::Response<ureq::Body>> {
    let response = agent(body_timeout)
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("Cannot reach GitHub: {e}"))?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        404 => bail!("No release published yet"),
        403 | 429 => bail!("GitHub limits the checks for now: try again in an hour"),
        status => bail!("GitHub answered HTTP {status}"),
    }
}

/// Download a bundle into `dir` and check its SHA-256; `progress` gets the
/// bytes received and the total. Returns the file.
pub fn download(
    download: &Download,
    dir: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    if !valid_file_name(&download.file) {
        bail!("Invalid file name: {}", download.file);
    }
    fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    // Leftovers of an earlier attempt
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }
    let path = dir.join(&download.file);
    let part = dir.join(format!("{}.part", download.file));
    let result = (|| {
        // ureq has no idle timeout: a stalled download fails once it is
        // below MIN_DOWNLOAD_RATE on average, rather than after an hour
        let timeout = 60 + download.size / MIN_DOWNLOAD_RATE;
        let response = fetch(&download.url, Duration::from_secs(timeout))?;
        let mut body = response
            .into_body()
            .into_reader()
            .take(MAX_DOWNLOAD_BYTES + 1);
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
            file.write_all(&buffer[..n])
                .context("Not enough space for the update")?;
            hash.update(&buffer[..n]);
            received += n as u64;
            if received > MAX_DOWNLOAD_BYTES {
                bail!("The update is too large");
            }
            progress(received, download.size.max(received));
        }
        if received != download.size {
            bail!(
                "Download incomplete ({received} of {} bytes)",
                download.size
            );
        }
        let digest = hex(hash.finish().as_ref());
        if digest != download.sha256 {
            bail!("The download is damaged (checksum mismatch)");
        }
        file.sync_all()?;
        fs::rename(&part, &path)?;
        Ok(path.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// State of an installation, from the root installer's status file
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Installing,
    Done,
    Failed(String),
}

/// The root installer of the pedalboard OS (`/usr/libexec/pedalboard/updater`)
pub struct Installer {
    dir: PathBuf,
}

impl Installer {
    /// Only on the pedalboard OS, where the launcher sets `PEDALBOARD_UPDATE_DIR`
    pub fn from_env() -> Option<Self> {
        std::env::var_os(DIR_ENV).map(|dir| Self { dir: dir.into() })
    }

    /// Ask for `file` (in the download directory) to be installed; returns
    /// the request id the status refers to
    pub fn request(&self, kind: Kind, file: &str) -> Result<String> {
        let id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        );
        // Non-blocking: without the installer reading, opening fails
        // instead of waiting forever
        #[cfg(unix)]
        let mut fifo = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(self.dir.join("request"))
        }
        .map_err(|_| anyhow!("The system installer is not running"))?;
        #[cfg(not(unix))]
        let mut fifo = OpenOptions::new()
            .write(true)
            .open(self.dir.join("request"))
            .map_err(|_| anyhow!("The system installer is not running"))?;
        writeln!(fifo, "{id} {} {file}", kind.request())?;
        Ok(id)
    }

    /// Status of request `id`; `None` until the installer took it
    pub fn status(&self, id: &str) -> Option<Status> {
        let text = fs::read_to_string(self.dir.join("status")).ok()?;
        parse_status(&text, id)
    }
}

fn parse_status(text: &str, id: &str) -> Option<Status> {
    let line = text.lines().next()?;
    let rest = line.strip_prefix(id)?.strip_prefix(' ')?;
    let (state, detail) = rest.split_once(' ').unwrap_or((rest, ""));
    match state {
        "installing" => Some(Status::Installing),
        "done" => Some(Status::Done),
        "error" => Some(Status::Failed(if detail.is_empty() {
            "The installation failed".into()
        } else {
            detail.to_string()
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, assets: &[&str]) -> Release {
        Release {
            tag: format!("v{version}"),
            notes: None,
            html_url: "https://github.com/Siryu6/OpenRig/releases/tag/v".into(),
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: name.to_string(),
                    size: 10,
                    browser_download_url: format!("https://example.invalid/{name}"),
                })
                .collect(),
        }
    }

    fn system() -> System {
        System {
            version: "0.1.0".into(),
            board: "rpi5".into(),
            os_id: "aaaa".into(),
        }
    }

    fn manifest(os_id: &str) -> Manifest {
        let bundle = |file: &str| Bundle {
            file: file.into(),
            size: 10,
            sha256: "AB".into(),
        };
        Manifest {
            version: "0.2.0".into(),
            board: "rpi5".into(),
            os_id: os_id.into(),
            system: bundle("openrig-os-0.2.0-rpi5.raucb"),
            app: bundle("openrig-os-0.2.0-rpi5-app.raucb"),
        }
    }

    const ALL: &[&str] = &[
        "openrig-os-0.2.0-rpi5.json",
        "openrig-os-0.2.0-rpi5.raucb",
        "openrig-os-0.2.0-rpi5-app.raucb",
    ];

    #[test]
    fn versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.0"));
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(is_newer("1.0.0-rc.10", "1.0.0-rc.2"));
        assert!(is_newer("1.0.0-rc.1", "1.0.0-alpha"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        // A development version is never compared
        assert!(!is_newer("0.2.0", "0dc5263-dirty"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn system_file() {
        let text = "NAME=\"Pedalboard OS\"\nVERSION=\"0.2.0\"\nAPP_VERSION=\"0.2.0\"\n\
                    BOARD=\"rpi5\"\nOS_ID=\"0123abcd\"\n";
        let system = System::parse(text).unwrap();
        assert_eq!(system.version, "0.2.0");
        assert_eq!(system.board, "rpi5");
        assert_eq!(system.os_id, "0123abcd");
        // An image from before app updates
        assert_eq!(System::parse("VERSION=\"0.1.0\"\n"), None);
    }

    #[test]
    fn up_to_date() {
        let plan = plan(&release("0.1.0", ALL), "0.1.0", Some(&system()), |_| {
            panic!("no manifest needed")
        });
        assert_eq!(plan.unwrap(), Plan::UpToDate);
    }

    #[test]
    fn desktop_only_reports() {
        let plan = plan(&release("0.2.0", ALL), "0.1.0", None, |_| panic!());
        assert_eq!(plan.unwrap(), Plan::Available);
    }

    #[test]
    fn same_system_installs_the_app() {
        let plan = plan(&release("0.2.0", ALL), "0.1.0", Some(&system()), |asset| {
            assert_eq!(asset.name, "openrig-os-0.2.0-rpi5.json");
            Ok(manifest("aaaa"))
        })
        .unwrap();
        let Plan::Install(download) = plan else {
            panic!("{plan:?}")
        };
        assert_eq!(download.kind, Kind::App);
        assert_eq!(download.file, "openrig-os-0.2.0-rpi5-app.raucb");
        assert_eq!(
            download.url,
            "https://example.invalid/openrig-os-0.2.0-rpi5-app.raucb"
        );
        assert_eq!(download.sha256, "ab");
    }

    #[test]
    fn another_system_installs_everything() {
        let plan = plan(&release("0.2.0", ALL), "0.1.0", Some(&system()), |_| {
            Ok(manifest("bbbb"))
        })
        .unwrap();
        let Plan::Install(download) = plan else {
            panic!("{plan:?}")
        };
        assert_eq!(download.kind, Kind::System);
        assert_eq!(download.file, "openrig-os-0.2.0-rpi5.raucb");
    }

    #[test]
    fn files_still_building() {
        let building = plan(
            &release("0.2.0", &["openrig-0.2.0-linux-x86_64.tar.gz"]),
            "0.1.0",
            Some(&system()),
            |_| panic!(),
        );
        assert_eq!(building.unwrap(), Plan::Preparing);
        // Manifest there, bundle not yet
        let plan = plan(
            &release("0.2.0", &["openrig-os-0.2.0-rpi5.json"]),
            "0.1.0",
            Some(&system()),
            |_| Ok(manifest("aaaa")),
        );
        assert_eq!(plan.unwrap(), Plan::Preparing);
    }

    #[test]
    fn manifest_of_another_board_is_refused() {
        let mut other = manifest("aaaa");
        other.board = "x86_64".into();
        let plan = plan(&release("0.2.0", ALL), "0.1.0", Some(&system()), |_| {
            Ok(other)
        });
        assert!(plan.is_err());
    }

    #[test]
    fn github_release_json() {
        let json = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/x","body":"notes",
            "assets":[{"name":"a.json","size":12,"browser_download_url":"https://h/a.json","id":1}],
            "draft":false}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.version(), "0.2.0");
        assert_eq!(release.notes.as_deref(), Some("notes"));
        assert_eq!(release.assets[0].size, 12);
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.2.0","board":"rpi5","os_id":"x",
                "system":{"file":"s","size":1,"sha256":"00"},
                "app":{"file":"a","size":2,"sha256":"11"}}"#,
        )
        .unwrap();
        assert_eq!(manifest.app.size, 2);
    }

    #[test]
    fn installer_status() {
        assert_eq!(parse_status("- idle\n", "1-2"), None);
        assert_eq!(
            parse_status("1-2 installing\n", "1-2"),
            Some(Status::Installing)
        );
        assert_eq!(parse_status("1-2 done app\n", "1-2"), Some(Status::Done));
        assert_eq!(
            parse_status("1-2 error No space left\n", "1-2"),
            Some(Status::Failed("No space left".into()))
        );
        // An earlier request
        assert_eq!(parse_status("1-1 done app\n", "1-2"), None);
        assert_eq!(parse_status("1-22 done app\n", "1-2"), None);
    }

    #[test]
    fn download_checks_the_name() {
        let d = Download {
            kind: Kind::App,
            file: "../x".into(),
            url: String::new(),
            size: 0,
            sha256: String::new(),
        };
        assert!(download(&d, Path::new("/nonexistent"), |_, _| {}).is_err());
    }
}
