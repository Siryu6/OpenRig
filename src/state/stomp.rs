//! What a footswitch does in stomp mode: turn a block on and off, or play
//! one of its switches (a plugin's toggle parameter, by port symbol), either
//! latching (each press flips it) or momentary (it flips while the switch is
//! held and flips back on release, as Hold on a glitch or a freeze).
//!
//! A preset keeps the block of each switch in `stomps` (instance ids, as old
//! apps and the `.openrig` format read them) and the rest in `stomp_modes`,
//! left out while every switch is a latching block switch.

use serde::{Deserialize, Serialize};

use crate::midi::MidiTarget;

/// How a stomp switch plays its block
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StompMode {
    /// Port symbol of the toggle parameter it plays; `None`: the block's
    /// on/off
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// Flips while held, flips back on release; else each press flips it
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub momentary: bool,
}

impl StompMode {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A stomp switch: its block (`None`: automatic, the next block no other
/// switch has) and how it plays it
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stomp {
    pub block: Option<usize>,
    pub mode: StompMode,
}

impl Stomp {
    pub fn is_default(&self) -> bool {
        self.block.is_none() && self.mode.is_default()
    }
}

/// The switches as saved: their blocks, and their modes (empty when all
/// are latching block switches)
pub fn save(stomps: &[Stomp]) -> (Vec<Option<usize>>, Vec<StompMode>) {
    let blocks = trimmed(stomps.iter().map(|s| s.block).collect(), Option::is_none);
    let modes = trimmed(
        stomps.iter().map(|s| s.mode.clone()).collect(),
        StompMode::is_default,
    );
    (blocks, modes)
}

/// The switches of a saved preset (`modes` may be shorter or absent)
pub fn load(blocks: &[Option<usize>], modes: &[StompMode]) -> Vec<Stomp> {
    let len = blocks.len().max(modes.len());
    let stomps = (0..len)
        .map(|slot| Stomp {
            block: blocks.get(slot).copied().flatten(),
            mode: modes.get(slot).cloned().unwrap_or_default(),
        })
        .collect();
    trimmed(stomps, Stomp::is_default)
}

/// `items` without the default ones at the end
fn trimmed<T>(mut items: Vec<T>, default: impl Fn(&T) -> bool) -> Vec<T> {
    while items.last().is_some_and(&default) {
        items.pop();
    }
    items
}

/// A switch held down: what its press changed, to undo on release
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Held {
    pub instance_id: usize,
    pub target: MidiTarget,
    /// Value (bypass: 1 on, 0 off) before and after the press
    pub before: f32,
    pub after: f32,
    /// Chains loaded before the press: another preset has nothing to undo
    pub load: u64,
}

/// What a press of a stomp switch did
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pressed {
    pub instance_id: usize,
    pub target: MidiTarget,
    /// Now on (the block, or the switch parameter)
    pub on: bool,
    /// Momentary: what the release undoes
    pub held: Option<Held>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold() -> StompMode {
        StompMode {
            param: Some("hold".into()),
            momentary: true,
        }
    }

    #[test]
    fn plain_switches_save_as_block_ids_only() {
        let stomps = vec![
            Stomp::default(),
            Stomp {
                block: Some(3),
                mode: StompMode::default(),
            },
            Stomp::default(),
        ];
        let (blocks, modes) = save(&stomps);
        assert_eq!(blocks, [None, Some(3)]);
        assert!(modes.is_empty());
        assert_eq!(load(&blocks, &modes), stomps[..2]);
    }

    #[test]
    fn modes_round_trip() {
        let stomps = vec![
            Stomp {
                block: Some(1),
                mode: hold(),
            },
            Stomp {
                block: None,
                mode: StompMode {
                    param: None,
                    momentary: true,
                },
            },
        ];
        let (blocks, modes) = save(&stomps);
        assert_eq!(blocks, [Some(1)]);
        assert_eq!(modes.len(), 2);
        assert_eq!(load(&blocks, &modes), stomps);
        let json = serde_json::to_string(&modes).unwrap_or_default();
        assert_eq!(
            json,
            r#"[{"param":"hold","momentary":true},{"momentary":true}]"#
        );
    }
}
