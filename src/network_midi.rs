//! Network MIDI (RTP-MIDI / AppleMIDI) on the pedalboard OS: rtpmidid turns
//! each session a computer opens into an ALSA sequencer port, announced over
//! mDNS by avahi-daemon. RTP-MIDI has no authentication, so the app is the
//! gate: the MIDI of a session is only listened to once its device (remote
//! session name and address) is allowed, and never when it comes from
//! outside the local network.
//!
//! What lives here has no UI: the session name rules (the same as the root
//! helper's), the address scope, the devices allowed or blocked and their
//! file, the client of rtpmidid's control socket and the parser of its
//! `status` answer, and the `Gate` the MIDI thread asks before listening to
//! a port of rtpmidid.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::sync::MutexExt;

/// Control socket of rtpmidid (`/etc/rtpmidid/default.ini`)
pub const CONTROL_SOCKET: &str = "/run/rtpmidid/control.sock";
/// ALSA client name of rtpmidid (`alsa_name` in its configuration)
pub const ALSA_CLIENT: &str = "rtpmidid";
/// UDP port of the session (control; MIDI is the next one)
pub const PORT: u16 = 5004;
/// Longest session name
pub const NAME_MAX: usize = 32;
/// Devices remembered, allowed and blocked each: the oldest go first
pub const DEVICES_MAX: usize = 32;
/// Longest ALSA port name (`snd_seq_port_info` holds 64 bytes, NUL included)
const ALSA_NAME_MAX: usize = 63;
/// File of the devices allowed or blocked, in the data directory
pub const DEVICES_FILE: &str = "network-midi.json";

/// A session name the root helper takes: up to 32 letters, digits, spaces,
/// dots, dashes and underscores, no space first or last nor two in a row.
/// Empty means the host name.
pub fn valid_session_name(name: &str) -> bool {
    name.len() <= NAME_MAX
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && !name.contains("  ")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// An address of the local network: private IPv4 (RFC 1918), link-local
/// (169.254/16, fe80::/10) and unique local IPv6 (fc00::/7), IPv4 given as
/// IPv6 included. Loopback is not: rtpmidid only faces the network.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
            }
        },
    }
}

/// An address as shown and remembered: IPv4 given as IPv6 written as IPv4
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// A network session rtpmidid has: the remote end and its ALSA port
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Name the remote end gave its session (Reaper, a Mac's session…)
    pub name: String,
    /// Its address; `None` when rtpmidid gave none that parses
    pub ip: Option<IpAddr>,
    /// Its UDP control port
    pub udp_port: u16,
    /// Port number of the session in rtpmidid's ALSA client
    pub alsa_port: u8,
    /// Ids of the session's two peers in rtpmidid's router (ALSA, network)
    pub peers: [u64; 2],
}

impl Session {
    /// What the device is remembered by
    pub fn device(&self) -> Option<Device> {
        self.ip.map(|ip| Device::new(&self.name, ip))
    }
}

/// What rtpmidid reports
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Sessions announced over mDNS: name and port
    pub announced: Vec<(String, u16)>,
    /// Sessions computers opened, each with its ALSA port
    pub sessions: Vec<Session>,
}

/// The JSON request of `method` on the control socket, one line
pub fn request(id: u64, method: &str, params: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({ "id": id, "method": method, "params": params })
    )
}

