//! Secrets of pedalboard.txt, consumed at boot (`pedalboard remote-setup`,
//! run as root by the boot scripts before NetworkManager and the app).
//!
//! A board without a screen is set up by editing pedalboard.txt on the
//! boot partition from a computer. The secrets written there must not stay
//! in a file anyone with the card can read, so at boot:
//!
//! - `remote_password` is stored as a salted hash (the web remote) and as
//!   the WPA2 key of the hotspot profile (root only), then replaced with
//!   `(saved)`. `off` turns the remote off and forgets the password.
//! - `wifi_ssid` / `wifi_password` become a NetworkManager profile (root
//!   only), then the password is replaced with `(saved)`.
//!
//! Values starting with "(" are those markers: nothing to do. An empty
//! value keeps what was saved. A value that cannot be used is replaced with
//! a `(not saved: why)` marker, so the user sees it from the computer.

use std::path::Path;

use anyhow::{Context, Result};

use super::auth::{check_password, MAX_PASSWORD, MIN_PASSWORD};

/// Name of the hotspot profile (NetworkManager connection id and file)
pub const HOTSPOT_ID: &str = "OpenRig-hotspot";
/// Address of the pedal on its hotspot
pub const HOTSPOT_ADDRESS: &str = "10.42.0.1";

/// What pedalboard.txt asks for
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChange {
    Set(String),
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// Empty: an open network
    pub password: String,
}

/// The secrets found in pedalboard.txt, and the file without them
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Consumed {
    pub text: String,
    pub password: Option<PasswordChange>,
    pub wifi: Option<WifiNetwork>,
}

impl Consumed {
    /// Something to do (and the file to rewrite)
    pub fn changed(&self) -> bool {
        self.password.is_some() || self.wifi.is_some()
    }
}

/// `key=value` of a line, the value without spaces around nor quotes
fn key_value(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let value = value.trim();
    let value = ['"', '\'']
        .iter()
        .find_map(|q| {
            value
                .strip_prefix(*q)
                .and_then(|v| v.strip_suffix(*q))
                .filter(|_| value.len() >= 2)
        })
        .unwrap_or(value);
    Some((key.trim(), value))
}

fn is_marker(value: &str) -> bool {
    value.starts_with('(')
}

/// Why a Wi-Fi network cannot be saved
fn check_wifi(ssid: &str, password: &str) -> Result<(), String> {
    if ssid.len() > 32 {
        return Err("the network name has more than 32 bytes".into());
    }
    if ssid.chars().any(char::is_control) {
        return Err("the network name has control characters".into());
    }
    let hex = password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit());
    if !password.is_empty() && !hex && !(8..=63).contains(&password.chars().count()) {
        return Err("the password needs 8 to 63 characters".into());
    }
    if password.chars().any(char::is_control) {
        return Err("the password has control characters".into());
    }
    Ok(())
}

