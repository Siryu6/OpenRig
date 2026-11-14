//! A real LV2 echo with an lv2:enabled port: the bypass drives that port,
//! the plugin keeps running bypassed and its echoes ring on. Skipped when
//! no C compiler or LV2 headers are available.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Tests: failing is their job
#![cfg(feature = "lv2")]

use pedalboard::audio::ChainNode;
use pedalboard::plugins::lv2_host::host_from_path;
use pedalboard::plugins::{ChainCommand, ChainSpec, EffectChain};

mod support;

const URI: &str = "urn:pedalboard:test:echo";

/// Echo every 1000 frames, then fed back at 0.9. Enabled, it feeds its input to
/// the echo; disabled, its output is the dry input plus the echoes still
/// ringing. It reports the enabled value it last saw.
const SOURCE: &str = r#"
#include <stdlib.h>
#include <lv2/core/lv2.h>

#define DELAY 1000

typedef struct {
    const float* enabled; const float* level; const float* in; float* out; float* seen;
    float line[DELAY]; unsigned pos;
} Echo;

static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) {
    return calloc(1, sizeof(Echo));
}
static void connect_port(LV2_Handle h, uint32_t port, void* data) {
    Echo* e = (Echo*)h;
    switch (port) {
        case 0: e->enabled = data; break;
        case 1: e->level = data; break;
        case 2: e->in = data; break;
        case 3: e->out = data; break;
        case 4: e->seen = data; break;
    }
}
static void run(LV2_Handle h, uint32_t n) {
    Echo* e = (Echo*)h;
    int on = *e->enabled > 0.5f;
    for (uint32_t i = 0; i < n; i++) {
        float echo = e->line[e->pos];
        e->line[e->pos] = (on ? e->in[i] : 0.0f) + echo * 0.9f;
        e->pos = (e->pos + 1) % DELAY;
        e->out[i] = e->in[i] + echo * *e->level;
    }
    *e->seen = *e->enabled;
}
static void cleanup(LV2_Handle h) { free(h); }

static const LV2_Descriptor descriptor = {
    "urn:pedalboard:test:echo", instantiate, connect_port, NULL, run, NULL, cleanup, NULL
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:echo> a lv2:Plugin ;
    lv2:binary <echo.so> ;
    rdfs:seeAlso <echo.ttl> .
"#;

/// Not a delay class: without lv2:enabled its bypass would cut the echoes
const TTL: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
<urn:pedalboard:test:echo> a lv2:Plugin, lv2:ModulatorPlugin ;
    doap:name "Test Echo" ;
    lv2:port [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 0 ; lv2:symbol "enabled" ; lv2:name "Enabled" ;
        lv2:designation lv2:enabled ; lv2:portProperty lv2:toggled ;
        lv2:default 1 ; lv2:minimum 0 ; lv2:maximum 1
    ] , [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 1 ; lv2:symbol "level" ; lv2:name "Level" ;
        lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 1.0
    ] , [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 2 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 3 ; lv2:symbol "out" ; lv2:name "Out"
    ] , [
        a lv2:OutputPort, lv2:ControlPort ; lv2:index 4 ; lv2:symbol "seen" ; lv2:name "Seen"
    ] .
"#;

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

#[test]
fn a_bypassed_plugin_with_an_enabled_port_plays_its_own_tail() {
    let Some(root) = support::build_bundle("echo", SOURCE, MANIFEST, TTL) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    let host = host_from_path(&support::lv2_path(&root));
    let info = host
        .scanner()
        .find_by_uri(URI)
        .expect("test plugin discovered")
        .clone();

    // The enabled port is not a parameter
    assert_eq!(info.enabled_port(), Some(0));
    assert_eq!(info.default_parameters(), vec![1.0]);
    assert_eq!(info.parameter_index("enabled"), None);
    assert_eq!(info.parameter_index("level"), Some(0));
    assert_eq!(
        info.values_by_symbol(&[0.5]),
        [("level".to_string(), 0.5)].into_iter().collect()
    );
    // Older presets saved it, by symbol or by position: it is skipped
    let saved = [("enabled".to_string(), 0.0), ("level".to_string(), 0.25)]
        .into_iter()
        .collect();
    assert_eq!(info.values_from_saved(None, Some(&saved)), vec![0.25]);
    assert_eq!(info.values_from_saved(Some(&[0.0, 0.75]), None), vec![0.75]);

    // Played, bypassed, then fed silence: the echoes keep coming
    let mut plugin = host.instantiate(URI, 0, 48000.0, 256, None).unwrap();
    let mut note = vec![0.0f32; 1000];
    note[0] = 1.0;
    plugin.process_in_place(&mut note);
    assert_eq!(plugin.controls()[0], 1.0);
    plugin.set_bypass(true);
    assert_eq!(plugin.controls()[0], 0.0);
    let mut tail = vec![0.0f32; 4000];
    plugin.process_in_place(&mut tail);
    assert_eq!(plugin.controls()[4], 0.0, "the plugin ran with enabled = 0");
    // Echoes 1000, 2000 and 3000 frames after the note: 1, 0.9, 0.81
    for (k, echo) in tail.chunks(1000).take(3).enumerate() {
        assert!(
            (peak(echo) - 0.9f32.powi(k as i32)).abs() < 1e-4,
            "echo {k}: {}",
            peak(echo)
        );
    }
    // A new note is not echoed: it plays dry
    let mut dry = vec![0.0f32; 1000];
    dry[500] = 1.0;
    plugin.process_in_place(&mut dry);
    assert!((dry[500] - 1.0).abs() < 1e-6);

    // Stopped at once (by the guard, or built bypassed), it stays stopped
    let mut plugin = host.instantiate(URI, 0, 48000.0, 256, None).unwrap();
    plugin.process_in_place(&mut [0.0f32; 64]);
    plugin.set_bypass_immediate(true);
    plugin.set_bypass(true);
    let mut dry = [0.5f32; 64];
    plugin.process_in_place(&mut dry);
    assert_eq!(plugin.controls()[4], 1.0, "not run since");
    assert!(dry.iter().all(|&s| s == 0.5));

    // Through a chain and the live bypass command, the same
    let spec = ChainSpec {
        generation: 1,
        nodes: vec![ChainNode::Plugin {
            instance_id: 7,
            uri: URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }],
        ..Default::default()
    };
    let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 256);
    assert!(errors.is_empty(), "{:?}", errors);
    let process = |chain: &mut EffectChain, first: f32| {
        let (mut left, mut right) = (vec![0.0f32; 1000], vec![0.0f32; 1000]);
        left[0] = first;
        chain.process(&mut left, &mut right);
        left
    };
    process(&mut chain, 1.0);
    assert!(chain.apply(&ChainCommand::SetEnabled {
        instance_id: 7,
        enabled: false
    }));
    for k in 0..3 {
        let echo = peak(&process(&mut chain, 0.0));
        assert!((echo - 0.9f32.powi(k)).abs() < 1e-4, "echo {k}: {echo}");
    }

    let _ = std::fs::remove_dir_all(root);
}
