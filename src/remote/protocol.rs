//! What the web remote and the pedal say to each other over the WebSocket.
//!
//! The pedal sends JSON text messages: the whole `Snapshot` when the state
//! changed (presets, scenes, blocks, knobs…) and `Telemetry` (tuner,
//! meters) a few times a second. Frames of the full editor are binary
//! messages (see `frame_header`).
//!
//! The phone sends `ClientMessage`s. Those that change the pedal are
//! `Command`s, applied on the UI thread with the functions the touchscreen
//! uses, so the screen follows as it does for MIDI.

use serde::{Deserialize, Serialize};

/// Window size limits of the full editor (the UI is at least 800 wide)
pub const MIN_WIDTH: u32 = 800;
pub const MIN_HEIGHT: u32 = 480;
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1200;

/// A message from a web client
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Tuner values wanted (the tuner tab is open)
    Tuner { on: bool },
    /// Full editor: frames wanted, at this window size
    Stream {
        on: bool,
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
    },
    /// Everything else changes the pedal
    #[serde(untagged)]
    Command(Command),
}

/// Something to do on the pedal
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    LoadPreset {
        id: String,
    },
    /// Next (true) or previous preset, in the setlist when one is loaded
    PresetStep {
        next: bool,
    },
    /// Load the preset of a slot of the setlist
    SetlistSlot {
        slot: usize,
    },
    /// Recall scene `slot` (0-7) of the preset
    SelectScene {
        slot: usize,
    },
    /// Turn a block on or off; `uri` guards against a preset loaded meanwhile
    ToggleBypass {
        block: usize,
        uri: String,
    },
    SetParameter {
        block: usize,
        uri: String,
        index: usize,
        value: f32,
    },
    Tap,
    SetTempo {
        bpm: f32,
    },
    /// Full editor: window size wanted by a client
    Resize {
        width: u32,
        height: u32,
    },
    /// Full editor: pointer on the canvas, in window pixels
    Pointer {
        kind: PointerKind,
        x: f32,
        y: f32,
        #[serde(default)]
        dx: f32,
        #[serde(default)]
        dy: f32,
    },
    /// Full editor: a key, as text ("a", "\u{8}" for backspace…)
    Key {
        text: String,
        down: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerKind {
    Down,
    Move,
    Up,
    Scroll,
    Leave,
}

impl Command {
    /// Parameter changes are merged per knob before they reach the UI thread
    pub fn knob(&self) -> Option<(usize, usize)> {
        match self {
            Command::SetParameter { block, index, .. } => Some((*block, *index)),
            _ => None,
        }
    }

    /// A value the pedal can act on (numbers in range, sizes clamped)
    pub fn sanitized(self) -> Option<Command> {
        match self {
            Command::SetParameter { value, .. } if !value.is_finite() => None,
            Command::SetTempo { bpm } if !bpm.is_finite() => None,
            Command::Pointer { x, y, dx, dy, .. }
                if ![x, y, dx, dy].iter().all(|v| v.is_finite()) =>
            {
                None
            }
            Command::Key { ref text, .. } if text.chars().count() != 1 => None,
            Command::Resize { width, height } => Some(Command::Resize {
                width: width.clamp(MIN_WIDTH, MAX_WIDTH),
                height: height.clamp(MIN_HEIGHT, MAX_HEIGHT),
            }),
            other => Some(other),
        }
    }
}

/// Everything a web client shows, sent again whenever it changes
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Snapshot {
    /// App version
    pub version: String,
    /// The pedal runs without a screen: the full editor can be streamed
    pub editor: bool,
    /// Audio running (JACK)
    pub audio: bool,
    /// The preset playing, and all of them
    pub preset: Option<PresetRef>,
    pub presets: Vec<PresetRef>,
    pub setlist: Option<Setlist>,
    pub scenes: Vec<SceneTile>,
    pub scene_name: String,
    pub bpm: f32,
    pub blocks: Vec<Block>,
    /// Toast shown on the pedal
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PresetRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Setlist {
    pub id: String,
    pub name: String,
    pub slots: Vec<SetlistSlot>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SetlistSlot {
    pub slot: usize,
    pub name: String,
    pub empty: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SceneTile {
    pub slot: usize,
    pub name: String,
    pub used: bool,
    pub active: bool,
}

/// A block of the chain, in signal order
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Block {
    pub id: usize,
    pub uri: String,
    pub name: String,
    /// "#rrggbb"
    pub color: String,
    pub bypassed: bool,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Param {
    pub index: usize,
    pub symbol: String,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub unit: String,
    /// "float", "int", "toggle", "enum"
    pub kind: String,
    pub options: Vec<ParamOption>,
    /// In the block's knob strip (the main knobs)
    pub strip: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParamOption {
    pub label: String,
    pub value: f32,
}

/// Values that move all the time, sent at a modest rate
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Telemetry {
    /// Note heard by the tuner, when one is
    pub tuner: Option<TunerReading>,
    /// Input, left and right output levels, 0-1 (meter positions)
    pub meters: [f32; 3],
    pub clip_in: bool,
    pub clip_out: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TunerReading {
    pub frequency: f32,
    pub note: String,
    pub octave: i32,
    pub cents: f32,
    pub tuned: bool,
}

/// A message to the clients: `{"type": "state", ...}`
pub fn tagged<T: Serialize>(kind: &str, value: &T) -> String {
    let mut json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(map) = &mut json {
        map.insert("type".into(), kind.into());
    }
    json.to_string()
}

/// Encoding of a frame rectangle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameFormat {
    Png = 1,
    Jpeg = 2,
}

/// Size of the header of a binary frame message
pub const FRAME_HEADER: usize = 14;

/// Binary frame message: version 1, format, x, y, width, height of the
/// rectangle, then width and height of the window (u16, little endian),
/// then the encoded picture
pub fn frame_header(format: FrameFormat, rect: [u16; 4], window: [u16; 2]) -> [u8; FRAME_HEADER] {
    let mut header = [0u8; FRAME_HEADER];
    header[0] = 1;
    header[1] = format as u8;
    for (i, v) in rect.iter().chain(window.iter()).enumerate() {
        header[2 + i * 2..4 + i * 2].copy_from_slice(&v.to_le_bytes());
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Option<ClientMessage> {
        serde_json::from_str(json).ok()
    }

    #[test]
    fn client_messages_parse() {
        assert_eq!(
            parse(r#"{"type":"tuner","on":true}"#),
            Some(ClientMessage::Tuner { on: true })
        );
        assert_eq!(
            parse(r#"{"type":"load_preset","id":"p1"}"#),
            Some(ClientMessage::Command(Command::LoadPreset {
                id: "p1".into()
            }))
        );
        assert_eq!(
            parse(r#"{"type":"set_parameter","block":3,"uri":"u","index":1,"value":0.5}"#),
            Some(ClientMessage::Command(Command::SetParameter {
                block: 3,
                uri: "u".into(),
                index: 1,
                value: 0.5
            }))
        );
        assert_eq!(
            parse(r#"{"type":"stream","on":true,"width":1024,"height":600}"#),
            Some(ClientMessage::Stream {
                on: true,
                width: 1024,
                height: 600
            })
        );
        assert_eq!(
            parse(r#"{"type":"pointer","kind":"down","x":1,"y":2}"#),
            Some(ClientMessage::Command(Command::Pointer {
                kind: PointerKind::Down,
                x: 1.0,
                y: 2.0,
                dx: 0.0,
                dy: 0.0
            }))
        );
        assert_eq!(parse(r#"{"type":"rm_rf"}"#), None);
        assert_eq!(parse(r#"{"type":"select_scene","slot":"x"}"#), None);
    }

    #[test]
    fn commands_are_sanitized() {
        let nan = Command::SetParameter {
            block: 1,
            uri: String::new(),
            index: 0,
            value: f32::NAN,
        };
        assert_eq!(nan.sanitized(), None);
        assert_eq!(
            Command::Resize {
                width: 10,
                height: 99999
            }
            .sanitized(),
            Some(Command::Resize {
                width: MIN_WIDTH,
                height: MAX_HEIGHT
            })
        );
        let two = Command::Key {
            text: "ab".into(),
            down: true,
        };
        assert_eq!(two.sanitized(), None);
    }

    #[test]
    fn snapshot_serializes_with_its_tag() {
        let snapshot = Snapshot {
            version: "1".into(),
            blocks: vec![Block {
                id: 4,
                name: "Amp".into(),
                params: vec![Param {
                    name: "Gain".into(),
                    kind: "float".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let json: serde_json::Value =
            serde_json::from_str(&tagged("state", &snapshot)).unwrap_or_default();
        assert_eq!(json["type"], "state");
        assert_eq!(json["blocks"][0]["params"][0]["name"], "Gain");
        assert_eq!(json["preset"], serde_json::Value::Null);
    }

    #[test]
    fn frame_header_layout() {
        let h = frame_header(FrameFormat::Jpeg, [1, 2, 300, 4], [1024, 600]);
        assert_eq!(h[0], 1);
        assert_eq!(h[1], 2);
        assert_eq!(u16::from_le_bytes([h[6], h[7]]), 300);
        assert_eq!(u16::from_le_bytes([h[10], h[11]]), 1024);
        assert_eq!(u16::from_le_bytes([h[12], h[13]]), 600);
    }
}
