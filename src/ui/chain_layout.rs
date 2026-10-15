//! Chain layout: positions of the rack blocks and the lines between them.
//!
//! Coordinates are in layout units: the main line is at y = 0, x = 0 is the
//! left edge of the first block, each block takes one cell. A split takes a
//! cell for its fork icon, the width of its widest branch, and a merge cell.
//! Branches are placed far enough above/below the line for their own nested
//! splits, so nothing overlaps at any depth.
use crate::audio::{ChainNode, SplitType};
use crate::plugins::SignalMap;
use crate::state::ChainPlace;

pub const CELL_WIDTH: f32 = 96.0;
pub const CELL_HEIGHT: f32 = 64.0;
/// Room between the last block and the OUT terminal(s), for the fork to
/// the L and R terminals in stereo mode
pub const OUT_GAP: f32 = CELL_WIDTH / 2.0;

/// A positioned block, ready for the UI
#[derive(Debug, Clone)]
pub struct FlatNode {
    pub instance_id: usize,
    /// Left edge of the block cell
    pub x: f32,
    /// Line the block sits on (centre)
    pub y: f32,
    /// One cell for plugins, the whole fork-to-merge span for splits
    pub width: f32,
    pub height: f32,
    pub name: String,
    pub is_split: bool,
    pub split_type: String,
    pub uri: String,
    pub bypassed: bool,
    pub ab_select_a: bool,
    pub mix_ratio: f32,
    /// Mix split: position of the top and bottom branches
    pub pan: [f32; 2],
}

/// Straight line segment
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSegment {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// Lead to the L/R outputs, only shown in stereo mode
    pub is_stereo: bool,
    /// Only shown in mono mode (lines to the OUT terminal)
    pub is_mono: bool,
    /// Carries a stereo signal (drawn as a double line)
    pub wide: bool,
}

/// Where a dragged block can be dropped: a gap between blocks (or the middle
/// of an empty branch), in layout units
#[derive(Debug, Clone, PartialEq)]
pub struct DropSlot {
    pub place: ChainPlace,
    pub x: f32,
    pub y: f32,
}

/// Full layout of a chain
#[derive(Debug, Clone, Default)]
pub struct ChainLayout {
    pub nodes: Vec<FlatNode>,
    pub connectors: Vec<ConnectorSegment>,
    pub slots: Vec<DropSlot>,
    /// x of the output terminal
    pub width: f32,
    /// Vertical extent of the block lines (min ≤ 0 ≤ max)
    pub min_y: f32,
    pub max_y: f32,
}

/// Rows used above and below a chain's line
fn extent(nodes: &[ChainNode]) -> (u32, u32) {
    nodes
        .iter()
        .map(node_extent)
        .fold((0, 0), |(u, d), (nu, nd)| (u.max(nu), d.max(nd)))
}

fn node_extent(node: &ChainNode) -> (u32, u32) {
    match node {
        ChainNode::Plugin { .. } => (0, 0),
        ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } => {
            let (top_up, top_down) = extent(top_chain);
            let (bottom_up, bottom_down) = extent(bottom_chain);
            (top_down + 1 + top_up, bottom_up + 1 + bottom_down)
        }
    }
}

fn has_branches(top: &[ChainNode], bottom: &[ChainNode]) -> bool {
    !top.is_empty() || !bottom.is_empty()
}

/// Width of a chain, in cells
fn cells(nodes: &[ChainNode]) -> u32 {
    nodes.iter().map(node_cells).sum()
}

fn node_cells(node: &ChainNode) -> u32 {
    match node {
        ChainNode::Plugin { .. } => 1,
        ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } => {
            if has_branches(top_chain, bottom_chain) {
                2 + cells(top_chain).max(cells(bottom_chain))
            } else {
                1
            }
        }
    }
}

type Point = (f32, f32);

fn node_id(node: &ChainNode) -> usize {
    match node {
        ChainNode::Plugin { instance_id, .. } | ChainNode::Split { instance_id, .. } => {
            *instance_id
        }
    }
}

struct Builder<'a> {
    layout: ChainLayout,
    signal: &'a SignalMap,
}

