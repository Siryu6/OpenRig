use pedalboard::audio::{ChainNode, SplitType};
use pedalboard::ui::chain_layout::{do_layout, CELL_HEIGHT};

#[test]
fn test_empty_chain_has_connector() {
    let nodes = vec![];
    let (flat_nodes, connectors, chain_width) = do_layout(&nodes);
    let connectors: Vec<_> = connectors.into_iter().filter(|c| !c.is_stereo).collect();

    assert_eq!(flat_nodes.len(), 0, "Empty chain should have no nodes");
    assert_eq!(
        connectors.len(),
        1,
        "Empty chain should have IN→OUT connector"
    );
    assert_eq!(
        connectors[0].y1, 0.0,
        "Connector should be on main line Y=0"
    );
    assert_eq!(
        connectors[0].y2, 0.0,
        "Connector should be on main line Y=0"
    );
    assert_eq!(chain_width, 0.0, "Empty chain has no width");
}

#[test]
fn test_single_plugin_on_main_line() {
    let nodes = vec![ChainNode::Plugin {
        instance_id: 1,
        uri: "test:plugin".to_string(),
        enabled: true,
        num_inputs: 2,
        num_outputs: 2,
    }];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(flat_nodes.len(), 1, "Should have 1 node");
    assert_eq!(flat_nodes[0].y, 0.0, "Plugin should be on main line Y=0");
}

#[test]
fn test_two_plugins_on_main_line() {
    let nodes = vec![
        ChainNode::Plugin {
            instance_id: 1,
            uri: "test:plugin1".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        },
        ChainNode::Plugin {
            instance_id: 2,
            uri: "test:plugin2".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        },
    ];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(flat_nodes.len(), 2, "Should have 2 nodes");
    assert_eq!(
        flat_nodes[0].y, 0.0,
        "First plugin should be on main line Y=0"
    );
    assert_eq!(
        flat_nodes[1].y, 0.0,
        "Second plugin should be on main line Y=0"
    );
    assert!(
        flat_nodes[1].x > flat_nodes[0].x,
        "Second plugin should be to the right"
    );
}

#[test]
fn test_split_node_on_main_line() {
    let nodes = vec![ChainNode::Split {
        instance_id: 1,
        split_type: SplitType::Mix,
        enabled: true,
        top_chain: vec![],
        bottom_chain: vec![],
        ab_select_a: true,
        mix_ratio: 0.5,
        pan: [0.0, 0.0],
        num_inputs: 2,
        num_outputs: 2,
    }];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(flat_nodes.len(), 1, "Should have 1 split node");
    assert_eq!(
        flat_nodes[0].y, 0.0,
        "Split node should be on main line Y=0"
    );
    assert!(flat_nodes[0].is_split, "Node should be marked as split");
}

#[test]
fn test_split_with_top_chain_plugin() {
    let nodes = vec![ChainNode::Split {
        instance_id: 1,
        split_type: SplitType::Mix,
        enabled: true,
        top_chain: vec![ChainNode::Plugin {
            instance_id: 2,
            uri: "test:top_plugin".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        }],
        bottom_chain: vec![],
        ab_select_a: true,
        mix_ratio: 0.5,
        pan: [0.0, 0.0],
        num_inputs: 2,
        num_outputs: 2,
    }];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(
        flat_nodes.len(),
        2,
        "Should have split + 1 plugin = 2 nodes"
    );

    // Find split and top plugin
    let split = flat_nodes
        .iter()
        .find(|n| n.is_split)
        .expect("Should have split node");
    let top_plugin = flat_nodes
        .iter()
        .find(|n| !n.is_split)
        .expect("Should have plugin node");

    assert_eq!(split.y, 0.0, "Split should be on main line Y=0");
    assert_eq!(
        top_plugin.y, -CELL_HEIGHT,
        "Top chain plugin should be at Y=-64"
    );
}

