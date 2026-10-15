//! MIDI control. What every modeler agrees on is followed as is: Program
//! Change recalls presets, Bank Select (CC 0 / CC 32) received before it to
//! reach another group of presets, MIDI clock sets the tempo, and any other
//! control is learned by the parameter it drives (Fractal and Boss have
//! nothing else). Scenes, tap tempo, tuner and footswitches have no common
//! numbers (Quad Cortex CC 43–45, Kemper CC 30/31/50…): they follow the
//! Line 6 Helix / HX map, also used by HeadRush and the one most MIDI foot
//! controllers ship presets for. Nothing has to be set up:
//!
//! | Message | Action |
//! |---------|--------|
//! | Program Change n | Preset n of the setlist (slot n), or of the preset list when no setlist is loaded |
//! | CC 0 (Bank Select MSB) | Group of 128 presets in the preset list: PC n then loads preset 128 × group + n |
//! | CC 32 (Bank Select LSB) | 0: leave the setlist (PC addresses the preset list); n: load setlist n |
//! | CC 49–53, 54–56 | Footswitches 1–5, 6–8 (what they do depends on the Live mode) |
//! | CC 60 | Looper: 0–63 overdub, 64–127 record |
//! | CC 61 | Looper: 0–63 stop, 64–127 play |
//! | CC 63 | Looper: undo (64–127) |
//! | CC 64 | Tap tempo (64–127) |
//! | CC 68 | Tuner on / off |
//! | CC 69 | Scene: 0–7 scene 1–8, 8 next, 9 previous |
//! | CC 71 | Footswitch mode: 0 Stomp, 2 Setlist, 3 Scenes, 4 next, 5 previous |
//! | CC 72 | Preset: 0–63 previous, 64–127 next |
//! | MIDI clock | Tempo (when enabled) |
//!
//! Any other CC (CC 1–3 being the expression pedals by convention) can be
//! learned by a block parameter or bypass, and is saved with the preset.
//!
//! LV2 plugins with a MIDI input also get the notes, pitch bend, pressure
//! and the CCs that have no global meaning (`for_plugins`).

use std::collections::VecDeque;

use crate::state::FootswitchMode;

/// MIDI channel 1–16, or 0 for all of them (omni)
pub const OMNI: u8 = 0;

/// MIDI settings, kept with the session
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MidiSettings {
    /// Channel listened to, 1–16, or OMNI
    #[serde(default)]
    pub channel: u8,
    /// Follow the tempo of an incoming MIDI clock
    #[serde(default = "yes")]
    pub clock: bool,
}

impl Default for MidiSettings {
    fn default() -> Self {
        Self {
            channel: OMNI,
            clock: true,
        }
    }
}

impl MidiSettings {
    /// Whether messages of `channel` (0–15) are listened to
    pub fn listens(&self, channel: u8) -> bool {
        self.channel == OMNI || self.channel == channel + 1
    }
}

fn yes() -> bool {
    true
}

/// A MIDI message the pedalboard reacts to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    /// Channel 0–15
    Program {
        channel: u8,
        program: u8,
    },
    Control {
        channel: u8,
        controller: u8,
        value: u8,
    },
    Clock,
    Start,
    Continue,
    Stop,
}

/// Decode one complete message (status byte first); None for the others
pub fn parse(bytes: &[u8]) -> Option<MidiMessage> {
    let (&status, data) = bytes.split_first()?;
    let channel = status & 0x0f;
    let data7 = |i: usize| data.get(i).copied().filter(|b| *b < 0x80);
    match status {
        0xf8 => Some(MidiMessage::Clock),
        0xfa => Some(MidiMessage::Start),
        0xfb => Some(MidiMessage::Continue),
        0xfc => Some(MidiMessage::Stop),
        s if s & 0xf0 == 0xc0 => Some(MidiMessage::Program {
            channel,
            program: data7(0)?,
        }),
        s if s & 0xf0 == 0xb0 => Some(MidiMessage::Control {
            channel,
            controller: data7(0)?,
            value: data7(1)?,
        }),
        _ => None,
    }
}

