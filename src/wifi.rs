//! Wi-Fi from the settings page. The pedal has no desktop, so the app lists,
//! joins and forgets networks itself, through NetworkManager (`nmcli`, as on
//! Raspberry Pi OS). Where NetworkManager is missing (macOS, Windows, other
//! Linux setups) Wi-Fi is left to the system and the page says so.
//!
//! Every call runs `nmcli` and may take seconds (a scan, joining a network):
//! call them off the UI thread.

use std::process::{Command, Output};

/// A network seen by the last scan
#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    pub ssid: String,
    /// 0..=100
    pub signal: u8,
    pub secured: bool,
    /// The device is on this network
    pub connected: bool,
    /// A profile is saved: joining needs no password
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    /// Wi-Fi radio on
    pub enabled: bool,
    /// Network the device is on
    pub connected: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WifiError {
    #[error("Wi-Fi is managed by the system on this computer")]
    Unsupported,
    #[error("No Wi-Fi adapter found")]
    NoDevice,
    #[error("Wrong password")]
    WrongPassword,
    #[error("{0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, WifiError>;

/// An IPv4 address of a connected Ethernet or Wi-Fi device, to reach the
/// pedal over SSH or from a browser
#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    pub device: String,
    /// "Ethernet" or "Wi-Fi"
    pub kind: &'static str,
    /// Without the prefix length
    pub ip: String,
}

/// IPv4 addresses of the connected Ethernet and Wi-Fi devices, through
/// NetworkManager; empty where it is missing. Loopback, VPN and other
/// virtual devices are left out.
pub fn addresses() -> Vec<Address> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    nmcli(&[
        "-t",
        "-f",
        "GENERAL.DEVICE,GENERAL.TYPE,IP4.ADDRESS",
        "device",
        "show",
    ])
    .map(|out| parse_addresses(&out))
    .unwrap_or_default()
}

/// The Wi-Fi device, driven through NetworkManager
#[derive(Debug, Clone)]
pub struct Wifi {
    device: String,
}

impl Wifi {
    /// The Wi-Fi device, if NetworkManager runs and manages one
    pub fn detect() -> Result<Wifi> {
        if !cfg!(target_os = "linux") {
            return Err(WifiError::Unsupported);
        }
        let out = nmcli(&["-t", "-f", "DEVICE,TYPE", "device"]).map_err(|e| match e {
            // nmcli missing, or NetworkManager not running
            WifiError::Failed(_) => WifiError::Unsupported,
            e => e,
        })?;
        wifi_device(&out)
            .map(|device| Wifi { device })
            .ok_or(WifiError::NoDevice)
    }

    pub fn status(&self) -> Result<Status> {
        let enabled = nmcli(&["radio", "wifi"])?.trim() == "enabled";
        let devices = nmcli(&["-t", "-f", "DEVICE,STATE,CONNECTION", "device"])?;
        let connected = devices
            .lines()
            .map(split_terse)
            .find(|f| f.len() == 3 && f[0] == self.device && f[1] == "connected")
            .map(|f| f[2].clone());
        Ok(Status { enabled, connected })
    }

    pub fn set_enabled(&self, on: bool) -> Result<()> {
        nmcli(&["radio", "wifi", if on { "on" } else { "off" }]).map(drop)
    }

    /// Networks in range, the current one first, then by signal. A rescan
    /// takes a few seconds; without it NetworkManager answers from its cache.
    pub fn networks(&self, rescan: bool) -> Result<Vec<Network>> {
        let list = nmcli(&[
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "ifname",
            &self.device,
            "--rescan",
            if rescan { "yes" } else { "no" },
        ])?;
        let saved = nmcli(&["-t", "-f", "NAME,TYPE", "connection", "show"])?;
        Ok(parse_networks(&list, &saved_wifi(&saved)))
    }