#[test]
fn test_split_with_bottom_chain_plugin() {
    let nodes = vec![ChainNode::Split {
        instance_id: 1,
        split_type: SplitType::Mix,
        enabled: true,
        top_chain: vec![],
        bottom_chain: vec![ChainNode::Plugin {
            instance_id: 2,
            uri: "test:bottom_plugin".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        }],
        ab_select_a: true,
        mix_ratio: 0.5,
        pan: [0.0, 0.0],
        num_inputs: 2,
        num_outputs: 2,
    }];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(
        flat_nodes.len(),
        2,
        "Should have split + 1 plugin = 2 nodes"
    );

    let split = flat_nodes
        .iter()
        .find(|n| n.is_split)
        .expect("Should have split node");
    let bottom_plugin = flat_nodes
        .iter()
        .find(|n| !n.is_split)
        .expect("Should have plugin node");

    assert_eq!(split.y, 0.0, "Split should be on main line Y=0");
    assert_eq!(
        bottom_plugin.y, CELL_HEIGHT,
        "Bottom chain plugin should be at Y=+64"
    );
}

#[test]
fn test_split_with_both_chains() {
    let nodes = vec![ChainNode::Split {
        instance_id: 1,
        split_type: SplitType::Mix,
        enabled: true,
        top_chain: vec![ChainNode::Plugin {
            instance_id: 2,
            uri: "test:top_plugin".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        }],
        bottom_chain: vec![ChainNode::Plugin {
            instance_id: 3,
            uri: "test:bottom_plugin".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        }],
        ab_select_a: true,
        mix_ratio: 0.5,
        pan: [0.0, 0.0],
        num_inputs: 2,
        num_outputs: 2,
    }];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(
        flat_nodes.len(),
        3,
        "Should have split + 2 plugins = 3 nodes"
    );

    let split = flat_nodes
        .iter()
        .find(|n| n.is_split)
        .expect("Should have split node");
    let plugins: Vec<_> = flat_nodes.iter().filter(|n| !n.is_split).collect();

    assert_eq!(split.y, 0.0, "Split should be on main line Y=0");

    // One plugin should be above (Y < 0) and one below (Y > 0)
    let has_top = plugins.iter().any(|p| p.y < 0.0);
    let has_bottom = plugins.iter().any(|p| p.y > 0.0);

    assert!(has_top, "Should have a plugin on top line (Y < 0)");
    assert!(has_bottom, "Should have a plugin on bottom line (Y > 0)");
}

#[test]
fn test_plugin_after_split_returns_to_main_line() {
    let nodes = vec![
        ChainNode::Split {
            instance_id: 1,
            split_type: SplitType::Mix,
            enabled: true,
            top_chain: vec![ChainNode::Plugin {
                instance_id: 2,
                uri: "test:top_plugin".to_string(),
                enabled: true,
                num_inputs: 2,
                num_outputs: 2,
            }],
            bottom_chain: vec![ChainNode::Plugin {
                instance_id: 3,
                uri: "test:bottom_plugin".to_string(),
                enabled: true,
                num_inputs: 2,
                num_outputs: 2,
            }],
            ab_select_a: true,
            mix_ratio: 0.5,
            pan: [0.0, 0.0],
            num_inputs: 2,
            num_outputs: 2,
        },
        ChainNode::Plugin {
            instance_id: 4,
            uri: "test:after_split".to_string(),
            enabled: true,
            num_inputs: 2,
            num_outputs: 2,
        },
    ];

    let (flat_nodes, _connectors, _chain_width) = do_layout(&nodes);

    assert_eq!(
        flat_nodes.len(),
        4,
        "Should have split + 3 plugins = 4 nodes"
    );

    // Find the plugin that comes after the split in the main chain
    let after_split_plugin = flat_nodes
        .iter()
        .find(|n| !n.is_split && n.name.contains("after_split"))
        .expect("Should have after-split plugin");

    assert_eq!(
        after_split_plugin.y, 0.0,
        "Plugin after split should return to main line Y=0"
    );
}