/// A short MIDI message as it travels to the audio thread (no allocation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawMidi {
    pub bytes: [u8; 3],
    pub len: u8,
}

impl RawMidi {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// The message for LV2 plugins with a MIDI input: channel voice messages
/// of the channel listened to, except Program Change (presets) and the
/// CCs of the global map; None for the others
pub fn for_plugins(bytes: &[u8], settings: &MidiSettings) -> Option<RawMidi> {
    let (&status, data) = bytes.split_first()?;
    if !(0x80..0xf0).contains(&status) || !settings.listens(status & 0x0f) {
        return None;
    }
    let len = match status & 0xf0 {
        0xc0 => return None,
        0xd0 => 2,
        _ => 3,
    };
    if bytes.len() < len || data[..len - 1].iter().any(|b| *b >= 0x80) {
        return None;
    }
    if status & 0xf0 == 0xb0 && is_global(data[0]) {
        return None;
    }
    let mut raw = RawMidi {
        bytes: [0; 3],
        len: len as u8,
    };
    raw.bytes[..len].copy_from_slice(&bytes[..len]);
    Some(raw)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperAction {
    Record,
    Overdub,
    Play,
    Stop,
    Undo,
}

/// What a message asks for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiAction {
    /// Preset n of the current bank (setlist or preset list)
    Program(u8),
    /// CC 0: group of 128 presets of the preset list
    BankMsb(u8),
    /// CC 32: 0 = the preset list, n = setlist n
    Bank(u8),
    Footswitch(usize),
    Looper(LooperAction),
    Tap,
    Tuner,
    Scene(usize),
    SceneStep(Step),
    Mode(FootswitchMode),
    ModeStep(Step),
    PresetStep(Step),
    /// A CC with no global meaning, for the learned assignments
    Controller {
        controller: u8,
        value: u8,
    },
}

/// Bank Select MSB: group of 128 presets
pub const CC_BANK_MSB: u8 = 0;
/// Bank Select LSB, used by Helix and Quad Cortex to pick a setlist
pub const CC_BANK: u8 = 32;
pub const CC_FOOTSWITCH: std::ops::RangeInclusive<u8> = 49..=56;
pub const CC_LOOPER_RECORD: u8 = 60;
pub const CC_LOOPER_PLAY: u8 = 61;
pub const CC_LOOPER_PLAY_ONCE: u8 = 62;
pub const CC_LOOPER_UNDO: u8 = 63;
pub const CC_TAP: u8 = 64;
pub const CC_LOOPER_REVERSE: u8 = 65;
pub const CC_LOOPER_SPEED: u8 = 66;
pub const CC_LOOPER_BLOCK: u8 = 67;
pub const CC_TUNER: u8 = 68;
pub const CC_SCENE: u8 = 69;
pub const CC_MODE: u8 = 71;
pub const CC_PRESET_STEP: u8 = 72;

/// CCs with a global meaning (or reserved for one, as on Helix): they
/// cannot be learned by a block
pub fn is_global(controller: u8) -> bool {
    matches!(controller, CC_BANK_MSB | CC_BANK | 49..=72 | 120..=127)
}

/// What a global CC does, to explain why it cannot be learned
pub fn global_name(controller: u8) -> &'static str {
    match controller {
        CC_BANK_MSB | CC_BANK => "bank select",
        49..=56 => "footswitches",
        CC_LOOPER_RECORD..=CC_LOOPER_UNDO | CC_LOOPER_REVERSE..=CC_LOOPER_BLOCK => "the looper",
        CC_TAP => "tap tempo",
        CC_TUNER => "the tuner",
        CC_SCENE => "scenes",
        CC_MODE => "the footswitch mode",
        CC_PRESET_STEP => "presets",
        120..=127 => "channel mode messages",
        _ => "future global commands",
    }
}

