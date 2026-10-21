//! TONE3000 account link.
//!
//! The pedalboard signs in with the OAuth device flow: it shows a short code
//! and a QR code, the user approves it on their phone, with their own account.
//! The pedalboard is the OpenRig application of TONE3000: its *publishable*
//! key (`t3k_pub_…`) is built in, as publishable keys are meant to be. A
//! developer can use the key of their own application instead (Settings, or
//! `PEDALBOARD_TONE3000_CLIENT_ID`). The secret key (`t3k_cs_…`) is never
//! needed on a device and is refused.
//!
//! The settings (key + session tokens) live in `data_dir/tone3000.json`,
//! readable by the owner only on Unix.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::state::app_state::data_dir;

pub const API: &str = "https://www.tone3000.com/api/v1";
pub const WEBSITE: &str = "https://www.tone3000.com";

/// Publishable key of the OpenRig application, set at build time with
/// `OPENRIG_TONE3000_CLIENT_ID` to build with another one
pub const OPENRIG_CLIENT_ID: &str = match option_env!("OPENRIG_TONE3000_CLIENT_ID") {
    Some(key) => key,
    None => "t3k_pub_NIxDz-jtyrfe8zo-28hzypotyOYp7qPQ",
};
/// Version of `tone3000.json`: 2 since the OpenRig key is built in
const SETTINGS_VERSION: u32 = 2;

const PUBLISHABLE_PREFIX: &str = "t3k_pub_";
const SECRET_PREFIX: &str = "t3k_cs_";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Refresh the access token this long before it expires
const EXPIRY_MARGIN_SECS: i64 = 60;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Check a key typed by the user; returns it trimmed
pub fn check_client_id(key: &str) -> Result<String> {
    let key = key.trim();
    if key.starts_with(SECRET_PREFIX) {
        bail!(
            "This is a secret key: never put it on a device. Use the publishable key (t3k_pub_…)"
        );
    }
    if !key.starts_with(PUBLISHABLE_PREFIX) || key.len() <= PUBLISHABLE_PREFIX.len() {
        bail!("A TONE3000 publishable key starts with t3k_pub_");
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("The key contains spaces");
    }
    Ok(key.to_string())
}

/// A signed-in account
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub username: String,
    access_token: String,
    refresh_token: String,
    /// Unix time the access token expires at
    expires_at: i64,
}

/// What `tone3000.json` holds
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    version: u32,
    /// Publishable key of the user's own application, "" for OpenRig's
    #[serde(default)]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("tone3000.json")
    }

    /// Load the settings; a missing or broken file gives empty settings
    pub fn load() -> Self {
        let path = Self::path();
        let mut settings = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                warn!("Ignoring {}: {}", path.display(), e);
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        if settings.migrate() {
            if let Err(e) = settings.save() {
                warn!("{:#}", e);
            }
        }
        if settings.client_id.is_empty() {
            if let Ok(key) = std::env::var("PEDALBOARD_TONE3000_CLIENT_ID") {
                settings.client_id = key;
            }
        }
        // Never show a secret key in Settings (the value is not logged)
        if !settings.client_id.is_empty() {
            match check_client_id(&settings.client_id) {
                Ok(key) => settings.client_id = key,
                Err(e) => {
                    warn!("TONE3000 key ignored: {e}");
                    settings.client_id.clear();
                }
            }
        }
        settings
    }

    /// Move settings of an older version to the OpenRig key; true when they
    /// changed. Up to 0.5, every user signed in with the key of their own
    /// application: its session cannot be carried over (a refresh token
    /// belongs to the key it was issued for), so the user connects again.
    fn migrate(&mut self) -> bool {
        if self.version >= SETTINGS_VERSION {
            return false;
        }
        self.version = SETTINGS_VERSION;
        if self.client_id.trim() == OPENRIG_CLIENT_ID {
            // Already the OpenRig application: the session stays
            self.client_id.clear();
        } else if !self.client_id.is_empty() || self.session.is_some() {
            info!("TONE3000: OpenRig now has its own application, connect again");
            self.client_id.clear();
            self.session = None;
        }
        true
    }

    /// The publishable key to sign in with
    pub fn key(&self) -> &str {
        if self.client_id.is_empty() {
            OPENRIG_CLIENT_ID
        } else {
            &self.client_id
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let dir = path.parent().unwrap_or(&path);
        std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        write_private(&tmp, serde_json::to_string_pretty(self)?.as_bytes())?;
        std::fs::rename(&tmp, &path).with_context(|| format!("Cannot write {}", path.display()))?;
        Ok(())
    }

    /// Forget the session (the publishable key is kept)
    pub fn disconnect(&mut self) -> Result<()> {
        self.session = None;
        self.save()
    }

    pub fn connected(&self) -> bool {
        self.session.is_some()
    }

    /// A valid access token, refreshed (and saved) when it expired
    pub fn access_token(&mut self) -> Result<String, AuthError> {
        let session = self.session.as_ref().ok_or(AuthError::SignedOut)?;
        if session.expires_at - EXPIRY_MARGIN_SECS <= now() {
            return self.refresh();
        }
        Ok(session.access_token.clone())
    }

    /// A new access token after the API refused `refused` (revoked early,
    /// clock off): only a refused refresh means the link is gone. Another
    /// request may have refreshed it meanwhile.
    pub fn access_token_after(&mut self, refused: &str) -> Result<String, AuthError> {
        let session = self.session.as_ref().ok_or(AuthError::SignedOut)?;
        if session.access_token != refused {
            return Ok(session.access_token.clone());
        }
        self.refresh()
    }

    /// Get (and save) a new access token
    fn refresh(&mut self) -> Result<String, AuthError> {
        let client_id = self.key().to_string();
        let session = self.session.as_mut().ok_or(AuthError::SignedOut)?;
        let token = refresh_token(&client_id, &session.refresh_token)?;
        session.access_token = token.access_token;
        session.refresh_token = token
            .refresh_token
            .unwrap_or_else(|| session.refresh_token.clone());
        session.expires_at = now() + token.expires_in;
        let access_token = session.access_token.clone();
        if let Err(e) = self.save() {
            warn!("{:#}", e);
        }
        Ok(access_token)
    }
}

