//! How the app draws the blocks of a rig: their shape in the rack, their
//! family and their colour. OpenRig writes it for every plugin block;
//! renderers use `kind` and `color` / `color_hex` as written, never a
//! guess from the plugin name. Only rigs written before it have none, and
//! only for them may a renderer fall back to guessing.

use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;

/// How the rack draws the blocks of the preset (`display` in the file)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RackDisplay {
    /// By instance id of a plugin block, written as a decimal string like
    /// the keys of `chain.colors` (`"3"`)
    pub blocks: BTreeMap<String, BlockDisplay>,
}

impl RackDisplay {
    /// No block described
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The entry of block `instance_id`
    pub fn block(&self, instance_id: u64) -> Option<&BlockDisplay> {
        self.blocks.get(&instance_id.to_string())
    }
}

/// How the rack draws one plugin block, as the exporting app decided it
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockDisplay {
    /// Shape of the block in the rack: `amp`, `combo`, `cab`, `pedal`,
    /// `space`, `send`, `return`, or `plugin` (a plain card); see
    /// [`BlockKind`]
    pub kind: String,
    /// Family of the plugin in OpenRig: `dynamics`, `distortion`,
    /// `filter`, `modulation`, `delay`, `reverb`, `simulator`, `utility`,
    /// `generator`, `routing`. Absent when the exporting board did not know
    /// it (plugin not installed, or of no known family)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// LV2 class of the plugin by its local name (`CompressorPlugin`,
    /// `ReverbPlugin`…), absent when the plugin has none or is built in
    #[serde(skip_serializing_if = "String::is_empty")]
    pub lv2_class: String,
    /// Name of the colour the block is drawn in: the user's choice, else
    /// its family's (`red`, `orange`, `yellow`, `green`, `teal`, `blue`,
    /// `purple`, `pink`, `silver`)
    pub color: String,
    /// The same colour as `#rrggbb`, lower case
    pub color_hex: String,
}

/// Shape of a block in the rack
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Amp head: a flat box with its control panel
    Amp,
    /// Combo, or amp with its cabinet: an amp with a grille
    Combo,
    /// Speaker cabinet
    Cab,
    /// Stompbox with its footswitch
    Pedal,
    /// Room or reverb: a rounded box
    Space,
    /// Send to an output: a jack plate
    Send,
    /// Return from an input: a jack plate
    Return,
    /// Any other plugin: a plain card
    Plugin,
    /// A shape of a newer app (draw it as [`BlockKind::Plugin`])
    Other(String),
}

impl BlockKind {
    /// The kind written in a rig
    pub fn parse(text: &str) -> Self {
        match text {
            "amp" => Self::Amp,
            "combo" => Self::Combo,
            "cab" => Self::Cab,
            "pedal" => Self::Pedal,
            "space" => Self::Space,
            "send" => Self::Send,
            "return" => Self::Return,
            "plugin" => Self::Plugin,
            other => Self::Other(other.to_string()),
        }
    }

    /// The name written in a rig
    pub fn as_str(&self) -> &str {
        match self {
            Self::Amp => "amp",
            Self::Combo => "combo",
            Self::Cab => "cab",
            Self::Pedal => "pedal",
            Self::Space => "space",
            Self::Send => "send",
            Self::Return => "return",
            Self::Plugin => "plugin",
            Self::Other(other) => other,
        }
    }
}

/// Serialized as its name, `"amp"`
impl Serialize for BlockKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A colour of the rack
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockColor {
    /// Colour name (`red`)
    pub name: String,
    /// `#rrggbb`, lower case
    pub hex: String,
}

/// `#rrggbb`
pub(crate) fn is_hex_color(text: &str) -> bool {
    text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}
