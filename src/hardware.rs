//! What is wired to the pedalboard: how many footswitches and on which
//! GPIO, and the rotary encoder, if any. Set in Settings → Hardware and
//! kept in `hardware.json` in the data directory (it belongs to the board,
//! not to a preset or a session).
//!
//! Without the file, the defaults come from the environment, then from the
//! reference build (4 footswitches on GPIO 17, 27, 22, 23; encoder on 5, 6,
//! push button on 13):
//! - `PEDALBOARD_FOOTSWITCH_PINS`: footswitch GPIOs, empty for none
//! - `PEDALBOARD_ENCODER_PINS`: A,B[,button], empty for no encoder
//! - `PEDALBOARD_ENCODER_STEPS`: quadrature steps per detent

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const FILE: &str = "hardware.json";
/// Footswitches the Live page and the keys 1–8 can drive
pub const MAX_FOOTSWITCHES: usize = 8;
/// GPIOs on the Raspberry Pi header (BCM numbering)
pub const MAX_PIN: u8 = 27;
/// Quadrature steps per detent an encoder can have
pub const ENCODER_STEPS: [u8; 3] = [1, 2, 4];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hardware {
    /// GPIO of each footswitch, in order: as many switches as pins
    pub footswitches: Vec<u8>,
    pub encoder: Option<Encoder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encoder {
    pub a: u8,
    pub b: u8,
    /// Push button, if the encoder has one
    pub button: Option<u8>,
    /// Quadrature steps per detent (1, 2 or 4)
    pub steps: u8,
}

impl Default for Hardware {
    fn default() -> Self {
        Self {
            footswitches: vec![17, 27, 22, 23],
            encoder: Some(Encoder::default()),
        }
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self {
            a: 5,
            b: 6,
            button: Some(13),
            steps: 4,
        }
    }
}

impl Hardware {
    /// The saved hardware, else the environment's, else the defaults
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        match std::fs::read(&path) {
            Ok(json) => match serde_json::from_slice::<Self>(&json)
                .map_err(anyhow::Error::from)
                .and_then(|h| h.validate().map(|()| h))
            {
                Ok(hardware) => return hardware,
                Err(e) => tracing::warn!("{}: {e:#}; using the defaults", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{}: {e}; using the defaults", path.display()),
        }
        Self::from_env(|name| std::env::var(name).ok())
    }

    fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let mut hardware = Self::default();
        let pins = |name: &str| -> Option<Vec<u8>> {
            let value = var(name)?;
            let pins: Result<Vec<u8>, _> = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::parse)
                .collect();
            pins.map_err(|_| tracing::warn!("{name}: invalid pins '{value}'"))
                .ok()
        };
        if let Some(pins) = pins("PEDALBOARD_FOOTSWITCH_PINS") {
            hardware.footswitches = pins;
        }
        if let Some(pins) = pins("PEDALBOARD_ENCODER_PINS") {
            hardware.encoder = match pins[..] {
                [] => None,
                [a, b] => Some(Encoder {
                    a,
                    b,
                    button: None,
                    ..Encoder::default()
                }),
                [a, b, button] => Some(Encoder {
                    a,
                    b,
                    button: Some(button),
                    ..Encoder::default()
                }),
                _ => {
                    tracing::warn!("PEDALBOARD_ENCODER_PINS needs A,B[,button]: no encoder");
                    None
                }
            };
        }
        if let (Some(encoder), Some(steps)) = (
            &mut hardware.encoder,
            var("PEDALBOARD_ENCODER_STEPS").and_then(|s| s.parse().ok()),
        ) {
            if ENCODER_STEPS.contains(&steps) {
                encoder.steps = steps;
            }
        }
        if let Err(e) = hardware.validate() {
            tracing::warn!("Hardware from the environment: {e:#}; using the defaults");
            return Self::default();
        }
        hardware
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.validate()?;
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(FILE);
        let tmp: PathBuf = data_dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("Cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("Cannot write {}", path.display()))
    }

    /// Every GPIO used, with what uses it
    pub fn pins(&self) -> Vec<(u8, String)> {
        let mut pins: Vec<(u8, String)> = self
            .footswitches
            .iter()
            .enumerate()
            .map(|(i, &pin)| (pin, format!("FS {}", i + 1)))
            .collect();
        if let Some(e) = &self.encoder {
            pins.push((e.a, "encoder A".into()));
            pins.push((e.b, "encoder B".into()));
            if let Some(button) = e.button {
                pins.push((button, "encoder button".into()));
            }
        }
        pins
    }

    /// A GPIO wired to two inputs, or that does not exist
    pub fn validate(&self) -> Result<()> {
        if self.footswitches.len() > MAX_FOOTSWITCHES {
            bail!("At most {MAX_FOOTSWITCHES} footswitches");
        }
        if let Some(e) = &self.encoder {
            if !ENCODER_STEPS.contains(&e.steps) {
                bail!("Encoder steps per detent: 1, 2 or 4");
            }
        }
        let pins = self.pins();
        for (i, (pin, user)) in pins.iter().enumerate() {
            if *pin > MAX_PIN {
                bail!("{user}: there is no GPIO {pin}");
            }
            if let Some((_, other)) = pins[..i].iter().find(|(p, _)| p == pin) {
                bail!("GPIO {pin} is used by both {other} and {user}");
            }
        }
        Ok(())
    }

    /// The inputs on a GPIO that a bus may use (a HAT, an audio board):
    /// "FS 2: GPIO 18 is also I2S"
    pub fn shared_pins(&self) -> Vec<String> {
        self.pins()
            .into_iter()
            .filter_map(|(pin, user)| Some(format!("{user}: GPIO {pin} is also {}", bus(pin)?)))
            .collect()
    }

    /// A free GPIO, from `from` up then down (for a new input)
    pub fn free_pin(&self, from: u8) -> Option<u8> {
        let used: Vec<u8> = self.pins().into_iter().map(|(p, _)| p).collect();
        let from = from.min(MAX_PIN);
        (from..=MAX_PIN)
            .chain((0..from).rev())
            .find(|p| !used.contains(p))
    }
}

/// What else may use a GPIO of the Raspberry Pi header
fn bus(pin: u8) -> Option<&'static str> {
    match pin {
        0 | 1 => Some("the HAT EEPROM (ID_SD/ID_SC)"),
        2 | 3 => Some("I2C"),
        14 | 15 => Some("the serial port"),
        18..=21 => Some("I2S (audio HATs)"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn defaults_and_environment() {
        assert_eq!(Hardware::from_env(env(&[])), Hardware::default());
        let h = Hardware::from_env(env(&[
            ("PEDALBOARD_FOOTSWITCH_PINS", "4, 5"),
            ("PEDALBOARD_ENCODER_PINS", ""),
        ]));
        assert_eq!(h.footswitches, vec![4, 5]);
        assert_eq!(h.encoder, None);
        let h = Hardware::from_env(env(&[
            ("PEDALBOARD_FOOTSWITCH_PINS", ""),
            ("PEDALBOARD_ENCODER_PINS", "20,21"),
            ("PEDALBOARD_ENCODER_STEPS", "2"),
        ]));
        assert!(h.footswitches.is_empty());
        assert_eq!(
            h.encoder,
            Some(Encoder {
                a: 20,
                b: 21,
                button: None,
                steps: 2
            })
        );
        // A pin used twice: the defaults
        let h = Hardware::from_env(env(&[("PEDALBOARD_FOOTSWITCH_PINS", "5")]));
        assert_eq!(h, Hardware::default());
    }

    #[test]
    fn validation() {
        let mut h = Hardware::default();
        assert!(h.validate().is_ok());
        h.footswitches.push(13);
        let e = h.validate().unwrap_err().to_string();
        assert_eq!(e, "GPIO 13 is used by both FS 5 and encoder button");
        h.footswitches.pop();
        h.footswitches.push(28);
        assert!(h.validate().is_err());
        h.footswitches = vec![2; 9];
        assert!(h.validate().is_err());
    }

    #[test]
    fn free_pins() {
        let h = Hardware::default();
        assert_eq!(h.free_pin(17), Some(18));
        assert_eq!(h.free_pin(22), Some(24));
        let full = Hardware {
            footswitches: (0..8).collect(),
            encoder: Some(Encoder {
                a: 8,
                b: 9,
                button: Some(10),
                steps: 4,
            }),
        };
        assert_eq!(full.free_pin(26), Some(26));
        assert_eq!(h.free_pin(200), Some(26));
        assert!(h.shared_pins().is_empty());
        assert_eq!(full.shared_pins().len(), 4);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("hardware-test-{}", std::process::id()));
        let h = Hardware {
            footswitches: vec![26],
            encoder: None,
        };
        h.save(&dir).unwrap();
        assert_eq!(Hardware::load(&dir), h);
        std::fs::write(dir.join(FILE), "{").unwrap();
        assert_eq!(
            Hardware::load(&dir),
            Hardware::from_env(|n| std::env::var(n).ok())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
