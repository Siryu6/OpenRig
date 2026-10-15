//! Effect chain tree: plugins in series, and splits that route the signal
//! through two branches (A/B, Mix, L/R)

use serde::{Deserialize, Serialize};

/// How a split combines its two branches
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum SplitType {
    /// A/B switch: only one branch is heard
    #[default]
    Ab = 0,
    /// Mix: both branches are blended
    Mix = 1,
    /// L/R: top branch to the left channel, bottom to the right
    Lr = 2,
}

impl SplitType {
    /// Name shown in the rack
    pub fn label(self) -> &'static str {
        match self {
            SplitType::Ab => "A/B split",
            SplitType::Mix => "Mix split",
            SplitType::Lr => "L/R split",
        }
    }
}

/// A block of the effect chain: a plugin or a split
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChainNode {
    /// Single plugin
    Plugin {
        instance_id: usize,
        uri: String,
        enabled: bool,
        /// Audio inputs (1 = mono, 2 = stereo)
        num_inputs: usize,
        /// Audio outputs (1 = mono, 2 = stereo)
        num_outputs: usize,
    },
    /// Signal split into two branches
    Split {
        instance_id: usize,
        split_type: SplitType,
        enabled: bool,
        /// Top branch (A)
        top_chain: Vec<ChainNode>,
        /// Bottom branch (B)
        bottom_chain: Vec<ChainNode>,
        /// A/B: true when the top branch (A) is selected
        ab_select_a: bool,
        /// Mix: 1.0 = all top branch, 0.0 = all bottom branch
        mix_ratio: f32,
        /// Mix in stereo mode: position of the top and bottom branches,
        /// -1.0 = left, 0.0 = centre (both sides), 1.0 = right
        #[serde(default)]
        pan: [f32; 2],
        /// Audio inputs
        num_inputs: usize,
        /// Audio outputs
        num_outputs: usize,
    },
}

impl ChainNode {
    /// Whether this node is a split
    pub fn is_split(&self) -> bool {
        matches!(self, ChainNode::Split { .. })
    }

    /// Whether the node is enabled (not bypassed)
    pub fn is_enabled(&self) -> bool {
        match self {
            ChainNode::Plugin { enabled, .. } => *enabled,
            ChainNode::Split { enabled, .. } => *enabled,
        }
    }
}