/// Read the secrets of pedalboard.txt (`text`): what to do, and the text to
/// write back. The last line of a key wins, as when the file is read.
pub fn consume(text: &str) -> Consumed {
    let lines: Vec<&str> = text.lines().collect();
    let last = |key: &str| {
        lines
            .iter()
            .rposition(|l| key_value(l).is_some_and(|(k, _)| k == key))
    };
    let mut replace: Vec<(usize, String)> = Vec::new();
    let mut result = Consumed::default();

    if let Some(i) = last("remote_password") {
        let (_, value) = key_value(lines[i]).unwrap_or_default();
        if value.eq_ignore_ascii_case("off") {
            result.password = Some(PasswordChange::Off);
            replace.push((i, "remote_password=(off)".into()));
        } else if !value.is_empty() && !is_marker(value) {
            match check_password(value) {
                Ok(()) => {
                    result.password = Some(PasswordChange::Set(value.to_string()));
                    replace.push((i, "remote_password=(saved)".into()));
                }
                Err(_) => replace.push((
                    i,
                    format!(
                        "remote_password=(not saved: use {MIN_PASSWORD} to {MAX_PASSWORD} \
                         letters, digits, spaces or symbols of a US keyboard)"
                    ),
                )),
            }
        }
    }

    let ssid_line = last("wifi_ssid");
    let password_line = last("wifi_password");
    let ssid = ssid_line
        .and_then(|i| key_value(lines[i]))
        .map(|(_, v)| v)
        .unwrap_or_default();
    let password = password_line
        .and_then(|i| key_value(lines[i]))
        .map(|(_, v)| v)
        .unwrap_or_default();
    if let Some(i) = ssid_line.filter(|_| !ssid.is_empty() && !is_marker(ssid)) {
        let error = if is_marker(password) {
            Err("set wifi_password again, or leave it empty for an open network".to_string())
        } else {
            check_wifi(ssid, password)
        };
        match error {
            Ok(()) => {
                result.wifi = Some(WifiNetwork {
                    ssid: ssid.to_string(),
                    password: password.to_string(),
                });
                replace.push((i, format!("wifi_ssid=(saved) {ssid}")));
                if let Some(p) = password_line {
                    replace.push((p, "wifi_password=(saved)".into()));
                }
            }
            Err(why) => {
                replace.push((i, format!("wifi_ssid=(not saved: {why}) {ssid}")));
                if let Some(p) = password_line.filter(|_| !password.is_empty()) {
                    // Never left in the file, used or not
                    replace.push((p, "wifi_password=".into()));
                }
            }
        }
    } else if let Some(p) = password_line.filter(|_| !password.is_empty() && !is_marker(password)) {
        // A password without a network: removed all the same
        replace.push((p, "wifi_password=(not saved: set wifi_ssid too)".into()));
    }

    // Lines of a secret before the last one are removed too
    let secret = |l: &str| {
        key_value(l)
            .is_some_and(|(k, _)| matches!(k, "remote_password" | "wifi_password" | "wifi_ssid"))
    };
    let mut out = String::with_capacity(text.len());
    for (n, line) in lines.iter().enumerate() {
        if let Some((_, new)) = replace.iter().find(|(i, _)| *i == n) {
            out.push_str(new);
        } else if secret(line) && Some(n) != last_of(&lines, line) {
            continue;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !replace.is_empty() || out.trim_end() != text.trim_end() {
        result.text = out;
    } else {
        result.text = text.to_string();
    }
    result
}

/// Index of the last line with the same key as `line`
fn last_of(lines: &[&str], line: &str) -> Option<usize> {
    let key = key_value(line)?.0;
    lines
        .iter()
        .rposition(|l| key_value(l).is_some_and(|(k, _)| k == key))
}

/// Value of a NetworkManager keyfile: escapes as GKeyFile reads them
fn keyfile_value(value: &str) -> String {
    let mut out = String::new();
    for (i, c) in value.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' ' if i == 0 => out.push_str("\\s"),
            c => out.push(c),
        }
    }
    if out.ends_with(' ') {
        out.pop();
        out.push_str("\\s");
    }
    out
}

fn uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The hotspot profile: the pedal's own Wi-Fi (WPA2, 10.42.0.1), brought up
/// by the hotspot service. The network name is set by that service from
/// the Wi-Fi adapter's address.
pub fn hotspot_keyfile(password: &str) -> String {
    format!(
        "[connection]\n\
         id={HOTSPOT_ID}\n\
         uuid={}\n\
         type=wifi\n\
         autoconnect=false\n\
         \n\
         [wifi]\n\
         mode=ap\n\
         band=bg\n\
         ssid=OpenRig\n\
         \n\
         [wifi-security]\n\
         key-mgmt=wpa-psk\n\
         proto=rsn\n\
         pairwise=ccmp\n\
         group=ccmp\n\
         pmf=1\n\
         psk={}\n\
         \n\
         [ipv4]\n\
         method=shared\n\
         address1={HOTSPOT_ADDRESS}/24\n\
         \n\
         [ipv6]\n\
         method=disabled\n",
        uuid(),
        keyfile_value(password)
    )
}

/// A Wi-Fi network to join, as `nmcli device wifi connect` saves it
pub fn wifi_keyfile(network: &WifiNetwork) -> String {
    let security = if network.password.is_empty() {
        String::new()
    } else {
        format!(
            "\n[wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n",
            keyfile_value(&network.password)
        )
    };
    format!(
        "[connection]\n\
         id={id}\n\
         uuid={}\n\
         type=wifi\n\
         autoconnect=true\n\
         \n\
         [wifi]\n\
         mode=infrastructure\n\
         ssid={id}\n\
         {security}\n\
         [ipv4]\n\
         method=auto\n\
         \n\
         [ipv6]\n\
         method=auto\n",
        uuid(),
        id = keyfile_value(&network.ssid),
    )
}

/// File name of a profile: the id, without what a file name cannot hold
fn profile_file(id: &str) -> String {
    let name: String = id
        .chars()
        .map(|c| if c == '/' || c.is_control() { '_' } else { c })
        .collect();
    let name = name.trim_start_matches('.');
    format!(
        "{}.nmconnection",
        if name.is_empty() { "wifi" } else { name }
    )
}

/// Profiles of `dir` with this connection id
fn profiles_with_id(dir: &Path, id: &str) -> Vec<std::path::PathBuf> {
    let wanted = format!("id={}", keyfile_value(id));
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l.trim() == wanted)))
        .collect()
}