    /// Join a network: a saved one needs no password
    pub fn connect(&self, ssid: &str, password: Option<&str>) -> Result<()> {
        let saved = saved_wifi(&nmcli(&["-t", "-f", "NAME,TYPE", "connection", "show"])?)
            .iter()
            .any(|n| n == ssid);
        let result = match password {
            None if saved => nmcli(&[
                "-w",
                "45",
                "connection",
                "up",
                "id",
                ssid,
                "ifname",
                &self.device,
            ]),
            // nmcli takes the password as an argument: other local users
            // could see it in the process list while it runs. The pedal is
            // a single-user device.
            _ => {
                let mut args = vec!["-w", "45", "device", "wifi", "connect", ssid];
                if let Some(password) = password {
                    args.extend(["password", password]);
                }
                args.extend(["ifname", &self.device]);
                nmcli(&args)
            }
        };
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                // A first attempt refused for its password leaves a profile
                // with the wrong one behind: remove it, the next try starts
                // clean (other failures, e.g. a timeout, keep it)
                if !saved && e == WifiError::WrongPassword {
                    let _ = self.forget(ssid);
                }
                Err(e)
            }
        }
    }

    pub fn disconnect(&self) -> Result<()> {
        nmcli(&["device", "disconnect", &self.device]).map(drop)
    }

    /// Delete the saved profile(s) of a network
    pub fn forget(&self, ssid: &str) -> Result<()> {
        let saved = saved_wifi(&nmcli(&["-t", "-f", "NAME,TYPE", "connection", "show"])?);
        for _ in saved.iter().filter(|n| *n == ssid) {
            nmcli(&["connection", "delete", "id", ssid])?;
        }
        Ok(())
    }
}

/// Run nmcli; its error message (in English) on failure
fn nmcli(args: &[&str]) -> Result<String> {
    let output = Command::new("nmcli")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| WifiError::Failed(format!("nmcli: {e}")))?;
    check(output)
}

fn check(output: Output) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(error(&String::from_utf8_lossy(&output.stderr)))
}

/// A readable error from nmcli's message
fn error(stderr: &str) -> WifiError {
    let message = stderr.trim();
    if message.contains("Secrets were required") || message.contains("802-1X supplicant") {
        return WifiError::WrongPassword;
    }
    // "Error: Connection activation failed: (7) Secrets were required…"
    let message = message.lines().next().unwrap_or("").trim();
    let message = message.strip_prefix("Error: ").unwrap_or(message);
    WifiError::Failed(if message.is_empty() {
        "Wi-Fi command failed".to_string()
    } else {
        message.to_string()
    })
}

/// Split a line of `nmcli -t` output: fields are separated by ':', with
/// '\:' and '\\' escaped
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    fields.last_mut().unwrap().push(next);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

fn wifi_device(devices: &str) -> Option<String> {
    devices
        .lines()
        .map(split_terse)
        .find(|f| f.len() == 2 && f[1] == "wifi")
        .map(|f| f[0].clone())
}

/// Names of the saved Wi-Fi profiles (`nmcli -t -f NAME,TYPE connection show`)
fn saved_wifi(connections: &str) -> Vec<String> {
    connections
        .lines()
        .map(split_terse)
        .filter(|f| f.len() == 2 && f[1] == "802-11-wireless")
        .map(|f| f[0].clone())
        .collect()
}

/// Networks from `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device wifi list`:
/// one entry per name (the strongest access point), hidden ones left out
fn parse_networks(list: &str, saved: &[String]) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for fields in list.lines().map(split_terse) {
        let [in_use, ssid, signal, security] = fields.as_slice() else {
            continue;
        };
        if ssid.is_empty() {
            continue;
        }
        let network = Network {
            ssid: ssid.clone(),
            signal: signal.parse::<u8>().unwrap_or(0).min(100),
            secured: !security.is_empty() && security != "--",
            connected: in_use == "*",
            saved: saved.contains(ssid),
        };
        match networks.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(known) => {
                known.connected |= network.connected;
                if network.signal > known.signal {
                    known.signal = network.signal;
                    known.secured = network.secured;
                }
            }
            None => networks.push(network),
        }
    }
    networks.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
    networks
}

