//! Reads the OpenRig FX faceplate terms of a plugin through lilv: a plugin
//! under the OpenRig prefix gets its faceplate and its controls' face order,
//! the same Turtle under another URI gets neither. Skipped when no C
//! compiler or LV2 headers are available.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Tests: failing is their job
#![cfg(feature = "lv2")]

use pedalboard::plugins::faceplate::{Family, Finish, KnobStyle};
use pedalboard::plugins::lv2_host::host_from_path;

mod support;

const TRUSTED: &str = "https://openrig.eu/plugins/test-drive";
const OTHER: &str = "https://example.org/plugins/test-drive";

const SOURCE: &str = r#"
#include <stdlib.h>
#include <lv2/core/lv2.h>
static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) { return calloc(1, 1); }
static void connect_port(LV2_Handle h, uint32_t port, void* data) {}
static void run(LV2_Handle h, uint32_t n) {}
static void cleanup(LV2_Handle h) { free(h); }
static const LV2_Descriptor a = { "https://openrig.eu/plugins/test-drive",
    instantiate, connect_port, NULL, run, NULL, cleanup, NULL };
static const LV2_Descriptor b = { "https://example.org/plugins/test-drive",
    instantiate, connect_port, NULL, run, NULL, cleanup, NULL };
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &a : index == 1 ? &b : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<https://openrig.eu/plugins/test-drive> a lv2:Plugin ;
    lv2:binary <drive.so> ;
    rdfs:seeAlso <drive.ttl> .
<https://example.org/plugins/test-drive> a lv2:Plugin ;
    lv2:binary <drive.so> ;
    rdfs:seeAlso <drive.ttl> .
"#;

/// One description for both URIs: only the prefix differs
fn plugin(uri: &str) -> String {
    format!(
        r##"
<{uri}> a lv2:Plugin, lv2:DistortionPlugin ;
    doap:name "OR Test Drive" ;
    openrig:family openrig:Signature ;
    openrig:faceColor "#2E5A3A" ;
    openrig:knobStyle openrig:Skirted ;
    openrig:finish openrig:Brushed ;
    openrig:wear 0.4 ;
    openrig:plateName "TEST DRIVE" ;
    openrig:tagline "A test circuit." ;
    lv2:port [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 0 ; lv2:symbol "level" ; lv2:name "Level" ;
        lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0 ; openrig:order 2
    ] , [
        a lv2:InputPort, lv2:ControlPort ; lv2:index 1 ; lv2:symbol "drive" ; lv2:name "Drive" ;
        lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0 ; openrig:order 1
    ] , [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 2 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 3 ; lv2:symbol "out" ; lv2:name "Out"
    ] .
"##
    )
}

#[test]
fn reads_faceplates_of_openrig_plugins_only() {
    let ttl = format!(
        "@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n\
         @prefix doap: <http://usefulinc.com/ns/doap#> .\n\
         @prefix openrig: <https://openrig.eu/ns/view#> .\n{}{}",
        plugin(TRUSTED),
        plugin(OTHER)
    );
    let Some(root) = support::build_bundle("drive", SOURCE, MANIFEST, &ttl) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    let host = host_from_path(&support::lv2_path(&root));
    let scanner = host.scanner();

    let info = scanner.find_by_uri(TRUSTED).expect("trusted plugin");
    let face = info.faceplate.as_ref().expect("its faceplate");
    assert_eq!(face.family(), Family::Signature);
    assert_eq!(face.name, "TEST DRIVE");
    assert_eq!(face.tagline, "A test circuit");
    let plate = face.plate.as_ref().expect("its plate");
    assert_eq!(plate.color, 0x2E5A3A);
    assert_eq!(plate.knob_style, KnobStyle::Skirted);
    assert_eq!(plate.finish, Finish::Brushed);
    assert!((plate.wear - 0.4).abs() < 1e-6);
    let order: Vec<_> = info
        .control_inputs()
        .map(|p| (p.symbol.as_str(), p.order))
        .collect();
    assert_eq!(order, [("level", Some(2)), ("drive", Some(1))]);

    let other = scanner.find_by_uri(OTHER).expect("other plugin");
    assert!(other.faceplate.is_none());
    assert!(other.control_inputs().all(|p| p.order.is_none()));

    let _ = std::fs::remove_dir_all(root);
}
