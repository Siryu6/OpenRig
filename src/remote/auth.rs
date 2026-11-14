//! Password and sessions of the web remote.
//!
//! The password is kept as a salted Argon2id hash (`remote.json` in the data
//! directory, readable by its owner only). Logging in gives a random
//! session token in a cookie; tokens live in memory only (an app restart
//! signs every device out) and are all revoked when the password changes
//! or from Settings. Failed logins slow down per address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use argon2::Argon2;
use argon2::{PasswordHash, PasswordHasher, PasswordVerifier};
use serde::{Deserialize, Serialize};

use crate::sync::MutexExt;

/// File of the password hash, in the data directory
pub const PASSWORD_FILE: &str = "remote.json";
/// Password length: also the WPA2 key of the hotspot (8-63 characters)
pub const MIN_PASSWORD: usize = 8;
pub const MAX_PASSWORD: usize = 63;
/// Sessions unused for this long are dropped
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);
/// Signed-in devices kept at most (the oldest go first)
const MAX_SESSIONS: usize = 32;
/// Failed logins allowed before waiting, then the wait doubles up to MAX_WAIT
const FREE_FAILURES: u32 = 3;
const MAX_WAIT: Duration = Duration::from_secs(300);

/// Why a password is refused, for the user
pub fn check_password(password: &str) -> Result<()> {
    let len = password.chars().count();
    if !(MIN_PASSWORD..=MAX_PASSWORD).contains(&len) {
        bail!("Use {MIN_PASSWORD} to {MAX_PASSWORD} characters");
    }
    if !password.chars().all(|c| (' '..='~').contains(&c)) {
        bail!("Use letters, digits, spaces and symbols of a US keyboard only");
    }
    Ok(())
}

/// Salted Argon2id hash of `password`, as a PHC string
pub fn hash_password(password: &str) -> Result<String> {
    check_password(password)?;
    // A random salt from the system's generator
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("Password not hashed: {e}"))
}

/// Whether `password` matches `hash` (constant time in the library)
pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PasswordFile {
    password_hash: String,
}

/// The password hash on disk, and in memory
pub struct PasswordStore {
    path: PathBuf,
    hash: Mutex<Option<String>>,
}

impl PasswordStore {
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(PASSWORD_FILE);
        let hash = read_hash(&path);
        Self {
            path,
            hash: Mutex::new(hash),
        }
    }

    /// A password is set: the remote is on
    pub fn is_set(&self) -> bool {
        self.hash.locked().is_some()
    }

    pub fn verify(&self, password: &str) -> bool {
        let hash = self.hash.locked().clone();
        hash.is_some_and(|h| verify_password(password, &h))
    }

    pub fn set(&self, password: &str) -> Result<()> {
        let hash = hash_password(password)?;
        write_hash(&self.path, &hash)?;
        *self.hash.locked() = Some(hash);
        Ok(())
    }

    /// Remove the password: the remote is off
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("Password not removed"),
        }
        *self.hash.locked() = None;
        Ok(())
    }
}

fn read_hash(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let file: PasswordFile = serde_json::from_str(&text).ok()?;
    PasswordHash::new(&file.password_hash).ok()?;
    Some(file.password_hash)
}

/// Write a hash to the password file (owner only)
pub fn write_hash(path: &Path, hash: &str) -> Result<()> {
    let file = PasswordFile {
        password_hash: hash.to_string(),
    };
    crate::persist::write_private(path, serde_json::to_string_pretty(&file)?.as_bytes())
}

/// A signed-in device
#[derive(Debug, Clone)]
pub struct Session {
    token: [u8; 32],
    pub addr: IpAddr,
    pub created: Instant,
    pub last_seen: Instant,
}

/// Signed-in devices
#[derive(Default)]
pub struct Sessions {
    list: Mutex<Vec<Session>>,
}

