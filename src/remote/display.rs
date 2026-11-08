//! Screen or remote: whether the app draws on a touchscreen, or in memory
//! for the phone or tablet used as the pedal's screen.
//!
//! `display=` in pedalboard.txt (PEDALBOARD_DISPLAY for the app):
//!
//! - `screen`: the display, as without the remote;
//! - `remote`: the phone or tablet, whatever is plugged in;
//! - `auto` (default): the display when there is one with a touch device
//!   (a monitor without touch cannot be used), else the remote. Only when
//!   the remote is set up: without a password, the app runs as before.

use std::path::Path;
use std::time::{Duration, Instant};

/// How long a display without touch waits for its touch device (a USB or
/// DSI touch driver loads late)
pub const TOUCH_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Screen,
    Remote,
}

impl Mode {
    pub fn parse(value: &str) -> Mode {
        match value.trim().to_ascii_lowercase().as_str() {
            "screen" => Mode::Screen,
            "remote" => Mode::Remote,
            _ => Mode::Auto,
        }
    }

    pub fn from_env() -> Mode {
        Mode::parse(&std::env::var("PEDALBOARD_DISPLAY").unwrap_or_default())
    }
}

/// What was found, for the log
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Devices {
    /// Display connectors seen connected ("HDMI-A-1", "DSI-1")
    pub displays: Vec<String>,
    /// Touch devices ("ft5x06", "ILITEK Multi-Touch")
    pub touch: Vec<String>,
}

/// Run headless (UI streamed to the remote) or on the display
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub headless: bool,
    pub reason: String,
}

/// The decision for `mode`. `kms`: the app draws straight on the display
/// (the OS image), else it is a desktop window. `devices` is only called
/// when it matters (auto, on the OS image, with the remote set up).
pub fn decide(
    mode: Mode,
    kms: bool,
    remote_on: bool,
    devices: impl FnOnce() -> Devices,
) -> Decision {
    let decision = |headless: bool, reason: &str| Decision {
        headless,
        reason: reason.to_string(),
    };
    match mode {
        Mode::Screen => decision(false, "display=screen"),
        Mode::Remote => decision(true, "display=remote"),
        Mode::Auto if !kms => decision(false, "desktop window"),
        Mode::Auto if !remote_on => decision(false, "remote control off"),
        Mode::Auto => {
            let found = devices();
            let seen = format!(
                "displays: [{}], touch: [{}]",
                found.displays.join(", "),
                found.touch.join(", ")
            );
            if found.displays.is_empty() {
                decision(true, &format!("no display connected ({seen})"))
            } else if found.touch.is_empty() {
                decision(true, &format!("display without touch ({seen})"))
            } else {
                decision(false, &format!("touchscreen ({seen})"))
            }
        }
    }
}

/// Connected display connectors, from DRM (`/sys/class/drm/card0-HDMI-A-1/
/// status`). "unknown" counts as connected (DSI panels report it).
pub fn connected_displays(drm: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(drm)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // card0-HDMI-A-1: a connector of card0
            let connector = name.strip_prefix("card")?.split_once('-')?.1.to_string();
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            matches!(status.trim(), "connected" | "unknown").then_some(connector)
        })
        .collect();
    found.sort();
    found
}

/// Bit `bit` of a `B: KEY=` / `B: ABS=` / `B: PROP=` line: hexadecimal
/// words, the most significant first (each a `long` of the kernel)
fn has_bit(words: &str, bit: usize) -> bool {
    let words: Vec<&str> = words.split_whitespace().collect();
    // A long of the kernel: the app is built for the system it runs on
    let size = usize::BITS as usize;
    let (word, offset) = (bit / size, bit % size);
    let Some(text) = words.len().checked_sub(word + 1).and_then(|i| words.get(i)) else {
        return false;
    };
    u64::from_str_radix(text, 16).is_ok_and(|v| v >> offset & 1 == 1)
}

const INPUT_PROP_DIRECT: usize = 0x01;
const BTN_TOUCH: usize = 0x14a;
const ABS_X: usize = 0x00;
const ABS_MT_POSITION_X: usize = 0x35;