/// Addresses from `nmcli -t -f GENERAL.DEVICE,GENERAL.TYPE,IP4.ADDRESS device
/// show`: one "KEY:value" line per field, a blank line between devices
fn parse_addresses(show: &str) -> Vec<Address> {
    let mut addresses = Vec::new();
    let mut device = "";
    let mut kind = None;
    for line in show.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "GENERAL.DEVICE" => {
                device = value;
                kind = None;
            }
            "GENERAL.TYPE" => {
                kind = match value {
                    "ethernet" => Some("Ethernet"),
                    "wifi" => Some("Wi-Fi"),
                    _ => None,
                }
            }
            // IP4.ADDRESS[1], IP4.ADDRESS[2]…
            key if key.starts_with("IP4.ADDRESS") => {
                let ip = value.split('/').next().unwrap_or("").trim();
                if let (Some(kind), false) = (kind, ip.is_empty()) {
                    addresses.push(Address {
                        device: device.to_string(),
                        kind,
                        ip: ip.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    addresses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_fields_are_unescaped() {
        assert_eq!(
            split_terse(r"*:Cafe\:Wifi:80:WPA2"),
            ["*", "Cafe:Wifi", "80", "WPA2"]
        );
        assert_eq!(split_terse(r"a\\b:"), [r"a\b", ""]);
    }

    #[test]
    fn finds_the_wifi_device() {
        let devices = "tailscale0:tun\nwlan0:wifi\n/net/connman/iwd/0:wifi-p2p\nlo:loopback\n";
        assert_eq!(wifi_device(devices).as_deref(), Some("wlan0"));
        assert_eq!(wifi_device("eth0:ethernet\n"), None);
    }

    #[test]
    fn networks_are_merged_and_sorted() {
        let list = " :Home:40:WPA2\n\
                    *:Home:70:WPA2\n\
                    \x20:Cafe:90:\n\
                    \x20::95:WPA2\n\
                    \x20:Studio:55:WPA1 WPA2\n";
        let saved = vec!["Studio".to_string()];
        let networks = parse_networks(list, &saved);
        let names: Vec<_> = networks.iter().map(|n| n.ssid.as_str()).collect();
        // Connected first, then by signal; the hidden network is left out
        assert_eq!(names, ["Home", "Cafe", "Studio"]);
        assert!(networks[0].connected && networks[0].signal == 70);
        assert!(!networks[1].secured);
        assert!(networks[2].saved && networks[2].secured);
    }

    #[test]
    fn addresses_of_ethernet_and_wifi_only() {
        let show = "GENERAL.DEVICE:eth0\n\
                    GENERAL.TYPE:ethernet\n\
                    IP4.ADDRESS[1]:192.168.1.50/24\n\
                    \n\
                    GENERAL.DEVICE:wlan0\n\
                    GENERAL.TYPE:wifi\n\
                    IP4.ADDRESS[1]:192.168.1.42/24\n\
                    IP4.ADDRESS[2]:10.0.0.7/8\n\
                    \n\
                    GENERAL.DEVICE:tailscale0\n\
                    GENERAL.TYPE:tun\n\
                    IP4.ADDRESS[1]:100.64.0.20/32\n\
                    \n\
                    GENERAL.DEVICE:lo\n\
                    GENERAL.TYPE:loopback\n\
                    IP4.ADDRESS[1]:127.0.0.1/8\n\
                    \n\
                    GENERAL.DEVICE:eth1\n\
                    GENERAL.TYPE:ethernet\n";
        let found: Vec<_> = parse_addresses(show)
            .into_iter()
            .map(|a| format!("{} {} {}", a.device, a.kind, a.ip))
            .collect();
        assert_eq!(
            found,
            [
                "eth0 Ethernet 192.168.1.50",
                "wlan0 Wi-Fi 192.168.1.42",
                "wlan0 Wi-Fi 10.0.0.7"
            ]
        );
    }

    #[test]
    fn errors_are_readable() {
        assert_eq!(
            error("Error: Connection activation failed: (7) Secrets were required, but not provided.\n"),
            WifiError::WrongPassword
        );
        assert_eq!(
            error("Error: No network with SSID 'Nope' found.\n"),
            WifiError::Failed("No network with SSID 'Nope' found.".to_string())
        );
    }
}