/// The action for `message`, if it is on the channel listened to
pub fn action(message: MidiMessage, settings: &MidiSettings) -> Option<MidiAction> {
    let on_channel = |channel: u8| settings.listens(channel);
    let high = |value: u8| value >= 64;
    match message {
        MidiMessage::Program { channel, program } if on_channel(channel) => {
            Some(MidiAction::Program(program))
        }
        MidiMessage::Control {
            channel,
            controller,
            value,
        } if on_channel(channel) => match controller {
            CC_BANK_MSB => Some(MidiAction::BankMsb(value)),
            CC_BANK => Some(MidiAction::Bank(value)),
            c if CC_FOOTSWITCH.contains(&c) => {
                Some(MidiAction::Footswitch((c - CC_FOOTSWITCH.start()) as usize))
            }
            CC_LOOPER_RECORD => Some(MidiAction::Looper(if high(value) {
                LooperAction::Record
            } else {
                LooperAction::Overdub
            })),
            CC_LOOPER_PLAY => Some(MidiAction::Looper(if high(value) {
                LooperAction::Play
            } else {
                LooperAction::Stop
            })),
            CC_LOOPER_UNDO if high(value) => Some(MidiAction::Looper(LooperAction::Undo)),
            CC_TAP if high(value) => Some(MidiAction::Tap),
            CC_TUNER => Some(MidiAction::Tuner),
            CC_SCENE => match value {
                0..=7 => Some(MidiAction::Scene(value as usize)),
                8 => Some(MidiAction::SceneStep(Step::Next)),
                9 => Some(MidiAction::SceneStep(Step::Previous)),
                _ => None,
            },
            CC_MODE => match value {
                0 => Some(MidiAction::Mode(FootswitchMode::Stomp)),
                2 => Some(MidiAction::Mode(FootswitchMode::Setlist)),
                3 => Some(MidiAction::Mode(FootswitchMode::Scenes)),
                4 => Some(MidiAction::ModeStep(Step::Next)),
                5 => Some(MidiAction::ModeStep(Step::Previous)),
                _ => None,
            },
            CC_PRESET_STEP => Some(MidiAction::PresetStep(if high(value) {
                Step::Next
            } else {
                Step::Previous
            })),
            c if is_global(c) => None,
            controller => Some(MidiAction::Controller { controller, value }),
        },
        _ => None,
    }
}

/// What a learned CC drives on a block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MidiTarget {
    /// 0–63 off, 64–127 on
    Bypass,
    /// Control input n of the block, over its whole range
    Parameter(usize),
}

/// A CC learned by a block, saved with the preset. As Helix's Min / Max,
/// `heel` and `toe` (percent of the target's range) are where the
/// controller at 0 and at 127 puts it; a toe below the heel reverses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MidiAssign {
    pub controller: u8,
    pub instance_id: usize,
    pub target: MidiTarget,
    #[serde(default, skip_serializing_if = "is_heel")]
    pub heel: u8,
    #[serde(default = "full_toe", skip_serializing_if = "is_toe")]
    pub toe: u8,
}

pub const HEEL: u8 = 0;
pub const TOE: u8 = 100;

fn full_toe() -> u8 {
    TOE
}

fn is_heel(heel: &u8) -> bool {
    *heel == HEEL
}

fn is_toe(toe: &u8) -> bool {
    *toe == TOE
}

impl MidiAssign {
    /// Over the whole range
    pub fn new(controller: u8, instance_id: usize, target: MidiTarget) -> Self {
        Self {
            controller,
            instance_id,
            target,
            heel: HEEL,
            toe: TOE,
        }
    }

    /// Position (0–1) in the target's range for CC value `value`
    pub fn position(&self, value: u8) -> f32 {
        let (heel, toe) = (f32::from(self.heel.min(TOE)), f32::from(self.toe.min(TOE)));
        (heel + (toe - heel) * f32::from(value.min(127)) / 127.0) / 100.0
    }
}

