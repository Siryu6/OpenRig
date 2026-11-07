//! openrig.eu, the community rig site: browse, download, favourite and
//! publish rigs from the pedalboard (API v1, `docs/API.md` of the site).
//!
//! - [`Client`]: the HTTP calls, anonymous or with the device token.
//! - [`Account`]: the link of this pedalboard to a site account, a device
//!   token kept in `<data>/openrig.json`, readable by its owner only.
//! - [`Linking`]: the device linking flow (a short code the user enters on
//!   the site), polled on its own thread.
//!
//! A downloaded rig goes through `rigs::import` like a file from a USB
//! drive: this module only moves bytes. The site is `https://openrig.eu`,
//! or `OPENRIG_RIGS_URL` (tests, self-hosting; plain HTTP is accepted only
//! there).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::export::Exported;
use super::format::{Level, Rig, MEDIA_TYPE};

/// The site, unless `URL_ENV` says otherwise
pub const DEFAULT_URL: &str = "https://openrig.eu";
/// Another site (a test server, a self-hosted one)
pub const URL_ENV: &str = "OPENRIG_RIGS_URL";
/// Largest rig file the site takes for a publish or a new version (its
/// default `MAX_RIG_BYTES`, 512 KiB). The app itself reads rig files up to
/// the format's [`MAX_RIG_BYTES`](super::format::MAX_RIG_BYTES) (1 MiB),
/// downloads included: the stricter limit only applies on the way up.
pub const MAX_PUBLISH_BYTES: usize = 512 * 1024;
const _: () = assert!(MAX_PUBLISH_BYTES <= super::format::MAX_RIG_BYTES);

/// What the app says of a rig over [`MAX_PUBLISH_BYTES`]
pub const TOO_LARGE: &str = "This rig is too large for openrig.eu (512 KiB at most)";

/// Styles of the site, in its order
pub const STYLES: &[&str] = &[
    "clean", "crunch", "rock", "metal", "blues", "jazz", "funk", "country", "ambient", "acoustic",
    "bass", "other",
];

/// Sorts of the rig list, as the API names them
pub const SORTS: &[&str] = &["trending", "new", "top-week", "downloads"];

/// Bounds of what the site says of the device flow: a code is shown at
/// most this long, polled at most this far apart
const MAX_CODE_SECS: u64 = 30 * 60;
const MAX_POLL_SECS: u64 = 60;

/// The site's base URL: `URL_ENV`, else openrig.eu; no trailing slash
pub fn base_url() -> String {
    base_url_from(std::env::var(URL_ENV).ok())
}

fn base_url_from(value: Option<String>) -> String {
    value
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Why a call failed
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// No network, DNS, timeout, TLS: try later
    #[error("openrig.eu is not reachable: {0}")]
    Offline(String),
    /// The token is unknown or revoked, or the account is banned
    #[error("This pedalboard is no longer linked to openrig.eu: link it again")]
    Unauthorized,
    /// Not the caller's rig
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    /// Too many calls, or the daily publishing cap
    #[error("{0}")]
    RateLimited(String),
    /// The request or the rig file was refused (400, 413, 422): `code` is
    /// the API's (`local_dependencies`, `invalid`, `too_large`…)
    #[error("{message}")]
    Refused { code: String, message: String },
    /// Server trouble, or an answer this client does not understand
    #[error("openrig.eu answered {0}")]
    Server(String),
}

impl ApiError {
    /// The API code of a refusal ("" for other errors)
    pub fn code(&self) -> &str {
        match self {
            Self::Refused { code, .. } => code,
            _ => "",
        }
    }
}

/// The error shape of every API answer
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