impl Builder<'_> {
    fn line(&mut self, (x1, y1): Point, (x2, y2): Point, wide: bool) {
        if (x1, y1) != (x2, y2) {
            self.layout.connectors.push(ConnectorSegment {
                x1,
                y1,
                x2,
                y2,
                is_stereo: false,
                is_mono: false,
                wide,
            });
        }
    }

    /// Line shown in stereo mode only, towards the L/R terminals
    fn lead(&mut self, (x1, y1): Point, (x2, y2): Point, wide: bool) {
        if (x1, y1) != (x2, y2) {
            self.layout.connectors.push(ConnectorSegment {
                x1,
                y1,
                x2,
                y2,
                is_stereo: true,
                is_mono: false,
                wide,
            });
        }
    }

    fn slot(&mut self, parent: Option<(usize, bool)>, index: usize, x: f32, y: f32) {
        self.layout.slots.push(DropSlot {
            place: ChainPlace { parent, index },
            x,
            y,
        });
    }

    /// Place a chain starting at cell edge `x` on line `y`; returns its
    /// entry and exit points, or None for an empty chain. Lines are wide
    /// where the signal is stereo.
    fn chain(
        &mut self,
        nodes: &[ChainNode],
        parent: Option<(usize, bool)>,
        mut x: f32,
        y: f32,
    ) -> Option<(Point, Point)> {
        let mut ends: Option<(Point, Point)> = None;
        for (index, node) in nodes.iter().enumerate() {
            self.slot(parent, index, x, y);
            let (entry, exit) = self.node(node, x, y);
            x += node_cells(node) as f32 * CELL_WIDTH;
            ends = Some(match ends {
                None => (entry, exit),
                Some((first, previous)) => {
                    let wide = self.signal.output(node_id(&nodes[index - 1]));
                    self.line(previous, entry, wide);
                    (first, exit)
                }
            });
        }
        if !nodes.is_empty() {
            self.slot(parent, nodes.len(), x, y);
        }
        ends
    }

    fn node(&mut self, node: &ChainNode, x: f32, y: f32) -> (Point, Point) {
        self.layout.min_y = self.layout.min_y.min(y);
        self.layout.max_y = self.layout.max_y.max(y);
        let centre = (x + CELL_WIDTH / 2.0, y);
        match node {
            ChainNode::Plugin {
                instance_id,
                uri,
                enabled,
                ..
            } => {
                self.layout.nodes.push(FlatNode {
                    instance_id: *instance_id,
                    x,
                    y,
                    width: CELL_WIDTH,
                    height: CELL_HEIGHT,
                    name: uri
                        .rsplit(['/', '#', ':'])
                        .next()
                        .unwrap_or(uri)
                        .to_string(),
                    is_split: false,
                    split_type: String::new(),
                    uri: uri.clone(),
                    bypassed: !enabled,
                    ab_select_a: true,
                    mix_ratio: 0.5,
                    pan: [0.0, 0.0],
                });
                (centre, centre)
            }
            ChainNode::Split {
                instance_id,
                split_type,
                enabled,
                top_chain,
                bottom_chain,
                ab_select_a,
                mix_ratio,
                pan,
                ..
            } => {
                self.layout.nodes.push(FlatNode {
                    instance_id: *instance_id,
                    x,
                    y,
                    width: node_cells(node) as f32 * CELL_WIDTH,
                    height: CELL_HEIGHT,
                    name: split_type.label().to_string(),
                    is_split: true,
                    split_type: format!("{:?}", split_type).to_lowercase(),
                    uri: String::new(),
                    bypassed: !enabled,
                    ab_select_a: *ab_select_a,
                    mix_ratio: *mix_ratio,
                    pan: *pan,
                });
                if !has_branches(top_chain, bottom_chain) {
                    // Drop above / below an empty split to start a branch
                    for (top, dy) in [(true, -CELL_HEIGHT), (false, CELL_HEIGHT)] {
                        self.slot(Some((*instance_id, top)), 0, centre.0, y + dy);
                    }
                    return (centre, centre);
                }

                let inner_x = x + CELL_WIDTH;
                let inner_cells = cells(top_chain).max(cells(bottom_chain));
                let merge = (
                    inner_x + inner_cells as f32 * CELL_WIDTH + CELL_WIDTH / 2.0,
                    y,
                );
                let branch_in = self.signal.branches(*instance_id);
                let top_y = y - (extent(top_chain).1 + 1) as f32 * CELL_HEIGHT;
                let bottom_y = y + (extent(bottom_chain).0 + 1) as f32 * CELL_HEIGHT;

                for (top, branch, branch_y) in
                    [(true, top_chain, top_y), (false, bottom_chain, bottom_y)]
                {
                    let fork = (centre.0, branch_y);
                    let join = (merge.0, branch_y);
                    self.line(centre, fork, branch_in);
                    // An empty branch carries the dry signal straight through
                    let branch_out = match branch.last() {
                        Some(last) => self.signal.output(node_id(last)),
                        None => branch_in,
                    };
                    match self.chain(branch, Some((*instance_id, top)), inner_x, branch_y) {
                        Some((entry, exit)) => {
                            self.line(fork, entry, branch_in);
                            self.line(exit, join, branch_out);
                        }
                        None => {
                            self.line(fork, join, branch_in);
                            self.slot(
                                Some((*instance_id, top)),
                                0,
                                (fork.0 + join.0) / 2.0,
                                branch_y,
                            );
                        }
                    }
                    // Each side of an L/R split is mono
                    let wide = branch_out && *split_type != SplitType::Lr;
                    self.line(join, merge, wide);
                    self.layout.min_y = self.layout.min_y.min(branch_y);
                    self.layout.max_y = self.layout.max_y.max(branch_y);
                }
                (centre, merge)
            }
        }
    }
}