/// Value of a parameter at `position` (0–1) over `min`..`max`; stepped
/// parameters land on whole steps
pub fn scale(position: f32, min: f32, max: f32, stepped: bool) -> f32 {
    let v = min + (max - min) * position.clamp(0.0, 1.0);
    if stepped {
        v.round().clamp(min.min(max), min.max(max))
    } else {
        v
    }
}

/// Clock ticks per quarter note
const PPQN: usize = 24;
/// No tick for this long: the clock stopped
const CLOCK_TIMEOUT_US: u64 = 500_000;
/// Smallest change reported, in BPM
const CLOCK_RESOLUTION: f32 = 0.5;

/// Tempo of an incoming MIDI clock (24 ticks per beat), averaged over one
/// beat so that USB jitter does not move it
#[derive(Debug, Default)]
pub struct ClockTracker {
    ticks: VecDeque<u64>,
    reported: Option<f32>,
}

impl ClockTracker {
    /// A tick at `now_us` (microseconds, any origin): the new tempo when it
    /// changed by at least half a BPM
    pub fn tick(&mut self, now_us: u64) -> Option<f32> {
        if let Some(&last) = self.ticks.back() {
            if now_us <= last || now_us - last > CLOCK_TIMEOUT_US {
                // The clock stopped: report it again when it comes back,
                // whatever the tempo was set to meanwhile
                self.reset();
            }
        }
        self.ticks.push_back(now_us);
        if self.ticks.len() > PPQN + 1 {
            self.ticks.pop_front();
        }
        if self.ticks.len() <= PPQN {
            return None;
        }
        let beat_us = (self.ticks.back()? - self.ticks.front()?) as f32;
        if beat_us <= 0.0 {
            return None;
        }
        let bpm = 60_000_000.0 / beat_us;
        let bpm = (bpm / CLOCK_RESOLUTION).round() * CLOCK_RESOLUTION;
        match self.reported {
            Some(r) if (r - bpm).abs() < CLOCK_RESOLUTION => None,
            _ => {
                self.reported = Some(bpm);
                Some(bpm)
            }
        }
    }

