//! A real LV2 plugin requiring the Worker (`work:schedule`), as TooB ML or
//! the Dragonfly reverbs do: when its Level control changes, `run` schedules
//! a job, `work` (worker thread) answers with the level doubled, and
//! `work_response` makes it the constant output. `end_run` must follow.
#![cfg(feature = "lv2")]

use std::time::{Duration, Instant};

use pedalboard::audio::ChainNode;
use pedalboard::plugins::lv2_host::host_from_path;
use pedalboard::plugins::{ChainCommand, ChainSpec, EffectChain};

mod support;

const URI: &str = "urn:pedalboard:test:worker";

const SOURCE: &str = r#"
#include <stdlib.h>
#include <string.h>
#include <lv2/core/lv2.h>
#include <lv2/worker/worker.h>

typedef struct {
    const float* level; const float* in; float* out;
    LV2_Worker_Schedule* schedule;
    float scheduled, value;
    int responses, end_runs;
} Worker;

static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) {
    LV2_Worker_Schedule* schedule = NULL;
    for (int i = 0; features[i]; i++)
        if (!strcmp(features[i]->URI, LV2_WORKER__schedule)) schedule = features[i]->data;
    if (!schedule) return NULL;
    Worker* w = calloc(1, sizeof(Worker));
    w->schedule = schedule;
    w->scheduled = -1.0f;
    return w;
}
static void connect_port(LV2_Handle h, uint32_t port, void* data) {
    Worker* w = (Worker*)h;
    switch (port) {
        case 0: w->level = data; break;
        case 1: w->in = data; break;
        case 2: w->out = data; break;
    }
}
static void run(LV2_Handle h, uint32_t n) {
    Worker* w = (Worker*)h;
    if (*w->level != w->scheduled) {
        float level = *w->level;
        if (w->schedule->schedule_work(w->schedule->handle, sizeof(float), &level)
                == LV2_WORKER_SUCCESS)
            w->scheduled = level;
    }
    /* end_run must have closed every previous cycle */
    for (uint32_t i = 0; i < n; i++) w->out[i] = w->end_runs > 0 ? w->value : -1.0f;
}
static LV2_Worker_Status work(LV2_Handle h, LV2_Worker_Respond_Function respond,
                              LV2_Worker_Respond_Handle handle, uint32_t size, const void* data) {
    if (size != sizeof(float)) return LV2_WORKER_ERR_UNKNOWN;
    float doubled = *(const float*)data * 2.0f;
    return respond(handle, sizeof(float), &doubled);
}
static LV2_Worker_Status work_response(LV2_Handle h, uint32_t size, const void* data) {
    Worker* w = (Worker*)h;
    w->value = *(const float*)data;
    w->responses++;
    return LV2_WORKER_SUCCESS;
}
static LV2_Worker_Status end_run(LV2_Handle h) {
    ((Worker*)h)->end_runs++;
    return LV2_WORKER_SUCCESS;
}
static const void* extension_data(const char* uri) {
    static const LV2_Worker_Interface worker = { work, work_response, end_run };
    return strcmp(uri, LV2_WORKER__interface) ? NULL : &worker;
}
static void cleanup(LV2_Handle h) { free(h); }

static const LV2_Descriptor descriptor = {
    "urn:pedalboard:test:worker", instantiate, connect_port, NULL, run, NULL, cleanup,
    extension_data
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:worker> a lv2:Plugin ;
    lv2:binary <worker.so> ;
    rdfs:seeAlso <worker.ttl> .
"#;

const TTL: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix work: <http://lv2plug.in/ns/ext/worker#> .
<urn:pedalboard:test:worker> a lv2:Plugin, lv2:UtilityPlugin ;
    doap:name "Test Worker" ;
    lv2:requiredFeature work:schedule ;
    lv2:extensionData work:interface ;
    lv2:port [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 0 ; lv2:symbol "level" ; lv2:name "Level" ;
        lv2:default 3.0 ; lv2:minimum 0.0 ; lv2:maximum 10.0
    ] , [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out" ; lv2:name "Out"
    ] .
"#;

/// Run the chain until it outputs `expected` (the worker is asynchronous)
fn wait_for(chain: &mut EffectChain, expected: f32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = f32::NAN;
    while Instant::now() < deadline {
        let (mut left, mut right) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        chain.process(&mut left, &mut right);
        last = left[63];
        if last == expected {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("output {last}, expected {expected}");
}

#[test]
fn plugins_requiring_the_worker_run_their_jobs() {
    let Some(root) = support::build_bundle("worker", SOURCE, MANIFEST, TTL) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    let host = host_from_path(&support::lv2_path(&root));
    // Skipped by the scan before the host provided work:schedule
    assert!(
        host.scanner().find_by_uri(URI).is_some(),
        "test plugin discovered"
    );

    let spec = ChainSpec {
        generation: 1,
        nodes: vec![ChainNode::Plugin {
            instance_id: 5,
            uri: URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }],
        ..Default::default()
    };
    let (mut chain, errors) = EffectChain::build(&spec, &host, 48000.0, 128);
    assert!(errors.is_empty(), "{:?}", errors);
    // The default level, doubled by the worker
    wait_for(&mut chain, 6.0);

    assert!(chain.apply(&ChainCommand::SetParameter {
        instance_id: 5,
        index: 0,
        value: 4.5
    }));
    wait_for(&mut chain, 9.0);

    // Dropping the chain stops the worker before freeing the plugin
    drop(chain);
    let _ = std::fs::remove_dir_all(root);
}