/// The answer to `status` (rtpmidid 26.01, src/control_socket.cpp): the
/// router lists its peers; a session opened by a computer is a
/// `local_alsa_peer_t` (`port`: its ALSA port) and a
/// `network_rtpmidi_peer_t` (`peer.remote`: name, address, port) sending to
/// each other (`send_to`).
pub fn parse_status(answer: &str) -> Result<Status> {
    let json: serde_json::Value =
        serde_json::from_str(answer).context("rtpmidid answered no JSON")?;
    if let Some(error) = json.get("error") {
        return Err(anyhow!("rtpmidid: {error}"));
    }
    let result = json
        .get("result")
        .ok_or_else(|| anyhow!("rtpmidid answered no result"))?;
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let number = |v: &serde_json::Value, key: &str| v.get(key).and_then(|n| n.as_u64());

    let announced = result
        .pointer("/mdns/announcements")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let port = u16::try_from(number(a, "port")?).ok()?;
                    Some((text(a, "name"), port))
                })
                .collect()
        })
        .unwrap_or_default();

    let router: &[serde_json::Value] = result
        .get("router")
        .and_then(|r| r.as_array())
        .map_or(&[], |r| r.as_slice());
    let of_type = |kind: &'static str| {
        router
            .iter()
            .filter(move |p| p.get("type").and_then(|t| t.as_str()) == Some(kind))
    };
    let sends_to = |from: &serde_json::Value, to: u64| {
        from.get("send_to")
            .and_then(|s| s.as_array())
            .is_some_and(|s| s.iter().any(|id| id.as_u64() == Some(to)))
    };
    let mut sessions = Vec::new();
    for alsa in of_type("local_alsa_peer_t") {
        let (Some(alsa_id), Some(port)) = (number(alsa, "id"), number(alsa, "port")) else {
            continue;
        };
        let Ok(alsa_port) = u8::try_from(port) else {
            continue;
        };
        let network = of_type("network_rtpmidi_peer_t")
            .find(|n| number(n, "id").is_some_and(|id| sends_to(alsa, id)) && sends_to(n, alsa_id));
        let Some((network, network_id)) = network.and_then(|n| Some((n, number(n, "id")?))) else {
            continue;
        };
        let remote = network.pointer("/peer/remote");
        let name = remote
            .map(|r| text(r, "name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| text(network, "name"));
        let ip = remote
            .map(|r| text(r, "hostname"))
            .and_then(|h| h.parse::<IpAddr>().ok())
            .map(canonical);
        let udp_port = remote
            .and_then(|r| number(r, "port"))
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or_default();
        sessions.push(Session {
            name,
            ip,
            udp_port,
            alsa_port,
            peers: [alsa_id, network_id],
        });
    }
    Ok(Status {
        announced,
        sessions,
    })
}

/// Ask rtpmidid's control socket; returns its answer (one line)
#[cfg(unix)]
pub fn ask(socket: &Path, request: &str) -> Result<String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// Longest answer read (a status with many sessions is a few kB)
    const ANSWER_MAX: u64 = 1 << 20;

    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("Cannot reach rtpmidid at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream.take(ANSWER_MAX)).read_line(&mut answer)?;
    if !answer.ends_with('\n') {
        return Err(anyhow!("rtpmidid's answer was cut"));
    }
    Ok(answer)
}

/// What to do with the MIDI of a session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Listened to
    Allow,
    /// Ignored until the player allows or blocks the device
    Ask,
    /// Blocked by the player: dropped
    Block,
    /// Not from the local network (or no address): dropped
    Outside,
}

/// New devices: asked about, or allowed when on the local network
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Ask,
    AllowAll,
}

/// A device: the remote session's name and its address
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub address: IpAddr,
}

impl Device {
    pub fn new(name: &str, address: IpAddr) -> Self {
        Self {
            name: name.to_string(),
            address: canonical(address),
        }
    }
}

/// The devices allowed or blocked, and the mode for new ones
/// (`network-midi.json` in the data directory)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Devices {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub allowed: Vec<Device>,
    #[serde(default)]
    pub blocked: Vec<Device>,
}

impl Devices {
    /// The file in `dir`; none (or unreadable: logged by the caller) is
    /// "ask for every device"
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(crate::persist::load_json(&dir.join(DEVICES_FILE))?.unwrap_or_default())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        crate::persist::save_json(&dir.join(DEVICES_FILE), self)
    }

    /// What to do with the MIDI of `session`
    pub fn decide(&self, session: &Session) -> Decision {
        let Some(device) = session.device().filter(|d| is_local(d.address)) else {
            return Decision::Outside;
        };
        if self.blocked.contains(&device) {
            Decision::Block
        } else if self.allowed.contains(&device) || self.mode == Mode::AllowAll {
            Decision::Allow
        } else {
            Decision::Ask
        }
    }

    pub fn allow(&mut self, device: Device) {
        self.blocked.retain(|d| *d != device);
        remember(&mut self.allowed, device);
    }

    pub fn block(&mut self, device: Device) {
        self.allowed.retain(|d| *d != device);
        remember(&mut self.blocked, device);
    }

    /// Forget a device: asked about again
    pub fn forget(&mut self, device: &Device) {
        self.allowed.retain(|d| d != device);
        self.blocked.retain(|d| d != device);
    }
}

/// Add `device` last, dropping the oldest past `DEVICES_MAX`
fn remember(list: &mut Vec<Device>, device: Device) {
    list.retain(|d| *d != device);
    list.push(device);
    let extra = list.len().saturating_sub(DEVICES_MAX);
    list.drain(..extra);
}