impl Sessions {
    /// A new session for a device at `addr`: its token, to put in the cookie
    pub fn create(&self, addr: IpAddr) -> Result<String> {
        let mut token = [0u8; 32];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("No random numbers: {e}"))?;
        let now = Instant::now();
        let mut list = self.list.locked();
        list.retain(|s| now.duration_since(s.last_seen) < SESSION_IDLE);
        if list.len() >= MAX_SESSIONS {
            list.sort_by_key(|s| s.last_seen);
            list.remove(0);
        }
        list.push(Session {
            token,
            addr,
            created: now,
            last_seen: now,
        });
        Ok(encode_token(&token))
    }

    /// Whether `token` is a live session (and mark it used)
    pub fn check(&self, token: &str) -> bool {
        let Some(token) = decode_token(token) else {
            return false;
        };
        let now = Instant::now();
        let mut list = self.list.locked();
        list.retain(|s| now.duration_since(s.last_seen) < SESSION_IDLE);
        // Every session is compared, each in constant time
        let mut found = None;
        for (i, session) in list.iter().enumerate() {
            if constant_time_eq(&session.token, &token) {
                found = Some(i);
            }
        }
        match found {
            Some(i) => {
                list[i].last_seen = now;
                true
            }
            None => false,
        }
    }

    /// Sign one device out
    pub fn remove(&self, token: &str) {
        if let Some(token) = decode_token(token) {
            self.list
                .locked()
                .retain(|s| !constant_time_eq(&s.token, &token));
        }
    }

    /// Sign every device out
    pub fn revoke_all(&self) -> usize {
        let mut list = self.list.locked();
        let n = list.len();
        list.clear();
        n
    }

    pub fn count(&self) -> usize {
        self.list.locked().len()
    }
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn encode_token(token: &[u8; 32]) -> String {
    token
        .iter()
        .flat_map(|b| {
            [
                HEX[(b >> 4) as usize] as char,
                HEX[(b & 15) as usize] as char,
            ]
        })
        .collect()
}

fn decode_token(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let digit = |c: u8| HEX.iter().position(|&h| h == c).map(|v| v as u8);
    let mut token = [0u8; 32];
    for (i, pair) in bytes.chunks(2).enumerate() {
        token[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(token)
}

/// Failed logins of one address
#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
}

/// Slows down password guessing: after a few failures, an address waits
/// longer and longer before it may try again
#[derive(Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<IpAddr, Failures>>,
}

impl LoginLimiter {
    /// How long `addr` must wait before trying, if it must
    pub fn wait(&self, addr: IpAddr) -> Option<Duration> {
        self.wait_at(addr, Instant::now())
    }

    fn wait_at(&self, addr: IpAddr, now: Instant) -> Option<Duration> {
        let failures = *self.failures.locked().get(&addr)?;
        let wait = backoff(failures.count);
        let since = now.saturating_duration_since(failures.last);
        (since < wait).then(|| wait - since)
    }

    pub fn failed(&self, addr: IpAddr) {
        self.failed_at(addr, Instant::now());
    }

