//! A real LV2 plugin reading its atom input: it outputs the last tempo
//! (time:Position) as a constant signal on the left (once a MIDI note came:
//! the note times the notes received), and the last bar beat on the right,
//! so the transport and MIDI written by the host can be checked end to end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Tests: failing is their job
#![cfg(feature = "lv2")]

use pedalboard::audio::transport::Transport;
use pedalboard::audio::ChainNode;
use pedalboard::midi::RawMidi;
use pedalboard::plugins::chain::no_io;
use pedalboard::plugins::lv2_host::host_from_path;
use pedalboard::plugins::{ChainCommand, ChainSpec, EffectChain};

mod support;

const URI: &str = "urn:pedalboard:test:events";

const SOURCE: &str = r#"
#include <stdlib.h>
#include <string.h>
#include <lv2/core/lv2.h>
#include <lv2/atom/atom.h>
#include <lv2/atom/util.h>
#include <lv2/midi/midi.h>
#include <lv2/time/time.h>
#include <lv2/urid/urid.h>

typedef struct {
    const LV2_Atom_Sequence* events; const float* in; float* out; float* beat;
    LV2_URID object, position, bpm, bar_beat, midi, f;
    float value, bar, note; int notes;
} Events;

static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) {
    LV2_URID_Map* map = NULL;
    for (int i = 0; features[i]; i++)
        if (!strcmp(features[i]->URI, LV2_URID__map)) map = features[i]->data;
    if (!map) return NULL;
    Events* e = calloc(1, sizeof(Events));
    e->object = map->map(map->handle, LV2_ATOM__Object);
    e->position = map->map(map->handle, LV2_TIME__Position);
    e->bpm = map->map(map->handle, LV2_TIME__beatsPerMinute);
    e->bar_beat = map->map(map->handle, LV2_TIME__barBeat);
    e->midi = map->map(map->handle, LV2_MIDI__MidiEvent);
    e->f = map->map(map->handle, LV2_ATOM__Float);
    return e;
}
static void connect_port(LV2_Handle h, uint32_t port, void* data) {
    Events* e = (Events*)h;
    switch (port) {
        case 0: e->events = data; break;
        case 1: e->in = data; break;
        case 2: e->out = data; break;
        case 3: e->beat = data; break;
    }
}
static void run(LV2_Handle h, uint32_t n) {
    Events* e = (Events*)h;
    LV2_ATOM_SEQUENCE_FOREACH(e->events, ev) {
        if (ev->body.type == e->midi) {
            const uint8_t* msg = (const uint8_t*)(ev + 1);
            if ((msg[0] & 0xf0) == 0x90) { e->note = msg[1]; e->notes++; }
        } else if (ev->body.type == e->object) {
            const LV2_Atom_Object* obj = (const LV2_Atom_Object*)&ev->body;
            const LV2_Atom* bpm = NULL;
            const LV2_Atom* bar = NULL;
            if (obj->body.otype == e->position) {
                lv2_atom_object_get(obj, e->bpm, &bpm, e->bar_beat, &bar, 0);
                if (bpm && bpm->type == e->f) e->value = ((const LV2_Atom_Float*)bpm)->body;
                if (bar && bar->type == e->f) e->bar = ((const LV2_Atom_Float*)bar)->body;
            }
        }
    }
    for (uint32_t i = 0; i < n; i++) {
        e->out[i] = e->notes ? e->note * e->notes : e->value;
        e->beat[i] = e->bar;
    }
}
static void cleanup(LV2_Handle h) { free(h); }

static const LV2_Descriptor descriptor = {
    "urn:pedalboard:test:events", instantiate, connect_port, NULL, run, NULL, cleanup, NULL
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:events> a lv2:Plugin ;
    lv2:binary <events.so> ;
    rdfs:seeAlso <events.ttl> .
"#;

const TTL: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix time: <http://lv2plug.in/ns/ext/time#> .
@prefix urid: <http://lv2plug.in/ns/ext/urid#> .
<urn:pedalboard:test:events> a lv2:Plugin, lv2:UtilityPlugin ;
    doap:name "Test Events" ;
    lv2:requiredFeature urid:map ;
    lv2:port [
        a lv2:InputPort, atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent, time:Position ;
        lv2:designation lv2:control ;
        lv2:index 0 ; lv2:symbol "events" ; lv2:name "Events"
    ] , [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out" ; lv2:name "Out"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 3 ; lv2:symbol "beat" ; lv2:name "Beat"
    ] .
"#;

#[test]
fn plugins_receive_the_transport_and_midi() {
    let Some(root) = support::build_bundle("events", SOURCE, MANIFEST, TTL) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    let host = host_from_path(&support::lv2_path(&root));
    let scanner = host.scanner();
    let info = scanner.find_by_uri(URI).expect("test plugin discovered");
    let events = &info.ports[0];
    assert!(events.midi && events.time);

    let spec = ChainSpec {
        generation: 1,
        nodes: vec![ChainNode::Plugin {
            instance_id: 3,
            uri: URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 2,
        }],
        bpm: 120.0,
        stereo: true,
        ..Default::default()
    };
    let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 128);
    assert!(errors.is_empty(), "{:?}", errors);
    // The clock as the audio thread runs it: one period of 12000 frames is
    // a quarter of a second, half a beat at 120
    let mut clock = Transport::new(48000.0);
    clock.set_bpm(chain.bpm());
    let run = |chain: &mut EffectChain, clock: &mut Transport| {
        let (mut left, mut right) = (vec![0.0f32; 12000], vec![0.0f32; 12000]);
        no_io(|io| {
            io.set_start(Some(clock.position()));
            chain.process_io(&mut left, &mut right, io);
        });
        clock.advance(left.len());
        (left[11999], right[11999])
    };
    // The tempo of the spec reaches the plugin on its first run, and the
    // bar beat moves on from one period to the next. The plugin runs in
    // blocks of 128 frames: it reads the position of its last block
    let block = 128.0 * 120.0 / 60.0 / 48000.0;
    let last = |period: f32| period * 0.5 + (11999 / 128) as f32 * block;
    let (tempo, beat) = run(&mut chain, &mut clock);
    assert_eq!(tempo, 120.0);
    assert!((beat - last(0.0)).abs() < 1e-4, "{beat}");
    let (_, beat) = run(&mut chain, &mut clock);
    assert!((beat - last(1.0)).abs() < 1e-4, "{beat}");

    let tempo = ChainCommand::SetTempo { bpm: 90.0 };
    assert!(chain.apply(&tempo));
    clock.set_bpm(90.0);
    assert_eq!(run(&mut chain, &mut clock).0, 90.0);

    chain.send_midi(RawMidi {
        bytes: [0x90, 64, 100],
        len: 3,
    });
    assert_eq!(run(&mut chain, &mut clock).0, 64.0);
    // Events are sent once
    assert_eq!(run(&mut chain, &mut clock).0, 64.0);

    let _ = std::fs::remove_dir_all(root);
}
