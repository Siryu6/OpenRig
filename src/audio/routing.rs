//! Inputs and outputs beyond the guitar input and one stereo pair: which
//! physical input feeds the chain, and what each physical output plays (the
//! chain's main mix, or an input passed straight through), with its level,
//! balance, mute, solo and polarity.
//!
//! Set in the Routing view and kept in `routing.json` in the data directory:
//! like `hardware.json`, it belongs to the rig, not to a preset, so changing
//! presets never touches the front-of-house levels. Without the file, the
//! main mix plays on outputs 1 and 2, as before multi-channel interfaces were
//! supported.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::plugins::chain::{pan_gains, ramp};

const FILE: &str = "routing.json";
/// Inputs and outputs the pedalboard registers at most
pub const MAX_PORTS: usize = 16;
/// Level range of an output, in dB
pub const GAIN_RANGE_DB: (f32, f32) = (-60.0, 12.0);
/// Gain changes, mute, solo and source changes are ramped over this time
const RAMP_MS: f32 = 10.0;

/// What an output plays
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The chain output (after the looper)
    #[default]
    Main,
    /// A physical input as it comes in, without effects (0 = input 1)
    Input(u8),
}

/// How a stereo source plays on a single output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fold {
    /// Both sides at half level
    #[default]
    Sum,
    Left,
    Right,
}

/// One or two outputs playing a source
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    /// First output (0 = output 1)
    pub port: u8,
    /// A stereo pair: `port` and the next output
    pub stereo: bool,
    pub source: Source,
    /// A stereo source on a single output
    #[serde(default)]
    pub fold: Fold,
    #[serde(default)]
    pub gain_db: f32,
    /// Stereo pair: balance of the two sides, or position of a mono source.
    /// -1 left … 1 right
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Polarity flipped
    #[serde(default)]
    pub invert: bool,
}

impl Strip {
    /// The main mix on `port` (and the next output if `stereo`), at unity
    pub fn main(port: u8, stereo: bool) -> Self {
        Self {
            port,
            stereo,
            source: Source::Main,
            fold: Fold::Sum,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            invert: false,
        }
    }

    /// Outputs it covers
    fn ports(&self) -> std::ops::Range<usize> {
        let first = self.port as usize;
        first..first + if self.stereo { 2 } else { 1 }
    }
}

/// The input feeding the chain and what each output plays
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routing {
    /// Input feeding the chain and the tuner (0 = input 1)
    pub input: u8,
    /// Outputs not covered by a strip are silent
    pub strips: Vec<Strip>,
}

impl Default for Routing {
    /// The main mix on outputs 1 and 2
    fn default() -> Self {
        Self {
            input: 0,
            strips: vec![Strip::main(0, true)],
        }
    }
}

