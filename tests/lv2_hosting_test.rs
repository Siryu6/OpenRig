//! Hosts a real LV2 plugin: a tiny gain plugin is compiled with the system
//! C compiler into a temporary bundle, then discovered and run through lilv.
//! Skipped when no C compiler or LV2 headers are available.
#![cfg(feature = "lv2")]

use pedalboard::audio::{ChainNode, SplitType};
use pedalboard::plugins::lv2_host::host_from_path;
use pedalboard::plugins::{ChainCommand, ChainSpec, EffectChain};
use std::collections::HashMap;

mod support;

const URI: &str = "urn:pedalboard:test:gain";

const SOURCE: &str = r#"
#include <stdlib.h>
#include <lv2/core/lv2.h>

typedef struct { const float* gain; const float* in; float* out; float* peak; } Gain;

static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) {
    return calloc(1, sizeof(Gain));
}
static void connect_port(LV2_Handle h, uint32_t port, void* data) {
    Gain* g = (Gain*)h;
    switch (port) {
        case 0: g->gain = data; break;
        case 1: g->in = data; break;
        case 2: g->out = data; break;
        case 3: g->peak = data; break;
    }
}
static void run(LV2_Handle h, uint32_t n) {
    Gain* g = (Gain*)h;
    float peak = 0.0f;
    for (uint32_t i = 0; i < n; i++) {
        g->out[i] = g->in[i] * *g->gain;
        float a = g->out[i] < 0 ? -g->out[i] : g->out[i];
        if (a > peak) peak = a;
    }
    *g->peak = peak;
}
static void cleanup(LV2_Handle h) { free(h); }