/// Lay out a chain between the IN terminal (x = -CELL_WIDTH) and the OUT
/// terminal (x = `width`, `OUT_GAP` after the last block), both on the main
/// line, with mono lines only
pub fn layout_chain(nodes: &[ChainNode]) -> ChainLayout {
    layout_chain_with(nodes, &SignalMap::default())
}

/// Lay out a chain, drawing wide lines where `signal` says it is stereo
pub fn layout_chain_with(nodes: &[ChainNode], signal: &SignalMap) -> ChainLayout {
    let mut builder = Builder {
        layout: ChainLayout::default(),
        signal,
    };
    let end = cells(nodes) as f32 * CELL_WIDTH;
    let width = if nodes.is_empty() { end } else { end + OUT_GAP };
    let input = (-CELL_WIDTH, 0.0);
    let output = (width, 0.0);

    let exit = match builder.chain(nodes, None, 0.0, 0.0) {
        Some((entry, exit)) => {
            builder.line(input, entry, false);
            exit
        }
        None => input,
    };
    builder.line(exit, output, false);
    if let Some(line) = builder.layout.connectors.last_mut() {
        line.is_mono = true;
    }

    // Leads to the L/R terminals shown in stereo mode, at right angles
    // from a fork just before the terminals. A final L/R split, or a Mix
    // split panned hard left and right, sends its branches straight to
    // their side instead of merging them.
    let fork = if nodes.is_empty() {
        (width - CELL_WIDTH / 8.0, exit.1)
    } else {
        (end + OUT_GAP / 2.0, exit.1)
    };
    let mut sources = [exit, exit];
    if let Some(ChainNode::Split {
        split_type,
        top_chain,
        bottom_chain,
        pan,
        ..
    }) = nodes.last()
    {
        // Side of the top branch, when the branches go to separate sides
        let top_side = match split_type {
            SplitType::Lr => Some(0),
            SplitType::Mix if signal.chain_output() && pan[0].abs() == 1.0 && pan[1] == -pan[0] => {
                Some(usize::from(pan[0] > 0.0))
            }
            _ => None,
        };
        if let Some(top_side) = top_side.filter(|_| has_branches(top_chain, bottom_chain)) {
            for c in &mut builder.layout.connectors {
                // The two lines joining the branches at the merge point
                if c.x1 == exit.0 && c.x2 == exit.0 && c.y2 == exit.1 && c.y1 != exit.1 {
                    c.is_mono = true;
                    let top = c.y1 < exit.1;
                    sources[if top { top_side } else { 1 - top_side }] = (c.x1, c.y1);
                }
            }
        }
    }
    // A stereo output runs as a double line up to the fork; a mono one parts
    // at the fork with a single line (the same signal on both sides)
    if sources[0] == sources[1] {
        builder.lead(exit, fork, signal.chain_output());
        sources = [fork, fork];
    }
    for (source, y) in sources.into_iter().zip([-CELL_HEIGHT, CELL_HEIGHT]) {
        let turn = (fork.0, source.1);
        builder.lead(source, turn, false);
        builder.lead(turn, (fork.0, y), false);
        builder.lead((fork.0, y), (width, y), false);
    }

    builder.layout.width = width;
    builder.layout
}