    fn failed_at(&self, addr: IpAddr, now: Instant) {
        let mut failures = self.failures.locked();
        // Forget addresses idle for long (a phone's address changes)
        failures.retain(|_, f| now.saturating_duration_since(f.last) < MAX_WAIT * 4);
        let entry = failures.entry(addr).or_insert(Failures {
            count: 0,
            last: now,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last = now;
    }

    pub fn succeeded(&self, addr: IpAddr) {
        self.failures.locked().remove(&addr);
    }
}

/// Wait after `count` failures: none for the first few, then 2 s, 4 s…
fn backoff(count: u32) -> Duration {
    match count.checked_sub(FREE_FAILURES) {
        None | Some(0) => Duration::ZERO,
        Some(n) => Duration::from_secs(1u64 << n.min(9)).min(MAX_WAIT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_hashed_and_checked() {
        assert!(check_password("short").is_err());
        assert!(check_password(&"x".repeat(64)).is_err());
        assert!(check_password("pässwörd123").is_err());
        assert!(check_password("correct horse").is_ok());
        let hash = hash_password("correct horse").unwrap_or_default();
        assert!(hash.starts_with("$argon2id$"));
        assert!(!hash.contains("correct"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("correct horsE", &hash));
        assert!(!verify_password("correct horse", "not a hash"));
        // Salted: the same password hashes differently
        assert_ne!(hash, hash_password("correct horse").unwrap_or_default());
    }

    #[test]
    fn passwords_set_before_argon2_0_6_still_match() {
        // Hashed by argon2 0.5 (the remote password of earlier versions)
        let old = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$ibBMSBdQczc9WQRFzQSb3MHZeXENP+/qRLSkj8eYUu4";
        assert!(verify_password("old password", old));
        assert!(!verify_password("old passwore", old));
    }

    #[test]
    fn password_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("openrig-auth-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = PasswordStore::open(&dir);
        assert!(!store.is_set());
        assert!(!store.verify("anything at all"));
        assert!(store.set("too short").is_ok());
        assert!(store.set("bad").is_err());
        assert!(store.verify("too short"));
        let text = std::fs::read_to_string(dir.join(PASSWORD_FILE)).unwrap_or_default();
        assert!(!text.contains("too short"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(PASSWORD_FILE))
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0);
            assert_eq!(mode, 0o600);
        }
        // Read again from disk
        assert!(PasswordStore::open(&dir).verify("too short"));
        assert!(store.clear().is_ok());
        assert!(!store.is_set());
        assert!(!PasswordStore::open(&dir).is_set());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_are_checked_and_revoked() {
        let sessions = Sessions::default();
        let addr: IpAddr = [10, 42, 0, 2].into();
        let a = sessions.create(addr).unwrap_or_default();
        let b = sessions.create(addr).unwrap_or_default();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(sessions.check(&a));
        assert!(sessions.check(&b));
        assert!(!sessions.check(&"0".repeat(64)));
        assert!(!sessions.check("garbage"));
        assert!(!sessions.check(&a.to_uppercase()));
        sessions.remove(&a);
        assert!(!sessions.check(&a));
        assert!(sessions.check(&b));
        assert_eq!(sessions.revoke_all(), 1);
        assert!(!sessions.check(&b));
        assert_eq!(sessions.count(), 0);
    }

    #[test]
    fn sessions_are_capped() {
        let sessions = Sessions::default();
        let addr: IpAddr = [192, 168, 0, 2].into();
        let first = sessions.create(addr).unwrap_or_default();
        for _ in 0..MAX_SESSIONS {
            let _ = sessions.create(addr);
        }
        assert_eq!(sessions.count(), MAX_SESSIONS);
        assert!(!sessions.check(&first));
    }

    #[test]
    fn failed_logins_slow_down() {
        let limiter = LoginLimiter::default();
        let addr: IpAddr = [10, 42, 0, 9].into();
        let other: IpAddr = [10, 42, 0, 10].into();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES {
            assert_eq!(limiter.wait_at(addr, now), None);
            limiter.failed_at(addr, now);
        }
        assert_eq!(limiter.wait_at(addr, now), None);
        limiter.failed_at(addr, now);
        assert_eq!(limiter.wait_at(addr, now), Some(Duration::from_secs(2)));
        limiter.failed_at(addr, now);
        assert_eq!(limiter.wait_at(addr, now), Some(Duration::from_secs(4)));
        assert_eq!(
            limiter.wait_at(addr, now + Duration::from_secs(5)),
            None,
            "the wait is over"
        );
        assert_eq!(limiter.wait_at(other, now), None);
        for _ in 0..50 {
            limiter.failed_at(addr, now);
        }
        assert_eq!(limiter.wait_at(addr, now), Some(MAX_WAIT));
        limiter.succeeded(addr);
        assert_eq!(limiter.wait_at(addr, now), None);
    }
}