/// Where `remote-setup` reads and writes
pub struct Paths<'a> {
    /// pedalboard.txt
    pub config: &'a Path,
    /// The app's data directory (the password hash)
    pub data_dir: &'a Path,
    /// NetworkManager's profiles
    pub connections: &'a Path,
}

/// What `remote-setup` did, for the boot log (never a secret)
pub fn apply(paths: &Paths, consumed: &Consumed) -> Result<Vec<String>> {
    let mut done = Vec::new();
    match &consumed.password {
        Some(PasswordChange::Set(password)) => {
            let hash = super::auth::hash_password(password)?;
            super::auth::write_hash(&paths.data_dir.join(super::auth::PASSWORD_FILE), &hash)?;
            write_profile(paths.connections, HOTSPOT_ID, &hotspot_keyfile(password))?;
            done.push("remote control password saved".to_string());
        }
        Some(PasswordChange::Off) => {
            let _ = std::fs::remove_file(paths.data_dir.join(super::auth::PASSWORD_FILE));
            for p in profiles_with_id(paths.connections, HOTSPOT_ID) {
                let _ = std::fs::remove_file(p);
            }
            done.push("remote control turned off".to_string());
        }
        None => {}
    }
    if let Some(network) = &consumed.wifi {
        write_profile(paths.connections, &network.ssid, &wifi_keyfile(network))?;
        done.push(format!("Wi-Fi network {:?} saved", network.ssid));
    }
    Ok(done)
}

/// Write a profile (root only), replacing those with the same id
fn write_profile(dir: &Path, id: &str, contents: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("{}", dir.display()))?;
    let path = dir.join(profile_file(id));
    for old in profiles_with_id(dir, id) {
        if old != path {
            let _ = std::fs::remove_file(old);
        }
    }
    crate::persist::write_private(&path, contents.as_bytes())
}