    /// Forget the clock (it stopped): the next one is reported again
    pub fn reset(&mut self) {
        self.ticks.clear();
        self.reported = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(controller: u8, value: u8) -> MidiMessage {
        MidiMessage::Control {
            channel: 0,
            controller,
            value,
        }
    }

    #[test]
    fn parses_channel_messages_and_clock() {
        assert_eq!(
            parse(&[0xc3, 5]),
            Some(MidiMessage::Program {
                channel: 3,
                program: 5
            })
        );
        assert_eq!(
            parse(&[0xb0, 69, 2]),
            Some(MidiMessage::Control {
                channel: 0,
                controller: 69,
                value: 2
            })
        );
        assert_eq!(parse(&[0xf8]), Some(MidiMessage::Clock));
        // Notes, truncated and malformed messages are ignored
        assert_eq!(parse(&[0x90, 60, 100]), None);
        assert_eq!(parse(&[0xb0, 7]), None);
        assert_eq!(parse(&[0xc0, 0x80]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn follows_the_helix_map() {
        let s = MidiSettings::default();
        let act = |m| action(m, &s);
        assert_eq!(act(cc(69, 3)), Some(MidiAction::Scene(3)));
        assert_eq!(act(cc(69, 8)), Some(MidiAction::SceneStep(Step::Next)));
        assert_eq!(act(cc(64, 127)), Some(MidiAction::Tap));
        assert_eq!(act(cc(64, 0)), None);
        assert_eq!(act(cc(49, 127)), Some(MidiAction::Footswitch(0)));
        assert_eq!(act(cc(56, 0)), Some(MidiAction::Footswitch(7)));
        assert_eq!(
            act(cc(60, 0)),
            Some(MidiAction::Looper(LooperAction::Overdub))
        );
        assert_eq!(
            act(cc(71, 3)),
            Some(MidiAction::Mode(FootswitchMode::Scenes))
        );
        assert_eq!(act(cc(32, 2)), Some(MidiAction::Bank(2)));
        assert_eq!(act(cc(0, 1)), Some(MidiAction::BankMsb(1)));
        assert_eq!(act(cc(72, 100)), Some(MidiAction::PresetStep(Step::Next)));
        // Reserved global CCs do nothing; the others are for the blocks
        assert_eq!(act(cc(70, 127)), None);
        assert_eq!(
            act(cc(1, 90)),
            Some(MidiAction::Controller {
                controller: 1,
                value: 90
            })
        );
    }

    #[test]
    fn listens_to_one_channel_or_all() {
        let pc = |channel| MidiMessage::Program {
            channel,
            program: 1,
        };
        let omni = MidiSettings::default();
        assert!(action(pc(9), &omni).is_some());
        let ch2 = MidiSettings {
            channel: 2,
            ..Default::default()
        };
        assert!(action(pc(1), &ch2).is_some());
        assert!(action(pc(0), &ch2).is_none());
    }

    #[test]
    fn scales_controller_values() {
        let full = MidiAssign::new(1, 0, MidiTarget::Parameter(0));
        let at = |assign: &MidiAssign, value| scale(assign.position(value), 0.0, 10.0, false);
        assert_eq!(at(&full, 0), 0.0);
        assert_eq!(at(&full, 127), 10.0);
        assert_eq!(scale(full.position(64), 0.0, 2.0, true), 1.0);
        // Heel and toe narrow the range, or reverse it as on Helix
        let wah = MidiAssign {
            heel: 20,
            toe: 60,
            ..full
        };
        assert_eq!(at(&wah, 0), 2.0);
        assert!((at(&wah, 127) - 6.0).abs() < 1e-5);
        let volume = MidiAssign {
            heel: 100,
            toe: 0,
            ..full
        };
        assert_eq!(at(&volume, 127), 0.0);
        // Saved without the range when it is the whole one
        let json = serde_json::to_string(&full).unwrap();
        assert!(!json.contains("heel") && !json.contains("toe"), "{json}");
        let back: MidiAssign = serde_json::from_str(&json).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn clock_tempo_is_averaged_over_a_beat() {
        let mut clock = ClockTracker::default();
        // 120 BPM: a tick every 20.833 ms, with ±1 ms of jitter
        let tick = 500_000.0 / 24.0;
        let mut reported = Vec::new();
        for i in 0..96u64 {
            let jitter = if i % 2 == 0 { 1000.0 } else { -1000.0 };
            let t = (i as f64 * tick + jitter) as u64 + 1_000_000;
            reported.extend(clock.tick(t));
        }
        assert_eq!(reported, vec![120.0]);

        // The clock stops, then comes back at 90 BPM
        let start = 10_000_000u64;
        let tick = 60_000_000.0 / 90.0 / 24.0;
        let later: Vec<f32> = (0..30u64)
            .filter_map(|i| clock.tick(start + (i as f64 * tick) as u64))
            .collect();
        assert_eq!(later, vec![90.0]);
    }

    #[test]
    fn plugins_get_notes_and_free_controllers() {
        let omni = MidiSettings::default();
        let note = for_plugins(&[0x90, 60, 100], &omni).unwrap();
        assert_eq!(note.as_slice(), &[0x90, 60, 100]);
        assert_eq!(
            for_plugins(&[0xd3, 40], &omni).unwrap().as_slice(),
            &[0xd3, 40]
        );
        assert!(for_plugins(&[0xb0, 1, 64], &omni).is_some());
        // Presets, the global map and system messages stay with the host
        assert_eq!(for_plugins(&[0xc0, 3], &omni), None);
        assert_eq!(for_plugins(&[0xb0, 69, 1], &omni), None);
        assert_eq!(for_plugins(&[0xf8], &omni), None);
        assert_eq!(for_plugins(&[0x90, 60], &omni), None);
        let ch2 = MidiSettings { channel: 2, ..omni };
        assert_eq!(for_plugins(&[0x90, 60, 100], &ch2), None);
        assert!(for_plugins(&[0x91, 60, 100], &ch2).is_some());
    }
}