fn api_error(status: u16, body: &str) -> ApiError {
    let parsed: Option<ErrorBody> = serde_json::from_str(body).ok();
    let (code, message) = parsed.map(|e| (e.error, e.message)).unwrap_or_default();
    let or = |default: &str| {
        if message.trim().is_empty() {
            default.to_string()
        } else {
            message.trim().to_string()
        }
    };
    match status {
        401 => ApiError::Unauthorized,
        403 => ApiError::Forbidden(or("openrig.eu refused: not your rig")),
        404 => ApiError::NotFound(or("Not found on openrig.eu")),
        429 => ApiError::RateLimited(or("openrig.eu limits requests for now: try again later")),
        413 => ApiError::Refused {
            code: "too_large".into(),
            message: TOO_LARGE.into(),
        },
        400 | 422 => ApiError::Refused {
            message: or(&format!("openrig.eu refused the request ({code})")),
            code,
        },
        _ => ApiError::Server(format!("HTTP {status}")),
    }
}

/// Author of a rig
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Author {
    pub login: String,
    pub url: String,
}

/// What a rig needs, counted by source
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DepCounts {
    pub store: u32,
    pub tone3000: u32,
    pub builtin: u32,
}

/// A rig of a list
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub style: String,
    pub tags: Vec<String>,
    pub author: Author,
    pub version: u32,
    pub upvotes: i64,
    pub favourites: i64,
    pub downloads: i64,
    pub comments: i64,
    pub blocks: u32,
    pub scenes: u32,
    pub dependencies: DepCounts,
    pub created_at: i64,
    pub updated_at: i64,
    pub url: String,
    pub download_url: String,
    /// Hidden by a moderator: only `GET /me/rigs` says so (its owner still
    /// sees it and can publish new versions)
    pub hidden: bool,
}

/// A page of rigs
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Page {
    pub items: Vec<Summary>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
}

impl Page {
    /// More rigs after this page
    pub fn has_more(&self) -> bool {
        u64::from(self.page) * u64::from(self.per_page) < self.total
    }
}

/// A scene of a rig: its name and the blocks it turns off
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SceneView {
    pub name: String,
}

/// One dependency, as the site reads it from the current version
/// (informative: installing reads the file)
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DependencyView {
    /// "builtin", "store", "tone3000" (or newer)
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// Built-in blocks: the end of their URI ("nam")
    #[serde(rename = "kind")]
    pub builtin_kind: String,
    pub uri: String,
    pub tone_title: String,
    pub model_name: String,
    pub creator: String,
    pub license: Option<String>,
}

impl DependencyView {
    /// The plugin URI, for plugins (built-in blocks have none in the view)
    pub fn plugin_uri(&self) -> Option<String> {
        match self.kind.as_str() {
            "store" if !self.uri.is_empty() => Some(self.uri.clone()),
            "builtin" if !self.builtin_kind.is_empty() => {
                Some(format!("urn:pedalboard:{}", self.builtin_kind))
            }
            _ => None,
        }
    }

    /// Name to show
    pub fn label(&self) -> String {
        match self.kind.as_str() {
            "tone3000" if !self.tone_title.is_empty() => {
                format!("{} · {}", self.tone_title, self.model_name)
            }
            _ if !self.name.is_empty() => self.name.clone(),
            _ => self.uri.clone(),
        }
    }
}

/// What the site reads from the current version of a rig
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RigView {
    pub scenes: Vec<SceneView>,
    pub dependencies: Vec<DependencyView>,
}

/// A published version
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Version {
    pub version: u32,
    pub changelog: String,
    pub created_at: i64,
}

/// A rig with what the site knows of it
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    #[serde(default)]
    pub view: RigView,
    #[serde(default)]
    pub versions: Vec<Version>,
    /// Whether the account favourited it: only when asked with a token, and
    /// from a site that says so (else `None`: see `/me/favourites`)
    #[serde(default)]
    pub favourited: Option<bool>,
}

/// The signed-in account
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Me {
    pub login: String,
    pub name: String,
    pub url: String,
}

/// A favourite, after a toggle
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct Favourite {
    pub on: bool,
    pub count: i64,
}

/// What to list
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Words to search, "" for all
    pub q: String,
    /// One of `SORTS`, "" for the site's default
    pub sort: String,
    /// One of `STYLES`, "" for all
    pub style: String,
    /// From 1
    pub page: u32,
    /// 0 for the site's default
    pub per_page: u32,
}

