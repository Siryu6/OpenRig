//! What is wired to the pedalboard: how many footswitches and on which
//! GPIO, and the rotary encoder, if any. Set in Settings → Hardware and
//! kept in `hardware.json` in the data directory (it belongs to the board,
//! not to a preset or a session).
//!
//! Without the file, the defaults come from the environment, then from the
//! reference build (4 footswitches on GPIO 17, 27, 22, 23; encoder on 5, 6,
//! push button on 13). An input on a GPIO the kernel gave to a driver (an
//! audio HAT's I2S lines, SPI...) is left out ([`Hardware::usable`]), so the
//! defaults never take a HAT's pins:
//! - `PEDALBOARD_FOOTSWITCH_PINS`: footswitch GPIOs, empty for none
//! - `PEDALBOARD_ENCODER_PINS`: A,B[,button], empty for no encoder
//! - `PEDALBOARD_ENCODER_STEPS`: quadrature steps per detent
//! - `PEDALBOARD_I2C_ENCODERS`: external encoders on I2C (`seesaw`,
//!   `seesaw-quad` or `m5-stack8`), none by default

use std::path::Path;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub use crate::i2c_encoders::{I2cEncoders, Module as I2cModule};

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
    /// External encoders on the I2C bus, for the knob strip
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i2c_encoders: Option<I2cEncoders>,
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
            i2c_encoders: None,
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
        let hardware = crate::persist::load_json::<Self>(&path)
            .and_then(|h| h.map(|h| h.validate().map(|()| h)).transpose());
        hardware
            .unwrap_or_else(|e| {
                warn!(
                    path = %path.display(),
                    error = format_args!("{e:#}"),
                    "Hardware settings unreadable; using the defaults"
                );
                None
            })
            .unwrap_or_else(|| Self::from_env(|name| std::env::var(name).ok()))
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
            pins.map_err(|_| warn!(var = name, value = %value, "Invalid GPIO pins ignored"))
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
                    warn!(
                        var = "PEDALBOARD_ENCODER_PINS",
                        "Encoder pins must be A,B[,button]; no encoder"
                    );
                    None
                }
            };
        }
        if let Some(module) = var("PEDALBOARD_I2C_ENCODERS") {
            hardware.i2c_encoders = match module.trim() {
                "" => None,
                name => match serde_json::from_value(serde_json::Value::String(name.into())) {
                    Ok(module) => Some(I2cEncoders::new(module)),
                    Err(_) => {
                        warn!(
                            var = "PEDALBOARD_I2C_ENCODERS",
                            board = %name,
                            "Unknown I2C encoder board ignored"
                        );
                        None
                    }
                },
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
            warn!(
                error = format_args!("{e:#}"),
                "Hardware from the environment invalid; using the defaults"
            );
            return Self::default();
        }
        hardware
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.validate()?;
        crate::persist::save_json(&data_dir.join(FILE), self)
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
        if self.i2c_encoders.is_some() {
            pins.push((2, "I2C SDA (external encoders)".into()));
            pins.push((3, "I2C SCL (external encoders)".into()));
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
        if let Some(e) = &self.i2c_encoders {
            if !crate::i2c_encoders::STEPS.contains(&e.steps) {
                bail!("External encoders, counts per detent: 1, 2 or 4");
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
            .filter(|(_, user)| !user.starts_with("I2C"))
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

/// A GPIO line, as the kernel reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Free,
    /// Taken by a driver: its label (empty when the pin is set to another
    /// function, I2S or SPI, rather than requested as a GPIO)
    Used(String),
}

/// The inputs that can be opened, and those left out
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usable {
    /// Footswitch slot (0-based) and its GPIO
    pub footswitches: Vec<(usize, u8)>,
    pub encoder: Option<Encoder>,
    /// Why an input is left out: "FS 2: GPIO 27 is used by the audio HAT
    /// (I2S), left out"
    pub skipped: Vec<String>,
}

impl Hardware {
    /// The inputs whose GPIO is free. One the kernel gave to a driver is
    /// left out: taking it would cut that driver off its pin (an audio HAT
    /// loses outputs). `line` is `None` when the state cannot be read: the
    /// input is tried, the kernel refuses it if it is taken.
    pub fn usable(&self, line: impl Fn(u8) -> Option<Line>) -> Usable {
        let mut usable = Usable::default();
        let free = |pin: u8, user: &str, skipped: &mut Vec<String>| match line(pin) {
            Some(Line::Used(consumer)) => {
                skipped.push(format!(
                    "{user}: GPIO {pin} is used by {}, left out",
                    owner(pin, &consumer)
                ));
                false
            }
            Some(Line::Free) | None => true,
        };
        for (slot, &pin) in self.footswitches.iter().enumerate() {
            if free(pin, &format!("FS {}", slot + 1), &mut usable.skipped) {
                usable.footswitches.push((slot, pin));
            }
        }
        if let Some(mut encoder) = self.encoder {
            // Both pins, or no encoder: its button alone would only confirm
            let a = free(encoder.a, "Encoder A", &mut usable.skipped);
            let b = free(encoder.b, "Encoder B", &mut usable.skipped);
            if a && b {
                if let Some(button) = encoder.button {
                    if !free(button, "Encoder button", &mut usable.skipped) {
                        encoder.button = None;
                    }
                }
                usable.encoder = Some(encoder);
            }
        }
        usable
    }
}

/// Who holds a GPIO: the driver's label, else what the pin is likely
/// set to on a Raspberry Pi
fn owner(pin: u8, consumer: &str) -> String {
    if !consumer.is_empty() {
        return format!("a driver ({consumer})");
    }
    match pin {
        18..=27 => "the audio HAT (I2S)".into(),
        7..=11 => "SPI".into(),
        _ => match bus(pin) {
            Some(bus) => bus.into(),
            None => "a driver".into(),
        },
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
    fn i2c_encoders_take_the_i2c_pins() {
        let mut h = Hardware {
            i2c_encoders: Some(I2cEncoders::new(I2cModule::M5Stack8)),
            ..Hardware::default()
        };
        assert!(h.validate().is_ok() && h.shared_pins().is_empty());
        h.footswitches.push(3);
        assert!(h.validate().is_err());
        let from_env = Hardware::from_env(env(&[("PEDALBOARD_I2C_ENCODERS", "seesaw-quad")]));
        assert_eq!(
            from_env.i2c_encoders,
            Some(I2cEncoders::new(I2cModule::SeesawQuad))
        );
        let unknown = Hardware::from_env(env(&[("PEDALBOARD_I2C_ENCODERS", "other")]));
        assert_eq!(unknown.i2c_encoders, None);
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
            i2c_encoders: None,
        };
        assert_eq!(full.free_pin(26), Some(26));
        assert_eq!(h.free_pin(200), Some(26));
        assert!(h.shared_pins().is_empty());
        assert_eq!(full.shared_pins().len(), 4);
    }

    /// GPIO lines as the Pi 5 reports them with an 8-channel audio HAT
    /// (hifiberry-dac8x overlay): I2S on 18–27, the ADC detection on 5
    fn dac8x(pin: u8) -> Option<Line> {
        Some(match pin {
            18..=27 => Line::Used(String::new()),
            5 => Line::Used("hasadc".into()),
            _ => Line::Free,
        })
    }

    #[test]
    fn busy_pins_left_out() {
        let usable = Hardware::default().usable(dac8x);
        assert_eq!(usable.footswitches, vec![(0, 17)]);
        assert_eq!(usable.encoder, None);
        assert_eq!(
            usable.skipped,
            vec![
                "FS 2: GPIO 27 is used by the audio HAT (I2S), left out",
                "FS 3: GPIO 22 is used by the audio HAT (I2S), left out",
                "FS 4: GPIO 23 is used by the audio HAT (I2S), left out",
                "Encoder A: GPIO 5 is used by a driver (hasadc), left out",
            ]
        );
        // Free pins: everything opens
        let all = Hardware::default().usable(|_| Some(Line::Free));
        assert_eq!(all.footswitches, vec![(0, 17), (1, 27), (2, 22), (3, 23)]);
        assert_eq!(all.encoder, Some(Encoder::default()));
        assert!(all.skipped.is_empty());
        // Unknown state: tried (the kernel refuses a taken line)
        assert_eq!(Hardware::default().usable(|_| None), all);
    }

    #[test]
    fn busy_encoder_button_left_out() {
        let h = Hardware {
            footswitches: vec![4, 16],
            encoder: Some(Encoder {
                a: 12,
                b: 13,
                button: Some(8),
                steps: 4,
            }),
            i2c_encoders: None,
        };
        let usable = h.usable(|pin| {
            Some(if pin == 8 {
                Line::Used(String::new())
            } else {
                Line::Free
            })
        });
        assert_eq!(usable.footswitches, vec![(0, 4), (1, 16)]);
        assert_eq!(
            usable.encoder.map(|e| (e.a, e.b, e.button)),
            Some((12, 13, None))
        );
        assert_eq!(
            usable.skipped,
            vec!["Encoder button: GPIO 8 is used by SPI, left out"]
        );
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("hardware-test-{}", std::process::id()));
        let mut h = Hardware {
            footswitches: vec![26],
            encoder: None,
            i2c_encoders: None,
        };
        h.save(&dir).unwrap();
        assert_eq!(Hardware::load(&dir), h);
        h.i2c_encoders = Some(I2cEncoders::new(I2cModule::SeesawQuad));
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