/// Drop slots for `dragged`: every slot that would move it, except those
/// inside it (a split cannot go into itself) and the two next to it
pub fn drop_slots(
    layout: &ChainLayout,
    chain: &crate::state::EffectChainState,
    dragged: usize,
) -> Vec<DropSlot> {
    let Some(from) = chain.place_of(dragged) else {
        return Vec::new();
    };
    let mut inside = Vec::new();
    if let Some(node) = chain.find(dragged) {
        collect(std::slice::from_ref(node), &mut inside);
    }
    layout
        .slots
        .iter()
        .filter(|s| {
            !s.place
                .parent
                .is_some_and(|(split, _)| inside.contains(&split))
        })
        .filter(|s| {
            s.place.parent != from.parent
                || (s.place.index != from.index && s.place.index != from.index + 1)
        })
        .cloned()
        .collect()
}

fn collect(nodes: &[ChainNode], out: &mut Vec<usize>) {
    for node in nodes {
        match node {
            ChainNode::Plugin { instance_id, .. } => out.push(*instance_id),
            ChainNode::Split {
                instance_id,
                top_chain,
                bottom_chain,
                ..
            } => {
                out.push(*instance_id);
                collect(top_chain, out);
                collect(bottom_chain, out);
            }
        }
    }
}

/// The slot under a drop at (`x`, `y`) in layout units: the nearest one
/// within half a cell
pub fn slot_at(slots: &[DropSlot], x: f32, y: f32) -> Option<&DropSlot> {
    slots
        .iter()
        .filter(|s| (s.x - x).abs() <= CELL_WIDTH / 2.0 && (s.y - y).abs() <= CELL_HEIGHT / 2.0)
        .min_by(|a, b| {
            let d = |s: &DropSlot| (s.x - x).powi(2) + (s.y - y).powi(2);
            d(a).total_cmp(&d(b))
        })
}