/// Code to show while waiting for the user to approve the device
#[derive(Clone, Deserialize)]
pub struct DeviceCode {
    device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The device code is a secret until it is used
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .finish()
    }
}

impl DeviceCode {
    /// The URL to encode in a QR code (pre-filled with the code if possible)
    pub fn qr_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Answer of one token poll
pub enum Poll {
    Pending,
    SlowDown,
    /// Approved: the token and the account login
    Granted {
        token: String,
        login: String,
    },
    /// Over: refused, expired or unknown code, with what to tell the user
    Failed(String),
}

#[derive(Deserialize)]
struct Grant {
    access_token: String,
    #[serde(default)]
    user: Me,
}

/// The site's HTTP API
pub struct Client {
    base: String,
    agent: ureq::Agent,
    token: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("linked", &self.token.is_some())
            .finish()
    }
}

/// `User-Agent` the site asks for: `OpenRig/<version> (<arch>-<os>)`
fn user_agent() -> String {
    format!(
        "OpenRig/{} ({}-{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

/// A slug as the site makes them; anything else is not put in a URL
fn check_slug(slug: &str) -> Result<&str, ApiError> {
    if !slug.is_empty()
        && slug.len() <= 120
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(slug)
    } else {
        Err(ApiError::NotFound("No such rig".into()))
    }
}

type Response = ureq::http::Response<ureq::Body>;

impl Client {
    /// Anonymous client of the site at `base` ("https://openrig.eu")
    pub fn new(base: &str) -> Self {
        let base = base.trim().trim_end_matches('/').to_string();
        let agent = ureq::Agent::config_builder()
            // Plain HTTP only for a site set by the owner (a local test)
            .https_only(!base.starts_with("http://"))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(user_agent())
            .build()
            .into();
        Self {
            base,
            agent,
            token: None,
        }
    }

    /// The client of the configured site
    pub fn configured() -> Self {
        Self::new(&base_url())
    }

    /// Calls with this device token from now on
    pub fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    fn authorized<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(token) => request.header("Authorization", format!("Bearer {token}")),
            None => request,
        }
    }

    fn token(&self) -> Result<(), ApiError> {
        self.token
            .as_ref()
            .map(|_| ())
            .ok_or(ApiError::Unauthorized)
    }

    /// The answer, or the error it holds
    fn check(result: Result<Response, ureq::Error>) -> Result<Response, ApiError> {
        let mut response = result.map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        Err(api_error(status, &body))
    }

    fn json<T: serde::de::DeserializeOwned>(
        result: Result<Response, ureq::Error>,
    ) -> Result<T, ApiError> {
        Self::check(result)?
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_json()
            .map_err(|e| ApiError::Server(format!("an unexpected answer ({e})")))
    }

    /// Browse or search
    pub fn list(&self, query: &Query) -> Result<Page, ApiError> {
        let mut request = self.agent.get(self.url("/rigs"));
        let q = query.q.trim();
        if !q.is_empty() {
            request = request.query("q", q.chars().take(100).collect::<String>());
        }
        if !query.sort.is_empty() {
            request = request.query("sort", &query.sort);
        }
        if !query.style.is_empty() {
            request = request.query("style", &query.style);
        }
        if query.page > 1 {
            request = request.query("page", query.page.to_string());
        }
        if query.per_page > 0 {
            request = request.query("per_page", query.per_page.to_string());
        }
        Self::json(request.call())
    }

    /// One rig, with its scenes, dependencies and versions (and, with a
    /// token, whether the account favourited it)
    pub fn rig(&self, slug: &str) -> Result<Detail, ApiError> {
        let slug = check_slug(slug)?;
        Self::json(
            self.authorized(self.agent.get(self.url(&format!("/rigs/{slug}"))))
                .call(),
        )
    }

    /// The `.openrig` file of the current version (or of `version`), read
    /// but not validated (`rigs::import::plan` does)
    pub fn download(&self, slug: &str, version: Option<u32>) -> Result<Rig, ApiError> {
        let slug = check_slug(slug)?;
        let mut request = self.agent.get(self.url(&format!("/rigs/{slug}/download")));
        if let Some(version) = version {
            request = request.query("version", version.to_string());
        }
        let bytes = Self::check(request.call())?
            .body_mut()
            .with_config()
            .limit(super::format::MAX_RIG_BYTES as u64)
            .read_to_vec()
            .map_err(|e| ApiError::Server(format!("an unreadable rig ({e})")))?;
        Rig::from_slice(&bytes).map_err(|e| ApiError::Server(format!("a broken rig: {e}")))
    }

    /// Start linking: a code for the user to enter on the site
    pub fn device_code(&self, device_name: &str) -> Result<DeviceCode, ApiError> {
        let body = serde_json::json!({
            "device_name": device_name,
            "app_version": env!("CARGO_PKG_VERSION"),
        });
        Self::json(self.agent.post(self.url("/device/code")).send_json(body))
    }

    /// Ask whether the user approved the code
    pub fn poll(&self, device_code: &str) -> Result<Poll, ApiError> {
        let body = serde_json::json!({ "device_code": device_code });
        match Self::json::<Grant>(self.agent.post(self.url("/device/token")).send_json(body)) {
            Ok(grant) => Ok(Poll::Granted {
                token: grant.access_token,
                login: grant.user.login,
            }),
            Err(ApiError::Refused { code, message }) => Ok(match code.as_str() {
                "authorization_pending" => Poll::Pending,
                "slow_down" => Poll::SlowDown,
                "access_denied" => Poll::Failed("The link was refused on openrig.eu".into()),
                "expired_token" => Poll::Failed("The code expired, try again".into()),
                "invalid_grant" => Poll::Failed("The code is no longer valid, try again".into()),
                _ => Poll::Failed(message),
            }),
            Err(e) => Err(e),
        }
    }

    /// The linked account
    pub fn me(&self) -> Result<Me, ApiError> {
        self.token()?;
        Self::json(self.authorized(self.agent.get(self.url("/me"))).call())
    }

    /// The account's favourite rigs, newest first
    pub fn favourites(&self) -> Result<Page, ApiError> {
        self.token()?;
        Self::json(
            self.authorized(self.agent.get(self.url("/me/favourites")))
                .call(),
        )
    }

    /// The account's own rigs, hidden ones included, newest first
    pub fn my_rigs(&self, page: u32) -> Result<Page, ApiError> {
        self.token()?;
        Self::json(
            self.authorized(self.agent.get(self.url("/me/rigs")))
                .query("page", page.max(1).to_string())
                .query("per_page", "100")
                .call(),
        )
    }

    /// Favourite a rig, or not any more
    pub fn set_favourite(&self, slug: &str, on: bool) -> Result<Favourite, ApiError> {
        self.token()?;
        let url = self.url(&format!("/rigs/{}/favourite", check_slug(slug)?));
        let result = if on {
            self.authorized(self.agent.put(url)).send_empty()
        } else {
            self.authorized(self.agent.delete(url)).call()
        };
        Self::json(result)
    }

    /// Publish a new rig; `style` "" lets the site take it from the tags
    pub fn publish(&self, file: &[u8], style: &str) -> Result<Summary, ApiError> {
        self.token()?;
        let mut request = self.authorized(self.agent.post(self.url("/rigs")));
        if !style.is_empty() {
            request = request.query("style", style);
        }
        Self::json(request.content_type(MEDIA_TYPE).send(file))
    }

    /// Publish `file` as a new version of the account's rig `slug`
    pub fn new_version(
        &self,
        slug: &str,
        file: &[u8],
        changelog: &str,
    ) -> Result<Summary, ApiError> {
        self.token()?;
        let url = self.url(&format!("/rigs/{}/versions", check_slug(slug)?));
        let mut request = self.authorized(self.agent.post(url));
        let changelog = changelog.trim();
        if !changelog.is_empty() {
            request = request.query("changelog", changelog);
        }
        Self::json(request.content_type(MEDIA_TYPE).send(file))
    }

    /// Revoke the token of this client (unlinks the device)
    pub fn revoke(&self) -> Result<(), ApiError> {
        self.token()?;
        Self::check(
            self.authorized(self.agent.delete(self.url("/me/token")))
                .call(),
        )?;
        Ok(())
    }

    /// The account's rig named `name` (case and spaces aside), for a new
    /// version of it: from `GET /me/rigs`, or, from a site without it (404),
    /// a search by name kept to the account's own
    pub fn own_rig(&self, login: &str, name: &str) -> Result<Option<Summary>, ApiError> {
        let name = name.trim();
        if name.is_empty() || login.is_empty() {
            return Ok(None);
        }
        if self.token.is_some() {
            // 10 pages of 100: more than the publish caps allow in months
            for page in 1..=10 {
                match self.my_rigs(page) {
                    Ok(mine) => {
                        let more = mine.has_more() && !mine.items.is_empty();
                        if let Some(own) = mine.items.into_iter().find(|r| same_name(&r.name, name))
                        {
                            return Ok(Some(own));
                        }
                        if !more {
                            return Ok(None);
                        }
                    }
                    Err(ApiError::NotFound(_)) if page == 1 => break,
                    Err(e) => return Err(e),
                }
            }
        }
        let page = self.list(&Query {
            q: name.to_string(),
            sort: "new".into(),
            per_page: 100,
            ..Default::default()
        })?;
        Ok(page
            .items
            .into_iter()
            .find(|r| r.author.login.eq_ignore_ascii_case(login) && same_name(&r.name, name)))
    }
}

fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// What keeps an exported rig from being published: its local plugins
/// and files by name, else the reasons the format crate gives
pub fn publish_blockers(exported: &Exported) -> Vec<String> {
    let mut blockers: Vec<String> = exported
        .local_items()
        .into_iter()
        .map(|name| format!("{name} (only on this pedalboard)"))
        .collect();
    if blockers.is_empty() {
        blockers.extend(
            exported
                .report
                .issues
                .iter()
                .filter(|i| i.level != Level::Warning)
                .map(|i| i.message.clone()),
        );
    }
    blockers
}

/// The link of this pedalboard to a site account
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Account {
    /// The site the token belongs to
    pub base_url: String,
    pub login: String,
    token: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the token
        f.debug_struct("Account")
            .field("base_url", &self.base_url)
            .field("login", &self.login)
            .finish()
    }
}

impl Account {
    pub fn new(base_url: &str, login: &str, token: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            login: login.to_string(),
            token: token.to_string(),
        }
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("openrig.json")
    }

    /// The account linked to the site at `base_url`, if any (a link to
    /// another site is ignored, not deleted)
    pub fn load(data_dir: &Path, base_url: &str) -> Option<Self> {
        let path = Self::path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "openrig.eu link not read");
                return None;
            }
        };
        // Only where the error is: serde's message may quote the token
        let account: Self = match serde_json::from_str(&text) {
            Ok(account) => account,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    line = e.line(),
                    column = e.column(),
                    "Unreadable openrig.eu link ignored"
                );
                return None;
            }
        };
        (account.base_url.trim_end_matches('/') == base_url.trim_end_matches('/')
            && !account.token.is_empty())
        .then_some(account)
    }

    /// Save it, readable by the owner only
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("Cannot create {}", data_dir.display()))?;
        crate::persist::write_private(&Self::path(data_dir), &serde_json::to_vec_pretty(self)?)
    }

    /// Forget the link on this pedalboard
    pub fn delete(data_dir: &Path) -> Result<()> {
        match std::fs::remove_file(Self::path(data_dir)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("Cannot remove the openrig.eu link"),
        }
    }

    /// A client of the site with this account
    pub fn client(&self) -> Client {
        Client::new(&self.base_url).with_token(&self.token)
    }
}