impl Routing {
    /// The saved routing, else the default one
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        match std::fs::read(&path) {
            Ok(json) => match serde_json::from_slice::<Self>(&json)
                .map_err(anyhow::Error::from)
                .and_then(|r| r.validate().map(|()| r))
            {
                Ok(routing) => return routing,
                Err(e) => tracing::warn!("{}: {e:#}; using the default routing", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{}: {e}; using the default routing", path.display()),
        }
        Self::default()
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

    /// An output used by two strips, or one that cannot exist
    pub fn validate(&self) -> Result<()> {
        if self.input as usize >= MAX_PORTS {
            bail!("There is no input {}", self.input as usize + 1);
        }
        let mut used = [false; MAX_PORTS];
        for strip in &self.strips {
            if let Source::Input(i) = strip.source {
                if i as usize >= MAX_PORTS {
                    bail!("There is no input {}", i as usize + 1);
                }
            }
            if !strip.gain_db.is_finite() || !strip.pan.is_finite() {
                bail!("Output {}: invalid level or pan", strip.port as usize + 1);
            }
            for port in strip.ports() {
                if port >= MAX_PORTS {
                    bail!("There is no output {}", port + 1);
                }
                if std::mem::replace(&mut used[port], true) {
                    bail!("Output {} is used twice", port + 1);
                }
            }
        }
        Ok(())
    }

    /// Gains for the audio thread, for an interface with `inputs` and
    /// `outputs` ports. A strip past the last output is ignored; a pair on
    /// the last output plays as a single output.
    pub fn mix(&self, inputs: usize, outputs: usize) -> Mix {
        let mut mix = Mix {
            input: (self.input as usize).min(inputs.saturating_sub(1)) as u8,
            ports: [PortMix::SILENT; MAX_PORTS],
        };
        let outputs = outputs.min(MAX_PORTS);
        let solo = self.strips.iter().any(|s| s.solo);
        for strip in &self.strips {
            let first = strip.port as usize;
            if first >= outputs {
                continue;
            }
            let heard = !strip.mute && (!solo || strip.solo);
            let gain = if heard {
                let db = strip.gain_db.clamp(GAIN_RANGE_DB.0, GAIN_RANGE_DB.1);
                let polarity = if strip.invert { -1.0 } else { 1.0 };
                10f32.powf(db / 20.0) * polarity
            } else {
                0.0
            };
            let input = match strip.source {
                Source::Main => None,
                Source::Input(i) if (i as usize) < inputs => Some(i),
                // An input this interface does not have
                Source::Input(_) => continue,
            };
            if strip.stereo && first + 1 < outputs {
                let (left, right) = pan_gains(strip.pan);
                for (port, side, side_gain) in [(first, 0, left), (first + 1, 1, right)] {
                    let g = gain * side_gain;
                    mix.ports[port] = match input {
                        None => {
                            let mut main = [0.0; 2];
                            main[side] = g;
                            PortMix::main(main)
                        }
                        Some(i) => PortMix::input(i, g),
                    };
                }
            } else {
                mix.ports[first] = match input {
                    None => PortMix::main(match strip.fold {
                        Fold::Sum => [gain * 0.5, gain * 0.5],
                        Fold::Left => [gain, 0.0],
                        Fold::Right => [0.0, gain],
                    }),
                    Some(i) => PortMix::input(i, gain),
                };
            }
        }
        mix
    }
}

/// What one output plays, as gains
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortMix {
    /// Gains of the left and right main mix
    pub main: [f32; 2],
    /// Input passed through, and its gain
    pub input: Option<u8>,
    pub input_gain: f32,
}

impl PortMix {
    pub const SILENT: Self = Self {
        main: [0.0; 2],
        input: None,
        input_gain: 0.0,
    };

    fn main(main: [f32; 2]) -> Self {
        Self {
            main,
            ..Self::SILENT
        }
    }

    fn input(input: u8, gain: f32) -> Self {
        Self {
            input: Some(input),
            input_gain: gain,
            ..Self::SILENT
        }
    }

    fn is_silent(&self) -> bool {
        self.main == [0.0; 2] && self.input_gain == 0.0
    }
}

/// What the audio thread plays: computed by [`Routing::mix`] off the audio
/// thread and sent whole (no allocation)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mix {
    /// Input feeding the chain
    pub input: u8,
    pub ports: [PortMix; MAX_PORTS],
}

/// Sends a new routing to the audio thread
#[derive(Clone)]
pub struct RoutingControl {
    tx: Sender<Mix>,
    /// Ports the pedalboard registered
    pub inputs: usize,
    pub outputs: usize,
}

impl RoutingControl {
    pub fn apply(&self, routing: &Routing) {
        let _ = self.tx.try_send(routing.mix(self.inputs, self.outputs));
    }
}

/// Output mixer of the audio thread: plays the main mix and the inputs on
/// the outputs, with every gain ramped
pub struct OutputRouter {
    target: Mix,
    current: [PortMix; MAX_PORTS],
    rx: Receiver<Mix>,
    step: f32,
}

impl OutputRouter {
    /// Router starting on `routing`, and the control to change it
    pub fn new(
        routing: &Routing,
        inputs: usize,
        outputs: usize,
        sample_rate: f32,
    ) -> (Self, RoutingControl) {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let target = routing.mix(inputs, outputs);
        let router = Self {
            target,
            current: target.ports,
            rx,
            step: 1.0 / (sample_rate * RAMP_MS / 1000.0).max(1.0),
        };
        (
            router,
            RoutingControl {
                tx,
                inputs,
                outputs,
            },
        )
    }

    /// Take the latest routing sent (real-time safe), once per cycle
    pub fn update(&mut self) {
        while let Ok(mix) = self.rx.try_recv() {
            self.target = mix;
        }
    }

    /// Input feeding the chain
    pub fn chain_input(&self) -> usize {
        self.target.input as usize
    }