/// `pedalboard remote-setup <pedalboard.txt> <data dir> <connections dir>`:
/// consume the secrets, then write pedalboard.txt back without them (to a
/// new file, synced, renamed over the old one). The caller makes the boot
/// partition writable when the exit status is 2 ("rewrite needed") and runs
/// it again; see S00storage.
pub fn run(paths: &Paths, write_back: bool) -> Result<bool> {
    let text = match std::fs::read(paths.config) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).replace('\r', ""),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("pedalboard.txt"),
    };
    let consumed = consume(&text);
    if consumed.text == text {
        return Ok(false);
    }
    if !write_back {
        return Ok(true);
    }
    for line in apply(paths, &consumed)? {
        println!("remote-setup: {line}");
    }
    crate::persist::write_atomic(paths.config, consumed.text.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# Settings\naudio_device=auto\n\n\
                        # Remote\nremote_password=\nremote_hotspot=auto\n\
                        wifi_ssid=\nwifi_password=\n";

    fn with(edits: &[(&str, &str)]) -> String {
        let mut text = FILE.to_string();
        for (key, value) in edits {
            text = text.replace(&format!("{key}=\n"), &format!("{key}={value}\n"));
        }
        text
    }

    #[test]
    fn nothing_to_do() {
        let c = consume(FILE);
        assert!(!c.changed());
        assert_eq!(c.text, FILE);
        let saved = with(&[
            ("remote_password", "(saved)"),
            ("wifi_ssid", "(saved) Home"),
            ("wifi_password", "(saved)"),
        ]);
        let c = consume(&saved);
        assert!(!c.changed());
        assert_eq!(c.text, saved);
    }

    #[test]
    fn password_is_consumed() {
        let c = consume(&with(&[("remote_password", "\"stage secret 1\"")]));
        assert_eq!(
            c.password,
            Some(PasswordChange::Set("stage secret 1".into()))
        );
        assert!(c.text.contains("remote_password=(saved)\n"));
        assert!(!c.text.contains("stage secret"));
        // Everything else kept
        assert!(c
            .text
            .starts_with("# Settings\naudio_device=auto\n\n# Remote\n"));
        assert!(c.text.contains("remote_hotspot=auto\n"));
    }

    #[test]
    fn short_password_is_refused_and_removed() {
        let c = consume(&with(&[("remote_password", "1234")]));
        assert_eq!(c.password, None);
        assert!(c.text.contains("remote_password=(not saved: use 8 to 63"));
        assert!(!c.text.contains("1234"));
    }

    #[test]
    fn password_off() {
        let c = consume(&with(&[("remote_password", "OFF")]));
        assert_eq!(c.password, Some(PasswordChange::Off));
        assert!(c.text.contains("remote_password=(off)\n"));
        assert!(!consume(&c.text).changed());
    }

    #[test]
    fn earlier_lines_of_a_secret_go_too() {
        let text = "remote_password=first secret\nremote_password=second secret\n";
        let c = consume(text);
        assert_eq!(
            c.password,
            Some(PasswordChange::Set("second secret".into()))
        );
        assert_eq!(c.text, "remote_password=(saved)\n");
    }

    #[test]
    fn wifi_is_consumed() {
        let c = consume(&with(&[
            ("wifi_ssid", "My Home"),
            ("wifi_password", "hunter2hunter2"),
        ]));
        assert_eq!(
            c.wifi,
            Some(WifiNetwork {
                ssid: "My Home".into(),
                password: "hunter2hunter2".into()
            })
        );
        assert!(c.text.contains("wifi_ssid=(saved) My Home\n"));
        assert!(c.text.contains("wifi_password=(saved)\n"));
        assert!(!c.text.contains("hunter2"));
        assert!(!consume(&c.text).changed());

        let open = consume(&with(&[("wifi_ssid", "Cafe")]));
        assert_eq!(open.wifi.map(|w| w.password), Some(String::new()));
    }

    #[test]
    fn bad_wifi_password_is_removed() {
        let c = consume(&with(&[("wifi_ssid", "Home"), ("wifi_password", "short")]));
        assert_eq!(c.wifi, None);
        assert!(c
            .text
            .contains("wifi_ssid=(not saved: the password needs 8 to 63"));
        assert!(c.text.contains("wifi_password=\n"));
        assert!(!c.text.contains("short\n"));

        let alone = consume(&with(&[("wifi_password", "lonely password")]));
        assert_eq!(alone.wifi, None);
        assert!(!alone.text.contains("lonely"));

        let new_name = consume(&with(&[
            ("wifi_ssid", "Other"),
            ("wifi_password", "(saved)"),
        ]));
        assert_eq!(new_name.wifi, None);
        assert!(new_name
            .text
            .contains("wifi_ssid=(not saved: set wifi_password again"));
    }

    #[test]
    fn keyfiles() {
        let hotspot = hotspot_keyfile(" pass\\word ");
        assert!(hotspot.contains("mode=ap\n"));
        assert!(hotspot.contains("method=shared\naddress1=10.42.0.1/24\n"));
        assert!(hotspot.contains("psk=\\spass\\\\word\\s\n"));
        assert!(hotspot.contains("autoconnect=false\n"));
        let wifi = wifi_keyfile(&WifiNetwork {
            ssid: "Home".into(),
            password: "12345678".into(),
        });
        assert!(wifi.contains("id=Home\n"));
        assert!(wifi.contains("ssid=Home\n"));
        assert!(wifi.contains("psk=12345678\n"));
        let open = wifi_keyfile(&WifiNetwork {
            ssid: "Cafe".into(),
            password: String::new(),
        });
        assert!(!open.contains("wifi-security"));
        assert_eq!(profile_file("a/b"), "a_b.nmconnection");
        assert_eq!(profile_file("..."), "wifi.nmconnection");
    }

    #[test]
    fn run_writes_files() {
        let dir = std::env::temp_dir().join(format!("openrig-bootcfg-{}", std::process::id()));
        let connections = dir.join("connections");
        let _ = std::fs::create_dir_all(&connections);
        let config = dir.join("pedalboard.txt");
        let _ = std::fs::write(
            &config,
            with(&[
                ("remote_password", "my stage pass"),
                ("wifi_ssid", "Home"),
                ("wifi_password", "home wifi pass"),
            ]),
        );
        // An older profile of the same network is replaced
        let _ = std::fs::write(
            connections.join("Home-1.nmconnection"),
            "[connection]\nid=Home\n",
        );
        let paths = Paths {
            config: &config,
            data_dir: &dir,
            connections: &connections,
        };
        assert!(run(&paths, false).unwrap_or(false), "a rewrite is needed");
        assert!(run(&paths, true).unwrap_or(false));
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        assert!(
            !text.contains("stage pass") && !text.contains("wifi pass"),
            "{text}"
        );
        assert!(super::super::auth::PasswordStore::open(&dir).verify("my stage pass"));
        let hotspot = std::fs::read_to_string(connections.join("OpenRig-hotspot.nmconnection"))
            .unwrap_or_default();
        assert!(hotspot.contains("psk=my stage pass\n"));
        assert!(connections.join("Home.nmconnection").exists());
        assert!(!connections.join("Home-1.nmconnection").exists());
        assert!(!run(&paths, true).unwrap_or(true), "nothing left to do");

        let _ = std::fs::write(&config, "remote_password=off\n");
        assert!(run(&paths, true).unwrap_or(false));
        assert!(!super::super::auth::PasswordStore::open(&dir).is_set());
        assert!(!connections.join("OpenRig-hotspot.nmconnection").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