/// Unlink: revoke the token on the site (best effort: the user can also
/// revoke it in the site's settings) and delete it here
pub fn unlink(data_dir: &Path, account: &Account) -> Result<()> {
    match account.client().revoke() {
        Ok(()) | Err(ApiError::Unauthorized) => {}
        Err(e) => warn!(error = %e, "openrig.eu token not revoked, deleted here only"),
    }
    Account::delete(data_dir)?;
    info!("Unlinked from openrig.eu");
    Ok(())
}

/// Progress of a link, reported from its thread
pub enum LinkEvent {
    /// Show this code to the user
    Code(DeviceCode),
    /// Linked; the account is saved
    Linked(Account),
    Failed(String),
    Cancelled,
}

/// A device link running in the background
pub struct Linking {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Linking {
    /// Link to the site at `base_url`, saving the account in `data_dir`;
    /// `on_event` is called from the linking thread
    pub fn start(
        base_url: String,
        data_dir: PathBuf,
        device_name: String,
        on_event: impl Fn(LinkEvent) + Send + 'static,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("openrig-link".into())
            .spawn(move || {
                let client = Client::new(&base_url);
                let wait = |d: Duration| wait(d, &stop);
                let last = link(&client, &data_dir, &device_name, &wait, &on_event);
                on_event(last);
            })
            .inspect_err(|e| error!(error = %e, "openrig.eu link not started"))
            .ok();
        Self { cancel, thread }
    }
}

impl Drop for Linking {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // The thread notices within a fraction of a second; don't block on it
        drop(self.thread.take());
    }
}