/// Touchscreens of `/proc/bus/input/devices`: direct input devices (on the
/// screen, not a touchpad) reporting touches and positions
pub fn touchscreens(devices: &str) -> Vec<String> {
    devices
        .split("\n\n")
        .filter_map(|block| {
            let field = |prefix: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(prefix))
                    .map(str::trim)
                    .unwrap_or_default()
            };
            let name = field("N: Name=").trim_matches('"').to_string();
            let prop = field("B: PROP=");
            let key = field("B: KEY=");
            let abs = field("B: ABS=");
            let touch = has_bit(prop, INPUT_PROP_DIRECT)
                && has_bit(key, BTN_TOUCH)
                && (has_bit(abs, ABS_MT_POSITION_X) || has_bit(abs, ABS_X));
            touch.then_some(name)
        })
        .collect()
}

/// What is plugged in now
pub fn devices_now() -> Devices {
    Devices {
        displays: connected_displays(Path::new("/sys/class/drm")),
        touch: touchscreens(
            &std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default(),
        ),
    }
}

/// What is plugged in, waiting up to `wait` for a touch device when a
/// display is connected without one
pub fn devices_waiting(wait: Duration) -> Devices {
    let start = Instant::now();
    loop {
        let found = devices_now();
        if found.displays.is_empty() || !found.touch.is_empty() || start.elapsed() >= wait {
            return found;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(displays: &[&str], touch: &[&str]) -> Devices {
        Devices {
            displays: displays.iter().map(|s| s.to_string()).collect(),
            touch: touch.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::parse("screen"), Mode::Screen);
        assert_eq!(Mode::parse(" Remote "), Mode::Remote);
        assert_eq!(Mode::parse(""), Mode::Auto);
        assert_eq!(Mode::parse("bogus"), Mode::Auto);
    }

    #[test]
    fn remote_off_keeps_the_display() {
        let never = || -> Devices { panic!("devices must not be looked at") };
        assert!(!decide(Mode::Auto, true, false, never).headless);
        assert!(!decide(Mode::Auto, false, true, never).headless);
        assert!(!decide(Mode::Screen, true, true, never).headless);
        assert!(decide(Mode::Remote, true, false, never).headless);
    }

    #[test]
    fn auto_with_the_remote() {
        let d = decide(Mode::Auto, true, true, || devices(&[], &[]));
        assert!(d.headless);
        assert!(d.reason.contains("no display"));
        let d = decide(Mode::Auto, true, true, || devices(&["HDMI-A-1"], &[]));
        assert!(d.headless);
        assert!(d.reason.contains("without touch"));
        assert!(d.reason.contains("HDMI-A-1"));
        let d = decide(Mode::Auto, true, true, || devices(&["DSI-1"], &["ft5x06"]));
        assert!(!d.headless);
    }

    const PROC: &str = r#"I: Bus=0018 Vendor=0000 Product=0000 Version=0000
N: Name="generic ft5x06 (79)"
P: Phys=
H: Handlers=event0
B: PROP=2
B: EV=b
B: KEY=400 0 0 0 0 0
B: ABS=260800000000003

I: Bus=0003 Vendor=046d Product=c52b Version=0111
N: Name="Logitech USB Receiver Mouse"
H: Handlers=mouse0 event1
B: PROP=0
B: EV=17
B: KEY=ffff0000 0 0 0 0
B: REL=1943
B: MSC=10

I: Bus=0018 Vendor=06cb Product=7e7e Version=0100
N: Name="SYNA Touchpad"
B: PROP=5
B: EV=1b
B: KEY=e520 10000 0 0 0 0
B: ABS=660800011000003

I: Bus=0003 Vendor=0eef Product=0005 Version=0111
N: Name="QEMU Virtio Tablet"
B: PROP=0
B: EV=b
B: KEY=70000 0 0 0 0
B: ABS=3
"#;

    #[test]
    fn finds_touchscreens_only() {
        // 64-bit words: BTN_TOUCH (0x14a) is bit 10 of the 6th word
        if usize::BITS != 64 {
            return;
        }
        assert_eq!(touchscreens(PROC), vec!["generic ft5x06 (79)".to_string()]);
        assert!(touchscreens("").is_empty());
    }

    #[test]
    fn displays_from_drm() {
        let dir = std::env::temp_dir().join(format!("openrig-drm-{}", std::process::id()));
        for (name, status) in [
            ("card0-HDMI-A-1", "disconnected"),
            ("card0-HDMI-A-2", "connected"),
            ("card1-DSI-1", "unknown"),
        ] {
            let _ = std::fs::create_dir_all(dir.join(name));
            let _ = std::fs::write(dir.join(name).join("status"), format!("{status}\n"));
        }
        let _ = std::fs::create_dir_all(dir.join("card0"));
        assert_eq!(connected_displays(&dir), vec!["DSI-1", "HDMI-A-2"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(connected_displays(&dir).is_empty());
    }
}