/// A port name as ALSA keeps it (cut to 63 bytes)
fn alsa_name(name: &str) -> &str {
    if name.len() <= ALSA_NAME_MAX {
        return name;
    }
    let mut end = ALSA_NAME_MAX;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// The session name of an rtpmidid port, as midir names ALSA ports
/// (`rtpmidid:<port name> <client>:<port>`); `None` for another client
pub fn session_port_name(port_name: &str) -> Option<&str> {
    let rest = port_name.strip_prefix(ALSA_CLIENT)?.strip_prefix(':')?;
    Some(rest.rsplit_once(' ').map_or(rest, |(name, _)| name))
}

/// Ports of rtpmidid that are not sessions: its own announcement port, and
/// the export port of older configurations
pub fn internal_port(session: &str) -> bool {
    matches!(session, "Announcements" | "Network" | "Network Export")
}

/// Which ports of rtpmidid the MIDI thread listens to: the sessions of
/// allowed devices, by ALSA port number and name (a port number freed by
/// a session and taken by another one between two updates is not
/// mistaken for the allowed session unless the names match too)
#[derive(Debug, Default)]
pub struct Gate {
    allowed: Mutex<BTreeSet<(u8, String)>>,
}

impl Gate {
    /// The sessions listened to from now on
    pub fn set_allowed(&self, sessions: &[&Session]) {
        *self.allowed.locked() = sessions
            .iter()
            .map(|s| (s.alsa_port, alsa_name(&s.name).to_string()))
            .collect();
    }

    /// Whether the port `id` ("client:port") named `port_name` by midir is
    /// listened to. Ports of other ALSA clients are not the gate's business
    /// (true); among rtpmidid's, only allowed sessions are.
    pub fn admits(&self, id: &str, port_name: &str) -> bool {
        let Some(session) = session_port_name(port_name) else {
            return true;
        };
        let Some(port) = id
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u8>().ok())
        else {
            return false;
        };
        self.allowed.locked().contains(&(port, session.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// `status` as rtpmidid 26.01 (with this repository's patches) answered
    /// it, captured as is: one session opened by another rtpmidid at
    /// 192.168.3.37, from its ALSA port "Midi Through Port-0"
    const STATUS: &str = include_str!("../tests/fixtures/rtpmidid-status.json");

    fn session(name: &str, ip: &str) -> Session {
        Session {
            name: name.into(),
            ip: ip.parse().ok(),
            udp_port: 5004,
            alsa_port: 1,
            peers: [3, 4],
        }
    }

    #[test]
    fn session_names() {
        assert!(valid_session_name(""));
        assert!(valid_session_name("pedalboard"));
        assert!(valid_session_name("OpenRig Live-2_b.v"));
        assert!(valid_session_name(&"a".repeat(32)));
        assert!(!valid_session_name(&"a".repeat(33)));
        for bad in [
            " lead",
            "lead ",
            "two  spaces",
            "quote\"",
            "new\nline",
            "semi;colon",
            "{{hostname}}",
            "dollar$",
            "slash/",
            "é",
            "tab\t",
        ] {
            assert!(!valid_session_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn local_addresses() {
        for local in [
            "10.0.0.1",
            "172.16.5.4",
            "172.31.255.255",
            "192.168.1.20",
            "169.254.3.4",
            "fe80::1",
            "fd12:3456::1",
            "fc00::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(is_local(local.parse().unwrap()), "{local}");
        }
        for outside in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.251",
            "2001:db8::1",
            "2a01:e0a::1",
            "::1",
            "::",
            "ff02::fb",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_local(outside.parse().unwrap()), "{outside}");
        }
        assert_eq!(
            canonical("::ffff:192.168.1.20".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))
        );
        assert_eq!(
            canonical(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
    }

    #[test]
    fn parses_the_status_of_rtpmidid() {
        let status = parse_status(STATUS).unwrap();
        assert_eq!(status.announced, [("pedalboard".to_string(), 5004)]);
        assert_eq!(
            status.sessions,
            [Session {
                name: "Midi Through-Midi Through Port-0".into(),
                ip: Some("192.168.3.37".parse().unwrap()),
                udp_port: 32924,
                alsa_port: 1,
                peers: [2, 3],
            }]
        );
    }

    #[test]
    fn status_errors() {
        assert!(parse_status("not json").is_err());
        assert!(parse_status(r#"{"id":1,"error":"Unknown method 'x'"}"#).is_err());
        assert!(parse_status(r#"{"id":1}"#).is_err());
        assert_eq!(
            parse_status(r#"{"id":1,"result":{"router":[]}}"#).unwrap(),
            Status::default()
        );
        // A session whose network peer is gone (removed) is not listed
        let alone = r#"{"id":1,"result":{"router":[{"id":2,"name":"x","port":1,"send_to":[3],"type":"local_alsa_peer_t"}]}}"#;
        assert!(parse_status(alone).unwrap().sessions.is_empty());
    }

    #[test]
    fn requests_are_one_line_of_json() {
        let line = request(7, "router.remove", serde_json::json!([3]));
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json["method"], "router.remove");
        assert_eq!(json["params"][0], 3);
        assert_eq!(json["id"], 7);
    }

    #[test]
    fn devices_are_asked_about_then_remembered() {
        let mut devices = Devices::default();
        let reaper = session("Reaper", "192.168.1.20");
        assert_eq!(devices.decide(&reaper), Decision::Ask);
        devices.allow(reaper.device().unwrap());
        assert_eq!(devices.decide(&reaper), Decision::Allow);
        // Same name, another address: asked again
        assert_eq!(
            devices.decide(&session("Reaper", "192.168.1.66")),
            Decision::Ask
        );
        // IPv4 given as IPv6 is the same device
        assert_eq!(
            devices.decide(&session("Reaper", "::ffff:192.168.1.20")),
            Decision::Allow
        );
        devices.block(reaper.device().unwrap());
        assert_eq!(devices.decide(&reaper), Decision::Block);
        assert!(devices.allowed.is_empty());
        devices.forget(&reaper.device().unwrap());
        assert_eq!(devices.decide(&reaper), Decision::Ask);
    }

    #[test]
    fn outside_the_local_network_is_never_listened_to() {
        let mut devices = Devices {
            mode: Mode::AllowAll,
            ..Default::default()
        };
        let far = session("Reaper", "203.0.113.5");
        assert_eq!(devices.decide(&far), Decision::Outside);
        devices.allow(far.device().unwrap());
        assert_eq!(devices.decide(&far), Decision::Outside);
        assert_eq!(devices.decide(&session("Reaper", "")), Decision::Outside);
        assert_eq!(
            devices.decide(&session("Reaper", "192.168.1.20")),
            Decision::Allow
        );
        // Blocked stays blocked when anyone is allowed
        devices.block(Device::new("Reaper", "192.168.1.20".parse().unwrap()));
        assert_eq!(
            devices.decide(&session("Reaper", "192.168.1.20")),
            Decision::Block
        );
    }

    #[test]
    fn device_lists_are_bounded() {
        let mut devices = Devices::default();
        for i in 0..40u8 {
            devices.block(Device::new(
                &format!("spam {i}"),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)),
            ));
        }
        assert_eq!(devices.blocked.len(), DEVICES_MAX);
        assert_eq!(devices.blocked[0].name, "spam 8");
    }

    #[test]
    fn devices_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("pb-netmidi-{}", uuid::Uuid::new_v4()));
        assert_eq!(Devices::load(&dir).unwrap(), Devices::default());
        let mut devices = Devices {
            mode: Mode::AllowAll,
            ..Default::default()
        };
        devices.allow(Device::new("Reaper", "192.168.1.20".parse().unwrap()));
        devices.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(DEVICES_FILE)).unwrap();
        assert!(text.contains("\"allow-all\"") && text.contains("\"192.168.1.20\""));
        assert_eq!(Devices::load(&dir).unwrap(), devices);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_gate_admits_allowed_sessions_only() {
        let gate = Gate::default();
        let reaper = Session {
            alsa_port: 2,
            ..session("Reaper", "192.168.1.20")
        };
        // Other clients are not the gate's business
        assert!(gate.admits("20:0", "AudioBox USB 96:AudioBox USB 96 MIDI 1 20:0"));
        assert!(!gate.admits("128:2", "rtpmidid:Reaper 128:2"));
        gate.set_allowed(&[&reaper]);
        assert!(gate.admits("128:2", "rtpmidid:Reaper 128:2"));
        // Same port number, another session: not until it is allowed
        assert!(!gate.admits("128:2", "rtpmidid:Intruder 128:2"));
        assert!(!gate.admits("128:3", "rtpmidid:Reaper 128:3"));
        assert!(!gate.admits("128:0", "rtpmidid:Announcements 128:0"));
        gate.set_allowed(&[]);
        assert!(!gate.admits("128:2", "rtpmidid:Reaper 128:2"));
        // Names with colons and spaces, long names cut by ALSA
        let long = Session {
            name: format!("Studio: {}", "x".repeat(80)),
            ..reaper.clone()
        };
        gate.set_allowed(&[&long]);
        let port = format!("rtpmidid:{} 128:2", alsa_name(&long.name));
        assert!(gate.admits("128:2", &port));
    }

    #[test]
    fn rtpmidid_port_names() {
        assert_eq!(session_port_name("rtpmidid:Reaper 128:2"), Some("Reaper"));
        assert_eq!(
            session_port_name("rtpmidid:My Mac: Session 1 128:3"),
            Some("My Mac: Session 1")
        );
        assert_eq!(session_port_name("rtpmidid2:Reaper 128:2"), None);
        assert_eq!(session_port_name("Midi Through:Port-0 14:0"), None);
        assert!(internal_port("Announcements"));
        assert!(!internal_port("Reaper"));
        assert_eq!(alsa_name(&"é".repeat(40)).len(), 62);
    }
}
