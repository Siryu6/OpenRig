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
    pub fn instance_id(&self) -> usize {
        match self {
            ChainNode::Plugin { instance_id, .. } | ChainNode::Split { instance_id, .. } => {
                *instance_id
            }
        }
    }

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

/// Every node of `nodes`, splits included, in processing order: a split,
/// then its top branch, then its bottom branch
pub fn walk(nodes: &[ChainNode]) -> Walk<'_> {
    Walk {
        stack: vec![nodes.iter()],
    }
}

/// Iterator of [`walk`]
pub struct Walk<'a> {
    /// The branches being walked, innermost last
    stack: Vec<std::slice::Iter<'a, ChainNode>>,
}

impl<'a> Iterator for Walk<'a> {
    type Item = &'a ChainNode;

    fn next(&mut self) -> Option<&'a ChainNode> {
        loop {
            let Some(node) = self.stack.last_mut()?.next() else {
                self.stack.pop();
                continue;
            };
            if let ChainNode::Split {
                top_chain,
                bottom_chain,
                ..
            } = node
            {
                self.stack.push(bottom_chain.iter());
                self.stack.push(top_chain.iter());
            }
            return Some(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: usize) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: String::new(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    fn split(id: usize, top: Vec<ChainNode>, bottom: Vec<ChainNode>) -> ChainNode {
        ChainNode::Split {
            instance_id: id,
            split_type: SplitType::Ab,
            enabled: true,
            top_chain: top,
            bottom_chain: bottom,
            ab_select_a: true,
            mix_ratio: 0.5,
            pan: [0.0; 2],
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    #[test]
    fn walk_follows_the_processing_order() {
        let nodes = vec![
            plugin(1),
            split(
                2,
                vec![plugin(3), split(4, vec![], vec![plugin(5)])],
                vec![plugin(6)],
            ),
            split(7, vec![], vec![]),
            plugin(8),
        ];
        let ids: Vec<usize> = walk(&nodes).map(ChainNode::instance_id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(walk(&[]).count(), 0);
        let inside: Vec<usize> = walk(std::slice::from_ref(&nodes[1]))
            .map(ChainNode::instance_id)
            .collect();
        assert_eq!(inside, [2, 3, 4, 5, 6]);
    }
}