/// Write a file only its owner can read (the tokens are credentials)
fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("Cannot write {}", path.display()))?;
    // The mode above only applies to a new file
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// Why an authenticated call failed
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Not connected to TONE3000")]
    SignedOut,
    /// The account link was revoked or expired: sign in again
    #[error("TONE3000 session expired, connect again in Settings")]
    Revoked,
    /// Not allowed or not found (private tone, partner-only endpoint)
    #[error("TONE3000 refused the request ({0})")]
    Refused(String),
    /// Network or server trouble: keep the session, try later
    #[error("TONE3000 is unreachable: {0}")]
    Unavailable(String),
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_expiry")]
    expires_in: i64,
}

fn default_expiry() -> i64 {
    3600
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Code to show while waiting for the user to approve the device
#[derive(Clone, Debug, Deserialize)]
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

impl DeviceCode {
    /// The URL to encode in the QR code (pre-filled with the code if possible)
    pub fn qr_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Answer of one token poll
enum Poll {
    Pending,
    SlowDown,
    Granted(TokenGrant),
    Failed(String),
}

struct TokenGrant {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log tokens
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

fn parse_poll(status: u16, body: &str) -> Poll {
    if (200..300).contains(&status) {
        return match serde_json::from_str::<TokenResponse>(body) {
            Ok(TokenResponse {
                access_token,
                refresh_token: Some(refresh_token),
                expires_in,
            }) => Poll::Granted(TokenGrant {
                access_token,
                refresh_token,
                expires_in,
            }),
            Ok(_) => Poll::Failed("TONE3000 sent no refresh token".into()),
            Err(_) => Poll::Failed("Unexpected answer from TONE3000".into()),
        };
    }
    match serde_json::from_str::<OAuthError>(body) {
        Ok(e) => match e.error.as_str() {
            "authorization_pending" => Poll::Pending,
            "slow_down" => Poll::SlowDown,
            "access_denied" => Poll::Failed("Connection refused on TONE3000".into()),
            "expired_token" => Poll::Failed("The code expired, try again".into()),
            "invalid_client" => Poll::Failed("TONE3000 does not know this publishable key".into()),
            _ => Poll::Failed(e.error_description.unwrap_or(e.error)),
        },
        Err(_) => Poll::Failed(format!("TONE3000 answered HTTP {status}")),
    }
}

/// HTTPS client for API calls
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("pedalboard/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// HTTPS client for model downloads: a large model over a weak Wi-Fi link
/// takes longer than an API call, as long as data keeps coming
pub(crate) fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .user_agent(concat!("pedalboard/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// POST a form, returning the status and the body
fn post_form(path: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
    let mut response = agent()
        .post(format!("{API}{path}"))
        .send_form(form.iter().copied())
        .map_err(|e| anyhow!("Cannot reach TONE3000: {e}"))?;
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string()?;
    Ok((status, body))
}

/// GET an API path with the account token
pub fn get_json<T: serde::de::DeserializeOwned>(
    access_token: &str,
    path: &str,
) -> Result<T, AuthError> {
    super::limiter::acquire(None);
    let mut response = agent()
        .get(format!("{API}{path}"))
        .header("Authorization", format!("Bearer {access_token}"))
        .call()
        .map_err(|e| AuthError::Unavailable(e.to_string()))?;
    match response.status().as_u16() {
        401 => Err(AuthError::Revoked),
        403 | 404 => Err(AuthError::Refused(format!(
            "HTTP {}",
            response.status().as_u16()
        ))),
        200..=299 => response
            .body_mut()
            .read_json()
            .map_err(|e| AuthError::Unavailable(e.to_string())),
        status => Err(AuthError::Unavailable(format!("HTTP {status}"))),
    }
}

fn refresh_token(client_id: &str, refresh_token: &str) -> Result<TokenResponse, AuthError> {
    let (status, body) = post_form(
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
    )
    .map_err(|e| AuthError::Unavailable(e.to_string()))?;
    match status {
        200..=299 => serde_json::from_str(&body).map_err(|e| AuthError::Unavailable(e.to_string())),
        400 | 401 | 403 => Err(AuthError::Revoked),
        _ => Err(AuthError::Unavailable(format!("HTTP {status}"))),
    }
}

/// Start the device flow: ask TONE3000 for a user code
pub fn request_device_code(client_id: &str) -> Result<DeviceCode> {
    let (status, body) = post_form("/oauth/device_authorization", &[("client_id", client_id)])?;
    if !(200..300).contains(&status) {
        return Err(match parse_poll(status, &body) {
            Poll::Failed(message) => anyhow!(message),
            _ => anyhow!("TONE3000 answered HTTP {status}"),
        });
    }
    serde_json::from_str(&body).context("Unexpected answer from TONE3000")
}

#[derive(Deserialize)]
struct User {
    username: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

/// Avatar of the signed-in account, if it has one
pub fn avatar(access_token: &str) -> Result<Option<String>, AuthError> {
    let user: User = get_json(access_token, "/user")?;
    Ok(user.avatar_url.filter(|u| !u.is_empty()))
}

/// Progress of a device-flow connection, reported from its thread
pub enum ConnectEvent {
    /// Show this code to the user
    Code(DeviceCode),
    /// Signed in; the settings are saved
    Connected(Settings),
    Failed(String),
    Cancelled,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("client_id", &self.key())
            .field("user", &self.session.as_ref().map(|s| &s.username))
            .finish()
    }
}

/// A device-flow connection running in the background
pub struct Connection {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Connection {
    /// Run the device flow for `client_id` ("" for the OpenRig key);
    /// `on_event` is called from the connection thread.
    pub fn start(client_id: String, on_event: impl Fn(ConnectEvent) + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("tone3000-connect".into())
            .spawn(move || {
                let last = run_device_flow(&client_id, &stop, &on_event);
                on_event(last);
            })
            .ok();
        Self { cancel, thread }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // The thread notices within a second; don't block the UI on it
        drop(self.thread.take());
    }
}

/// Sleep for `duration`, false if cancelled meanwhile
fn wait(duration: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !cancel.load(Ordering::Relaxed)
}

fn run_device_flow(
    client_id: &str,
    cancel: &AtomicBool,
    on_event: &dyn Fn(ConnectEvent),
) -> ConnectEvent {
    let key = if client_id.is_empty() {
        OPENRIG_CLIENT_ID
    } else {
        client_id
    };
    let code = match request_device_code(key) {
        Ok(code) => code,
        Err(e) => return ConnectEvent::Failed(format!("{e:#}")),
    };
    let device_code = code.device_code.clone();
    let mut interval = Duration::from_secs(code.interval.max(1));
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    on_event(ConnectEvent::Code(code));

    let grant = loop {
        if !wait(interval, cancel) {
            return ConnectEvent::Cancelled;
        }
        if Instant::now() >= deadline {
            return ConnectEvent::Failed("The code expired, try again".into());
        }
        let poll = post_form(
            "/oauth/token",
            &[
                ("grant_type", DEVICE_GRANT),
                ("device_code", &device_code),
                ("client_id", key),
            ],
        );
        match poll.map(|(status, body)| parse_poll(status, &body)) {
            Ok(Poll::Pending) => {}
            Ok(Poll::SlowDown) => interval += Duration::from_secs(5),
            Ok(Poll::Granted(grant)) => break grant,
            Ok(Poll::Failed(message)) => return ConnectEvent::Failed(message),
            // Transient network trouble: keep polling until the deadline
            Err(e) => warn!("TONE3000 poll: {:#}", e),
        }
    };

    let username = match get_json::<User>(&grant.access_token, "/user") {
        Ok(user) => user.username,
        Err(e) => return ConnectEvent::Failed(e.to_string()),
    };
    let settings = Settings {
        version: SETTINGS_VERSION,
        client_id: client_id.to_string(),
        session: Some(Session {
            username,
            access_token: grant.access_token,
            refresh_token: grant.refresh_token,
            expires_at: now() + grant.expires_in,
        }),
    };
    if let Err(e) = settings.save() {
        return ConnectEvent::Failed(format!("{e:#}"));
    }
    info!("Connected to TONE3000");
    ConnectEvent::Connected(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_keys() {
        assert_eq!(
            check_client_id("  t3k_pub_abc-1_X ").unwrap(),
            "t3k_pub_abc-1_X"
        );
        let secret = check_client_id("t3k_cs_abc").unwrap_err().to_string();
        assert!(secret.contains("secret"), "{secret}");
        assert!(check_client_id("t3k_pub_").is_err());
        assert!(check_client_id("hello").is_err());
        assert!(check_client_id("t3k_pub_a b").is_err());
    }

    #[test]
    fn parses_token_polls() {
        let pending = r#"{"error":"authorization_pending"}"#;
        assert!(matches!(parse_poll(400, pending), Poll::Pending));
        assert!(matches!(
            parse_poll(400, r#"{"error":"slow_down"}"#),
            Poll::SlowDown
        ));
        match parse_poll(400, r#"{"error":"access_denied"}"#) {
            Poll::Failed(message) => assert!(message.contains("refused")),
            _ => panic!("access_denied is final"),
        }
        assert!(matches!(parse_poll(502, "<html>"), Poll::Failed(_)));
        let granted =
            r#"{"access_token":"a","refresh_token":"r","expires_in":60,"token_type":"bearer"}"#;
        match parse_poll(200, granted) {
            Poll::Granted(grant) => {
                assert_eq!(
                    (grant.access_token.as_str(), grant.refresh_token.as_str()),
                    ("a", "r")
                );
                assert_eq!(grant.expires_in, 60);
            }
            _ => panic!("token not granted"),
        }
    }

    #[test]
    fn settings_hide_tokens() {
        let settings = Settings {
            version: SETTINGS_VERSION,
            client_id: "t3k_pub_x".into(),
            session: Some(Session {
                username: "me".into(),
                access_token: "ACCESS".into(),
                refresh_token: "REFRESH".into(),
                expires_at: 0,
            }),
        };
        let debug = format!("{settings:?}");
        assert!(
            !debug.contains("ACCESS") && !debug.contains("REFRESH"),
            "{debug}"
        );
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(restored.connected());
        assert_eq!(restored.session.unwrap().username, "me");
    }

    #[test]
    fn older_settings_move_to_the_openrig_key() {
        let session = || Session {
            username: "me".into(),
            access_token: "A".into(),
            refresh_token: "R".into(),
            expires_at: 0,
        };
        // Own key of 0.5 and before: signed out, connects with OpenRig's
        let mut own: Settings = serde_json::from_str(r#"{"client_id":"t3k_pub_mine"}"#).unwrap();
        own.session = Some(session());
        assert!(own.migrate());
        assert!(!own.connected());
        assert_eq!(own.key(), OPENRIG_CLIENT_ID);
        // Already the OpenRig key: still signed in
        let mut openrig = Settings {
            client_id: OPENRIG_CLIENT_ID.into(),
            session: Some(session()),
            ..Default::default()
        };
        assert!(openrig.migrate());
        assert!(openrig.connected() && openrig.client_id.is_empty());
        // Current settings, even with an own key, are left alone
        let mut current = Settings {
            version: SETTINGS_VERSION,
            client_id: "t3k_pub_mine".into(),
            session: Some(session()),
        };
        assert!(!current.migrate());
        assert!(current.connected());
        assert_eq!(current.key(), "t3k_pub_mine");
        assert!(check_client_id(OPENRIG_CLIENT_ID).is_ok());
    }

    #[test]
    fn a_token_refreshed_meanwhile_is_reused() {
        let mut settings = Settings {
            version: SETTINGS_VERSION,
            client_id: "t3k_pub_x".into(),
            session: Some(Session {
                username: "me".into(),
                access_token: "NEW".into(),
                refresh_token: "REFRESH".into(),
                expires_at: now() + 3600,
            }),
        };
        // No network call: the refused token was already replaced
        assert_eq!(settings.access_token_after("OLD").unwrap(), "NEW");
        settings.session = None;
        assert!(matches!(
            settings.access_token_after("NEW"),
            Err(AuthError::SignedOut)
        ));
    }
}