    /// Fill output `port` from the main mix and the inputs (`input(i)` is
    /// the buffer of input i)
    pub fn render<'a>(
        &mut self,
        port: usize,
        main: [&[f32]; 2],
        input: impl Fn(u8) -> Option<&'a [f32]>,
        out: &mut [f32],
    ) {
        let Some(current) = self.current.get_mut(port) else {
            out.fill(0.0);
            return;
        };
        let target = self.target.ports[port];
        if *current == target {
            // Steady: no ramp
            if target.is_silent() {
                out.fill(0.0);
                return;
            }
            let buffer = target.input.and_then(&input);
            for (i, o) in out.iter_mut().enumerate() {
                let passed = buffer.map_or(0.0, |b| b[i] * target.input_gain);
                *o = target.main[0] * main[0][i] + target.main[1] * main[1][i] + passed;
            }
            return;
        }
        // Another input: fade the current one out, then the new one in
        let mut buffer = current.input.and_then(&input);
        for (i, o) in out.iter_mut().enumerate() {
            ramp(&mut current.main[0], target.main[0], self.step);
            ramp(&mut current.main[1], target.main[1], self.step);
            let switching = current.input != target.input;
            let gain = if switching { 0.0 } else { target.input_gain };
            ramp(&mut current.input_gain, gain, self.step);
            if switching && current.input_gain == 0.0 {
                current.input = target.input;
                buffer = current.input.and_then(&input);
            }
            let passed = buffer.map_or(0.0, |b| b[i] * current.input_gain);
            *o = current.main[0] * main[0][i] + current.main[1] * main[1][i] + passed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(port: u8, stereo: bool, source: Source) -> Strip {
        Strip {
            source,
            ..Strip::main(port, stereo)
        }
    }

    #[test]
    fn default_plays_the_main_mix_on_outputs_1_and_2() {
        let mix = Routing::default().mix(1, 4);
        assert_eq!(mix.ports[0].main, [1.0, 0.0]);
        assert_eq!(mix.ports[1].main, [0.0, 1.0]);
        assert!(mix.ports[2].is_silent() && mix.ports[3].is_silent());
    }

    #[test]
    fn a_pair_on_the_last_output_folds_to_mono() {
        let mix = Routing::default().mix(1, 1);
        assert_eq!(mix.ports[0].main, [0.5, 0.5]);
    }

    #[test]
    fn mono_outputs_fold_the_main_mix() {
        let routing = Routing {
            input: 0,
            strips: vec![
                Strip {
                    fold: Fold::Left,
                    ..Strip::main(2, false)
                },
                Strip {
                    fold: Fold::Right,
                    ..Strip::main(3, false)
                },
            ],
        };
        let mix = routing.mix(2, 4);
        assert_eq!(mix.ports[2].main, [1.0, 0.0]);
        assert_eq!(mix.ports[3].main, [0.0, 1.0]);
    }

    #[test]
    fn solo_mutes_the_others_and_invert_flips_the_polarity() {
        let routing = Routing {
            input: 0,
            strips: vec![
                Strip::main(0, true),
                Strip {
                    solo: true,
                    invert: true,
                    ..strip(2, false, Source::Input(1))
                },
            ],
        };
        let mix = routing.mix(2, 4);
        assert!(mix.ports[0].is_silent() && mix.ports[1].is_silent());
        assert_eq!(mix.ports[2].input, Some(1));
        assert_eq!(mix.ports[2].input_gain, -1.0);
    }

    #[test]
    fn missing_inputs_and_outputs_are_ignored() {
        let routing = Routing {
            input: 5,
            strips: vec![strip(0, false, Source::Input(3)), Strip::main(6, true)],
        };
        let mix = routing.mix(2, 4);
        assert_eq!(mix.input, 1);
        assert!(mix.ports.iter().all(PortMix::is_silent));
    }

    #[test]
    fn overlapping_strips_are_refused() {
        let routing = Routing {
            input: 0,
            strips: vec![Strip::main(0, true), Strip::main(1, false)],
        };
        assert!(routing.validate().is_err());
        assert!(Routing::default().validate().is_ok());
    }

    #[test]
    fn saved_routing_is_read_back() {
        let dir = std::env::temp_dir().join(format!("pedalboard-routing-{}", uuid::Uuid::new_v4()));
        let routing = Routing {
            input: 1,
            strips: vec![
                Strip::main(0, true),
                Strip {
                    gain_db: -6.0,
                    ..strip(2, false, Source::Input(1))
                },
            ],
        };
        routing.save(&dir).unwrap();
        assert_eq!(Routing::load(&dir), routing);
        std::fs::write(dir.join(FILE), b"{oops").unwrap();
        assert_eq!(Routing::load(&dir), Routing::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn router_ramps_to_a_new_routing() {
        let (mut router, control) = OutputRouter::new(&Routing::default(), 2, 4, 1000.0);
        let main = [1.0f32; 8];
        let guitar = [0.5f32; 8];
        let input = |i: u8| (i == 1).then_some(&guitar[..]);
        let mut out = [0.0f32; 8];
        router.render(0, [&main, &main], input, &mut out);
        assert_eq!(out, [1.0; 8]);

        // Output 1 switches to input 2: the main mix fades out over 10 ms
        // (10 samples at 1 kHz) while the input fades in
        control.apply(&Routing {
            input: 0,
            strips: vec![strip(0, false, Source::Input(1))],
        });
        router.update();
        let mut out = [0.0f32; 16];
        let main = [1.0f32; 16];
        let guitar = [0.5f32; 16];
        let input = |i: u8| (i == 1).then_some(&guitar[..]);
        router.render(0, [&main, &main], input, &mut out);
        assert!(out[0] > 0.5 && out[0] < 1.0, "ramping: {}", out[0]);
        assert!((out[15] - 0.5).abs() < 1e-6, "input alone: {}", out[15]);
    }
}