static const LV2_Descriptor descriptor = {
    "urn:pedalboard:test:gain", instantiate, connect_port, NULL, run, NULL, cleanup, NULL
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:gain> a lv2:Plugin ;
    lv2:binary <gain.so> ;
    rdfs:seeAlso <gain.ttl> .
<urn:pedalboard:test:gain#quiet> a <http://lv2plug.in/ns/ext/presets#Preset> ;
    lv2:appliesTo <urn:pedalboard:test:gain> ;
    rdfs:seeAlso <presets.ttl> .
"#;

/// A factory preset, with a control value and a file
const PRESETS: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix pset: <http://lv2plug.in/ns/ext/presets#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix state: <http://lv2plug.in/ns/ext/state#> .
<urn:pedalboard:test:gain#quiet> a pset:Preset ;
    lv2:appliesTo <urn:pedalboard:test:gain> ;
    rdfs:label "Quiet room" ;
    lv2:port [ lv2:symbol "gain" ; pset:value 0.25 ] ;
    state:state [ <urn:pedalboard:test:gain#ir> <ir/room.wav> ] .
"#;

const TTL: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix units: <http://lv2plug.in/ns/extensions/units#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:gain#ir> a lv2:Parameter ;
    rdfs:label "Room" ;
    rdfs:range <http://lv2plug.in/ns/ext/atom#Path> ;
    <http://moddevices.com/ns/mod#fileTypes> "ir" .
<urn:pedalboard:test:gain> a lv2:Plugin, lv2:AmplifierPlugin ;
    doap:name "Test Gain" ;
    <http://lv2plug.in/ns/ext/patch#writable> <urn:pedalboard:test:gain#ir> ;
    lv2:port [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 0 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 2.0 ; units:unit units:coef
    ] , [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out" ; lv2:name "Out"
    ] , [
        a lv2:OutputPort, lv2:ControlPort ; lv2:index 3 ; lv2:symbol "peak" ; lv2:name "Peak"
    ] .
"#;

#[test]
fn hosts_a_real_lv2_plugin() {
    let Some(root) = support::build_bundle("gain", SOURCE, MANIFEST, TTL) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    std::fs::write(root.join("gain.lv2/presets.ttl"), PRESETS).unwrap();
    let host = host_from_path(&support::lv2_path(&root));
    let info = host
        .scanner()
        .find_by_uri(URI)
        .expect("test plugin discovered")
        .clone();
    assert_eq!(info.name, "Test Gain");
    assert_eq!(info.default_parameters(), vec![1.0]);

    // Its file parameter, and the preset of its bundle with a file
    let ir = "urn:pedalboard:test:gain#ir";
    assert_eq!(info.file_params.len(), 1);
    assert_eq!(info.file_params[0].uri, ir);
    assert_eq!(info.file_params[0].label, "Room");
    assert_eq!(info.file_params[0].extensions, ["wav", "flac"]);
    assert_eq!(info.presets.len(), 1);
    let preset = &info.presets[0];
    assert_eq!(preset.label, "Quiet room");
    assert_eq!(preset.values, [("gain".to_string(), 0.25)]);
    let room = std::fs::canonicalize(&root)
        .unwrap()
        .join("gain.lv2/ir/room.wav");
    assert_eq!(preset.files, [(ir.to_string(), room.display().to_string())]);

    // Direct instance: default gain, then clamped parameter changes
    let mut plugin = host.instantiate(URI, 0, 48000.0, 64, None).unwrap();
    let mut buffer = vec![0.5f32; 200];
    plugin.process_in_place(&mut buffer);
    assert!(buffer.iter().all(|&s| (s - 0.5).abs() < 1e-6));

    plugin.set_parameter(0, 5.0); // clamped to 2.0
    let mut buffer = vec![0.5f32; 200];
    plugin.process_in_place(&mut buffer);
    assert!(buffer.iter().all(|&s| (s - 1.0).abs() < 1e-6));

    // Through a chain, with saved parameters and live commands
    let spec = ChainSpec {
        generation: 1,
        nodes: vec![ChainNode::Plugin {
            instance_id: 7,
            uri: URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }],
        params: HashMap::from([(7, vec![0.5])]),
        ..Default::default()
    };
    let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 128);
    assert!(errors.is_empty(), "{:?}", errors);
    let process = |chain: &mut EffectChain| {
        let (mut left, mut right) = (vec![1.0f32; 256], vec![0.0f32; 256]);
        chain.process(&mut left, &mut right);
        (left, right)
    };
    let (left, right) = process(&mut chain);
    assert!(left.iter().chain(&right).all(|&s| (s - 0.5).abs() < 1e-6));

    assert!(chain.apply(&ChainCommand::SetParameter {
        instance_id: 7,
        index: 0,
        value: 0.0
    }));
    let (left, right) = process(&mut chain);
    assert!(left.iter().chain(&right).all(|&s| s.abs() < 1e-6));

    // Stereo: the gain on the left side of an L/R split only, then a mono
    // plugin on the stereo signal (gain 2), run once per side
    let gain = |instance_id| ChainNode::Plugin {
        instance_id,
        uri: URI.to_string(),
        enabled: true,
        num_inputs: 1,
        num_outputs: 1,
    };
    let spec = ChainSpec {
        generation: 2,
        nodes: vec![
            ChainNode::Split {
                instance_id: 1,
                split_type: SplitType::Lr,
                enabled: true,
                top_chain: vec![gain(7)],
                bottom_chain: vec![],
                ab_select_a: true,
                mix_ratio: 0.5,
                pan: [0.0, 0.0],
                num_inputs: 1,
                num_outputs: 2,
            },
            gain(8),
        ],
        params: HashMap::from([(7, vec![0.5]), (8, vec![2.0])]),
        stereo: true,
        ..Default::default()
    };
    let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 128);
    assert!(errors.is_empty(), "{:?}", errors);
    assert!(chain.is_stereo());
    let (left, right) = process(&mut chain);
    assert!(left.iter().all(|&s| (s - 1.0).abs() < 1e-6), "{}", left[0]);
    assert!(
        right.iter().all(|&s| (s - 2.0).abs() < 1e-6),
        "{}",
        right[0]
    );

    let _ = std::fs::remove_dir_all(root);
}