/// Sleep for `duration`, false if cancelled meanwhile
fn wait(duration: Duration, cancel: &AtomicBool) -> bool {
    let start = Instant::now();
    while start.elapsed() < duration {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !cancel.load(Ordering::Relaxed)
}

/// The device flow: ask for a code, show it, poll until it is approved,
/// refused or expired. `wait` sleeps between polls and says whether to go
/// on (false: cancelled).
pub fn link(
    client: &Client,
    data_dir: &Path,
    device_name: &str,
    wait: &dyn Fn(Duration) -> bool,
    on_event: &dyn Fn(LinkEvent),
) -> LinkEvent {
    let code = match client.device_code(device_name) {
        Ok(code) => code,
        Err(e) => return LinkEvent::Failed(e.to_string()),
    };
    let device_code = code.device_code.clone();
    let mut interval = Duration::from_secs(code.interval.clamp(1, MAX_POLL_SECS));
    let lifetime = Duration::from_secs(code.expires_in.min(MAX_CODE_SECS));
    let mut waited = Duration::ZERO;
    on_event(LinkEvent::Code(code));
    loop {
        if !wait(interval) {
            return LinkEvent::Cancelled;
        }
        waited += interval;
        if waited > lifetime {
            return LinkEvent::Failed("The code expired, try again".into());
        }
        match client.poll(&device_code) {
            Ok(Poll::Pending) => {}
            Ok(Poll::SlowDown) => {
                interval =
                    (interval + Duration::from_secs(5)).min(Duration::from_secs(MAX_POLL_SECS));
            }
            Ok(Poll::Granted { token, login }) => {
                let account = Account::new(client.base(), &login, &token);
                if let Err(e) = account.save(data_dir) {
                    return LinkEvent::Failed(format!("{e:#}"));
                }
                info!(login = %login, "Linked to openrig.eu");
                return LinkEvent::Linked(account);
            }
            Ok(Poll::Failed(message)) => return LinkEvent::Failed(message),
            // Network trouble, a rate limit: keep polling until the deadline
            Err(e) => warn!(error = %e, "openrig.eu poll failed, retrying"),
        }
    }
}

/// Name the site shows for this pedalboard in the account's devices
pub fn device_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty() && h.len() <= 40);
    match host {
        Some(host) => format!("OpenRig ({host})"),
        None => "OpenRig pedalboard".to_string(),
    }
}

#[cfg(test)]
mod tests;
