//! The plugin store end to end, without network: a signed index (signed
//! here with a throwaway key the test trusts), an archive already in the
//! cache, install into the LV2 folder, rescan, removal.
//! Skipped when no C compiler or LV2 headers are available.
#![cfg(all(feature = "lv2", feature = "store"))]

use blake2::{Blake2b512, Digest};
use pedalboard::plugins::lv2_host::host_from_path;
use pedalboard::store::{self, verify::TRUSTED_KEYS_FILE, Status, Store};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::fs::{self, File};
use std::path::Path;

mod support;

const URI: &str = "urn:pedalboard:test:store-gain";

const SOURCE: &str = r#"
#include <stdlib.h>
#include <lv2/core/lv2.h>
typedef struct { const float* in; float* out; } Gain;
static LV2_Handle instantiate(const LV2_Descriptor* d, double rate, const char* path,
                              const LV2_Feature* const* features) { return calloc(1, sizeof(Gain)); }
static void connect_port(LV2_Handle h, uint32_t port, void* data) {
    Gain* g = (Gain*)h;
    if (port == 0) g->in = data; else g->out = data;
}
static void run(LV2_Handle h, uint32_t n) {
    Gain* g = (Gain*)h;
    for (uint32_t i = 0; i < n; i++) g->out[i] = g->in[i];
}
static void cleanup(LV2_Handle h) { free(h); }
static const LV2_Descriptor descriptor = {
    "urn:pedalboard:test:store-gain", instantiate, connect_port, NULL, run, NULL, cleanup, NULL
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
"#;

const MANIFEST: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:pedalboard:test:store-gain> a lv2:Plugin ;
    lv2:binary <gain.so> ;
    rdfs:seeAlso <gain.ttl> .
"#;

const TTL: &str = r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
<urn:pedalboard:test:store-gain> a lv2:Plugin, lv2:AmplifierPlugin ;
    doap:name "Store Gain" ;
    lv2:port [
        a lv2:InputPort, lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in" ; lv2:name "In"
    ] , [
        a lv2:OutputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out" ; lv2:name "Out"
    ] .
"#;

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A minisign key pair: (public key line, signer)
fn keys() -> (String, impl Fn(&[u8]) -> String) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let key_id = [7u8; 8];
    let mut public = b"Ed".to_vec();
    public.extend_from_slice(&key_id);
    public.extend_from_slice(pair.public_key().as_ref());
    let sign = move |data: &[u8]| {
        let signature = pair.sign(&Blake2b512::digest(data));
        let mut bin = b"ED".to_vec();
        bin.extend_from_slice(&key_id);
        bin.extend_from_slice(signature.as_ref());
        let comment = "timestamp:0\tfile:catalog-v1.json";
        let mut global = signature.as_ref().to_vec();
        global.extend_from_slice(comment.as_bytes());
        format!(
            "untrusted comment: test\n{}\ntrusted comment: {comment}\n{}\n",
            base64(&bin),
            base64(pair.sign(&global).as_ref())
        )
    };
    (base64(&public), sign)
}

/// `gain-1.0/gain.lv2/…` as a tar.gz
fn archive(bundle: &Path, out: &Path) {
    let gz = flate2::write::GzEncoder::new(File::create(out).unwrap(), Default::default());
    let mut tar = tar::Builder::new(gz);
    tar.append_dir_all("gain-1.0/gain.lv2", bundle).unwrap();
    tar.into_inner().unwrap().finish().unwrap();
}

fn index(serial: u64, sha256: &str, size: u64) -> String {
    format!(
        r#"{{"schema":1,"serial":{serial},"generated":"2026-10-19T00:00:00Z","expires":"2099-01-01T00:00:00Z",
        "packages":[{{"id":"test-gain","name":"Test","version":"1.0","license":"MIT","homepage":"https://example.org",
        "origin":"openrig-build","source":{{"url":"https://example.org/src.tar.gz","sha256":null}},
        "artifacts":{{"{arch}":{{"url":"https://invalid.example/gain.tar.gz","sha256":"{sha256}","size":{size},
        "format":"tar.gz","root":"gain-1.0"}}}},
        "plugins":[{{"id":"store-gain","uri":"{URI}","bundle":"gain.lv2","name":"Store Gain","author":"Test",
        "license":"MIT","family":"Utility","subcategory":"Volume","kind":"pedal","summary":"s","description":"d",
        "required_features":[]}}]}}]}}"#,
        arch = store::arch()
    )
}

#[test]
fn installs_from_a_signed_index_and_rescans() {
    let Some(root) = support::build_bundle("gain", SOURCE, MANIFEST, TTL) else {
        eprintln!("skipped: cannot compile the test plugin (no C compiler or LV2 headers)");
        return;
    };
    fs::remove_file(root.join("gain.lv2/gain.c")).unwrap();
    let data = root.join("data");
    let store = Store::new(&data);
    fs::create_dir_all(store.dir.join("cache")).unwrap();

    // The archive, as the catalog pins it, already downloaded
    let tarball = root.join("gain.tar.gz");
    archive(&root.join("gain.lv2"), &tarball);
    let sha256 = store::fetch::sha256_file(&tarball).unwrap();
    let size = fs::metadata(&tarball).unwrap().len();
    fs::copy(
        &tarball,
        store.dir.join("cache").join(format!("{sha256}.tar.gz")),
    )
    .unwrap();

    // Signed by a key the owner trusts; tampering and replays are refused
    let (public, sign) = keys();
    let text = index(2026101900, &sha256, size);
    assert!(store
        .accept(text.as_bytes(), &sign(text.as_bytes()))
        .is_err());
    fs::write(store.dir.join(TRUSTED_KEYS_FILE), format!("{public}\n")).unwrap();
    let tampered = text.replace("Store Gain", "Evil Gain");
    assert!(store
        .accept(tampered.as_bytes(), &sign(text.as_bytes()))
        .is_err());
    let index = store
        .accept(text.as_bytes(), &sign(text.as_bytes()))
        .unwrap();
    assert_eq!(store.cached().unwrap().serial, 2026101900);
    let older = self::index(2026101800, &sha256, size);
    assert!(store
        .accept(older.as_bytes(), &sign(older.as_bytes()))
        .is_err());

    // Install, then the plugin is found by a rescan
    let host = host_from_path(&support::lv2_path(&store.lv2_dir));
    assert!(host.scanner().find_by_uri(URI).is_none());
    let (package, plugin) = index.find("store-gain").unwrap();
    assert_eq!(
        store::status(package, plugin, &store.installed(), false),
        Status::Available
    );
    store.install(&index, "store-gain", |_, _| {}).unwrap();
    host.rescan();
    assert_eq!(host.scanner().find_by_uri(URI).unwrap().name, "Store Gain");
    assert_eq!(
        store::status(package, plugin, &store.installed(), true),
        Status::Installed
    );
    let mut instance = host.instantiate(URI, 0, 48000.0, 64, None).unwrap();
    let mut buffer = vec![0.25f32; 64];
    instance.process_in_place(&mut buffer);
    assert!(buffer.iter().all(|&s| (s - 0.25).abs() < 1e-6));

    // Removed: gone after the next rescan, while the old instance still runs
    store.remove(&index, "store-gain").unwrap();
    host.rescan();
    assert!(host.scanner().find_by_uri(URI).is_none());
    instance.process_in_place(&mut buffer);
    drop(instance);
    fs::remove_dir_all(root).unwrap();
}