/// Nodes, connectors and output x of a chain
pub fn do_layout(nodes: &[ChainNode]) -> (Vec<FlatNode>, Vec<ConnectorSegment>, f32) {
    let layout = layout_chain(nodes);
    (layout.nodes, layout.connectors, layout.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: usize) -> ChainNode {
        ChainNode::Plugin {
            instance_id: id,
            uri: format!("test:p{id}"),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    fn split(id: usize, top: Vec<ChainNode>, bottom: Vec<ChainNode>) -> ChainNode {
        ChainNode::Split {
            instance_id: id,
            split_type: SplitType::Mix,
            enabled: true,
            top_chain: top,
            bottom_chain: bottom,
            ab_select_a: true,
            mix_ratio: 0.5,
            pan: [0.0, 0.0],
            num_inputs: 1,
            num_outputs: 1,
        }
    }

    fn connected(layout: &ChainLayout, from: Point, to: Point) -> bool {
        layout.connectors.iter().any(|c| {
            !c.is_stereo
                && (((c.x1, c.y1) == from && (c.x2, c.y2) == to)
                    || ((c.x1, c.y1) == to && (c.x2, c.y2) == from))
        })
    }

    fn node(layout: &ChainLayout, id: usize) -> &FlatNode {
        layout.nodes.iter().find(|n| n.instance_id == id).unwrap()
    }

    /// Start of the stereo leads to the L and R terminals
    fn lead_starts(layout: &ChainLayout) -> Vec<Point> {
        let leads = layout.connectors.iter().filter(|c| c.is_stereo);
        leads.map(|c| (c.x1, c.y1)).collect()
    }

    #[test]
    fn final_lr_split_sends_its_branches_to_their_side() {
        let mut lr = split(1, vec![plugin(2)], vec![plugin(3)]);
        if let ChainNode::Split { split_type, .. } = &mut lr {
            *split_type = SplitType::Lr;
        }
        let layout = layout_chain(&[lr]);
        let merge = layout.width - OUT_GAP - CELL_WIDTH / 2.0;
        let starts = lead_starts(&layout);
        assert!(starts.contains(&(merge, -CELL_HEIGHT)), "{starts:?}");
        assert!(starts.contains(&(merge, CELL_HEIGHT)), "{starts:?}");
        assert!(!starts.contains(&(merge, 0.0)), "{starts:?}");
        // The branches only merge in mono mode
        assert!(layout
            .connectors
            .iter()
            .filter(|c| c.y2 == 0.0 && c.x2 == merge)
            .all(|c| c.is_mono));

        // Other splits merge first
        let layout = layout_chain(&[split(1, vec![plugin(2)], vec![plugin(3)])]);
        let starts = lead_starts(&layout);
        assert!(starts.contains(&(merge, 0.0)), "{starts:?}");
        assert!(
            starts.iter().all(|s| s.1 == 0.0 || s.0 > merge),
            "{starts:?}"
        );
    }

    #[test]
    fn merge_point_is_connected_to_the_next_block() {
        // split(top: p1, bottom: p2) → p3: the screenshot case
        let layout = layout_chain(&[split(0, vec![plugin(1)], vec![plugin(2)]), plugin(3)]);
        let merge_x = 2.5 * CELL_WIDTH;
        let next = node(&layout, 3);
        assert_eq!((next.x, next.y), (3.0 * CELL_WIDTH, 0.0));
        assert!(connected(
            &layout,
            (merge_x, 0.0),
            (next.x + CELL_WIDTH / 2.0, 0.0)
        ));
        assert!(connected(&layout, (merge_x, -CELL_HEIGHT), (merge_x, 0.0)));
        assert!(connected(&layout, (merge_x, CELL_HEIGHT), (merge_x, 0.0)));
    }

    #[test]
    fn empty_branch_is_drawn_as_a_dry_line() {
        let layout = layout_chain(&[split(0, vec![plugin(1), plugin(2)], vec![])]);
        let fork_x = CELL_WIDTH / 2.0;
        let merge_x = 3.5 * CELL_WIDTH;
        assert!(connected(
            &layout,
            (fork_x, CELL_HEIGHT),
            (merge_x, CELL_HEIGHT)
        ));
        assert_eq!(layout.width, 4.0 * CELL_WIDTH + OUT_GAP);
    }

    #[test]
    fn nested_splits_do_not_overlap() {
        // The inner split's bottom branch would land on the main line if
        // branches were always one row away
        let inner = split(10, vec![plugin(11)], vec![plugin(12)]);
        let layout = layout_chain(&[split(0, vec![inner], vec![plugin(1)])]);

        let inner_bottom = node(&layout, 12);
        assert!(
            inner_bottom.y < 0.0,
            "inner bottom branch stays above the main line"
        );
        assert_eq!(node(&layout, 10).y, -2.0 * CELL_HEIGHT);
        assert_eq!(node(&layout, 11).y, -3.0 * CELL_HEIGHT);
        assert_eq!(node(&layout, 1).y, CELL_HEIGHT);
        assert_eq!(
            (layout.min_y, layout.max_y),
            (-3.0 * CELL_HEIGHT, CELL_HEIGHT)
        );

        let mut cells: Vec<(i32, i32)> = layout
            .nodes
            .iter()
            .filter(|n| !n.is_split)
            .map(|n| ((n.x / CELL_WIDTH) as i32, (n.y / CELL_HEIGHT) as i32))
            .collect();
        let count = cells.len();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), count, "two blocks share a cell");
    }

    #[test]
    fn every_block_is_reachable_from_the_input() {
        let inner = split(10, vec![], vec![plugin(11), plugin(12)]);
        let layout = layout_chain(&[
            plugin(1),
            split(0, vec![inner, plugin(2)], vec![plugin(3)]),
            plugin(4),
        ]);

        // Walk the connector graph from IN
        let mut reached = vec![(-CELL_WIDTH, 0.0)];
        let mut i = 0;
        while i < reached.len() {
            let p = reached[i];
            for c in layout.connectors.iter().filter(|c| !c.is_stereo) {
                for (a, b) in [((c.x1, c.y1), (c.x2, c.y2)), ((c.x2, c.y2), (c.x1, c.y1))] {
                    if a == p && !reached.contains(&b) {
                        reached.push(b);
                    }
                }
            }
            i += 1;
        }
        for n in layout.nodes.iter() {
            assert!(
                reached.contains(&(n.x + CELL_WIDTH / 2.0, n.y)),
                "block {} not connected",
                n.instance_id
            );
        }
        assert!(
            reached.contains(&(layout.width, 0.0)),
            "output not connected"
        );
    }

    #[test]
    fn drop_slots_move_blocks_anywhere_but_into_themselves() {
        use crate::state::EffectChainState;
        // 1, split 2 (top: 3, bottom: empty), 4
        let chain = EffectChainState {
            nodes: vec![plugin(1), split(2, vec![plugin(3)], vec![]), plugin(4)],
            ..Default::default()
        };
        let layout = layout_chain(&chain.nodes);
        let main = |index| ChainPlace {
            parent: None,
            index,
        };
        let top = |index| ChainPlace {
            parent: Some((2, true)),
            index,
        };
        let bottom = ChainPlace {
            parent: Some((2, false)),
            index: 0,
        };

        let places = |dragged| -> Vec<ChainPlace> {
            drop_slots(&layout, &chain, dragged)
                .iter()
                .map(|s| s.place)
                .collect()
        };
        // Block 1: not next to itself, anywhere else
        assert_eq!(places(1), [top(0), top(1), bottom, main(2), main(3)]);
        // The split cannot go into its own branches
        assert_eq!(places(2), [main(0), main(3)]);

        // The empty branch slot is in the middle of its line, below the split
        let slots = drop_slots(&layout, &chain, 1);
        let slot = slot_at(&slots, 2.5 * CELL_WIDTH, CELL_HEIGHT + 10.0).unwrap();
        assert_eq!(slot.place, bottom);
        assert!(slot_at(&slots, 2.5 * CELL_WIDTH, 3.0 * CELL_HEIGHT).is_none());
    }

    #[test]
    fn stereo_lines_are_wide() {
        // Split 1 outputs stereo, so does the block after it
        let mut signal = SignalMap::default();
        signal.set(1, false, true, false);
        signal.set(2, false, false, false);
        signal.set(4, true, true, false);
        let layout = layout_chain_with(&[split(1, vec![plugin(2)], vec![]), plugin(4)], &signal);
        let wide: Vec<_> = layout.connectors.iter().filter(|c| c.wide).collect();
        // Only the line from the split merge to the next block
        assert_eq!(wide.len(), 1, "{wide:?}");
        assert_eq!(wide[0].y1, 0.0);
        assert_eq!(wide[0].y2, 0.0);
        assert!(layout_chain(&[plugin(4)])
            .connectors
            .iter()
            .all(|c| !c.wide));

        // A stereo output stays a double line before parting to L and R
        signal.set_chain_output(true);
        let layout = layout_chain_with(&[split(1, vec![plugin(2)], vec![]), plugin(4)], &signal);
        let leads: Vec<_> = layout.connectors.iter().filter(|c| c.is_stereo).collect();
        assert!(leads[0].wide && leads[1..].iter().all(|c| !c.wide));
        // Then parts at right angles
        assert!(leads.iter().all(|c| c.x1 == c.x2 || c.y1 == c.y2));
        assert!(leads[1..]
            .iter()
            .any(|c| c.x1 == leads[0].x2 && c.y1 == 0.0));
    }

    #[test]
    fn mix_panned_hard_sends_its_branches_to_their_side() {
        let mut mix = split(1, vec![plugin(2)], vec![plugin(3)]);
        if let ChainNode::Split { pan, .. } = &mut mix {
            *pan = [1.0, -1.0]; // top branch to the right
        }
        let mut signal = SignalMap::default();
        signal.set(1, false, true, false);
        signal.set_chain_output(true);
        let layout = layout_chain_with(&[mix], &signal);
        let merge = layout.width - OUT_GAP - CELL_WIDTH / 2.0;
        let top_y = node(&layout, 2).y;
        let leads: Vec<_> = layout.connectors.iter().filter(|c| c.is_stereo).collect();
        // The top branch line reaches the R terminal
        let from_top = leads
            .iter()
            .find(|c| (c.x1, c.y1) == (merge, top_y))
            .unwrap();
        let down = leads
            .iter()
            .find(|c| c.x1 == from_top.x2 && c.x2 == c.x1 && c.y1 == top_y)
            .unwrap();
        assert_eq!(down.y2, CELL_HEIGHT);
        assert!(leads.iter().all(|c| !c.wide));
    }
}
