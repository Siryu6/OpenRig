#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Tests: failing is their job
//! Shareable rigs: export resolution (store, built-in, TONE3000, local),
//! import planning, path sanitising, install with partial failure, and the
//! round trip back to a preset.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use pedalboard::audio::{ChainNode, SplitType};
use pedalboard::midi::{MidiAssign, MidiTarget};
use pedalboard::presets::{Preset, PresetChain, SavedMidiAssign, SceneSnapshot};
use pedalboard::rigs::export::{self, Options};
use pedalboard::rigs::format::{Code, Level, Rig, Source};
use pedalboard::rigs::import::{self, Installer, Step, Subject};
use pedalboard::rigs::CatalogPlugin;
use pedalboard::tone_info::{ModelInfo, ToneInfo};

const COMP: &str = "urn:zamaudio:ZamComp";
const FUZZ: &str = "http://example.org/my-fuzz";
const AMP_MODEL: &str = "Tone3000/Amps/Plexi [41]/Crunch [987].nam";
const OLD_CAB: &str = "Tone3000/Cabs/V30 [55]/SM57 [770].wav";
const OWN_IR: &str = "My IRs/greenback.wav";

/// A fresh folder; tests run in parallel, each call gets its own
fn temp(name: &str) -> PathBuf {
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pb-rig-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn plugin(id: usize, uri: &str) -> ChainNode {
    ChainNode::Plugin {
        instance_id: id,
        uri: uri.into(),
        enabled: true,
        num_inputs: 1,
        num_outputs: 1,
    }
}

fn preset() -> Preset {
    let mut chain = PresetChain {
        nodes: vec![
            plugin(1, COMP),
            plugin(2, "urn:pedalboard:nam"),
            ChainNode::Split {
                instance_id: 3,
                split_type: SplitType::Ab,
                enabled: true,
                top_chain: vec![plugin(4, "urn:pedalboard:ir")],
                bottom_chain: vec![plugin(5, "urn:pedalboard:ir"), plugin(6, FUZZ)],
                ab_select_a: true,
                mix_ratio: 0.5,
                pan: [0.0, 0.0],
                num_inputs: 1,
                num_outputs: 1,
            },
        ],
        bpm: 96.0,
        ..PresetChain::default()
    };
    chain.files = HashMap::from([
        (2, AMP_MODEL.to_string()),
        (4, OLD_CAB.to_string()),
        (5, OWN_IR.to_string()),
    ]);
    chain.param_files = HashMap::from([(
        6,
        BTreeMap::from([(
            "http://example.org/my-fuzz#ir".to_string(),
            "/etc/passwd".to_string(),
        )]),
    )]);
    chain.params_by_symbol = HashMap::from([(1, BTreeMap::from([("attack".into(), 3.0)]))]);
    chain.scenes = vec![Some(SceneSnapshot {
        name: "Verse".into(),
        ..SceneSnapshot::default()
    })];
    chain.midi = vec![SavedMidiAssign {
        assign: MidiAssign {
            controller: 11,
            instance_id: 2,
            target: MidiTarget::Parameter(0),
            heel: 0,
            toe: 100,
        },
        symbol: None,
    }];
    let mut preset = Preset::new("Plexi Crunch".into());
    preset.tags = vec!["  Rock ".into(), String::new()];
    preset.chain = Some(chain);
    preset
}

/// The exporting pedalboard's models: a TONE3000 tone with the new
/// tone.json, one with an older tone.json (no model list), and an IR of
/// one's own
fn models_dir() -> PathBuf {
    let dir = temp("models");
    let amp = dir.join("Tone3000/Amps/Plexi [41]");
    fs::create_dir_all(&amp).unwrap();
    fs::write(amp.join("Crunch [987].nam"), "{}").unwrap();
    ToneInfo {
        id: 41,
        title: "Plexi".into(),
        creator: "someone".into(),
        license: Some("CC BY 4.0".into()),
        url: "https://www.tone3000.com/tones/plexi-41".into(),
        gear: "amp".into(),
        format: "nam".into(),
        models: vec![ModelInfo {
            id: 987,
            name: "Crunch".into(),
            file: "Crunch [987].nam".into(),
        }],
        ..ToneInfo::default()
    }
    .write(&amp)
    .unwrap();
    let cab = dir.join("Tone3000/Cabs/V30 [55]");
    fs::create_dir_all(&cab).unwrap();
    fs::write(cab.join("SM57 [770].wav"), "RIFF").unwrap();
    fs::write(
        cab.join("tone.json"),
        r#"{"id": 55, "title": "V30", "creator": "cabguy", "license": null,
            "url": "https://www.tone3000.com/tones/v30-55", "gear": "cab", "format": "ir",
            "source": "Downloaded from TONE3000 (https://www.tone3000.com)"}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("My IRs")).unwrap();
    fs::write(dir.join(OWN_IR), "RIFF").unwrap();
    dir
}

fn catalog() -> Vec<CatalogPlugin> {
    vec![CatalogPlugin {
        uri: COMP.into(),
        package: "zam-plugins".into(),
        version: "4.6".into(),
        installed_version: Some("4.5".into()),
        plugin_id: "zam-comp".into(),
    }]
}

fn exported(options: Options) -> export::Exported {
    let models = models_dir();
    let catalog = catalog();
    let name = |uri: &str| (uri == COMP).then(|| "ZamComp".to_string());
    let context = export::Context {
        models_dir: &models,
        catalog: &catalog,
        plugin_name: &name,
        app_version: "0.11.0",
        created: "2026-11-06T18:30:00Z".into(),
    };
    let result = export::export(&preset(), &context, options).unwrap();
    fs::remove_dir_all(&models).unwrap();
    result
}

#[test]
fn export_resolves_every_source() {
    let exported = exported(Options::default());
    let rig = &exported.rig;
    assert_eq!(rig.meta.name, "Plexi Crunch");
    assert_eq!(rig.meta.tags, ["rock"]);
    assert_eq!(rig.meta.app_version, "0.11.0");

    let deps = &rig.dependencies;
    assert_eq!(
        deps.plugin(COMP).unwrap().source,
        Source::Store {
            package: "zam-plugins".into(),
            version: "4.5".into(),
            plugin_id: "zam-comp".into()
        }
    );
    assert_eq!(
        deps.plugin("urn:pedalboard:nam").unwrap().source,
        Source::Builtin { kind: "nam".into() }
    );
    assert_eq!(deps.plugin("urn:pedalboard:ir").unwrap().name, "Cabinet");
    assert!(matches!(
        deps.plugin(FUZZ).unwrap().source,
        Source::Local { ref what, .. } if what == "plugin"
    ));

    let Source::Tone3000(amp) = &deps.file(AMP_MODEL).unwrap().source else {
        panic!("amp model not from TONE3000");
    };
    assert_eq!((amp.tone_id, amp.model_id), (41, Some(987)));
    assert_eq!(amp.model_name, "Crunch");
    assert_eq!(amp.license.as_deref(), Some("CC BY 4.0"));
    // An older tone.json: the model id comes from the file name
    let Source::Tone3000(cab) = &deps.file(OLD_CAB).unwrap().source else {
        panic!("cab not from TONE3000");
    };
    assert_eq!((cab.tone_id, cab.model_id), (55, Some(770)));
    assert_eq!(cab.creator, "cabguy");
    assert_eq!(
        deps.file(OWN_IR).unwrap().source,
        Source::Local {
            what: "impulse response".into(),
            name: "greenback.wav".into()
        }
    );
    assert_eq!(deps.file(OWN_IR).unwrap().blocks, [5]);

    // The absolute path is left out, the local items keep it unpublishable
    assert_eq!(exported.dropped, ["/etc/passwd"]);
    assert!(rig.file_refs().iter().all(|f| f.path != "/etc/passwd"));
    assert!(exported.report.is_valid(), "{:#?}", exported.report);
    assert!(!exported.report.is_publishable());
    let mut local = exported.local_items();
    local.sort();
    assert_eq!(local, ["greenback.wav", "my-fuzz"]);

    // Learned CCs are left out unless asked
    assert_eq!(rig.chain().midi_mappings, 0);
    assert_eq!(
        exported_with_midi().rig.chain().midi_mappings,
        1,
        "MIDI kept on request"
    );
}

fn exported_with_midi() -> export::Exported {
    exported(Options { include_midi: true })
}

/// Records what it was asked; the cab download fails
#[derive(Default)]
struct FakeInstaller {
    models: PathBuf,
    installed: Vec<String>,
    progress: Vec<(usize, usize)>,
}

impl Installer for FakeInstaller {
    fn install_plugin(&mut self, plugin_id: &str) -> anyhow::Result<()> {
        self.installed.push(plugin_id.into());
        Ok(())
    }

    fn download_model(
        &mut self,
        model: &pedalboard::rigs::format::Tone3000Model,
    ) -> anyhow::Result<PathBuf> {
        if model.gear == "cab" {
            anyhow::bail!("HTTP 503");
        }
        // Where the library puts it (the title changed on TONE3000 since)
        let path = Path::new("Tone3000/Amps/Plexi 1959 [41]/Crunch [987].nam");
        fs::create_dir_all(self.models.join(path.parent().unwrap())).unwrap();
        fs::write(self.models.join(path), "{}").unwrap();
        Ok(path.to_path_buf())
    }

    fn progress(&mut self, done: usize, total: usize, _label: &str) {
        self.progress.push((done, total));
    }
}

#[test]
fn import_plans_installs_and_remaps() {
    let bytes = exported(Options::default()).rig.to_vec_pretty().unwrap();
    let rig = Rig::from_slice(&bytes).unwrap();

    // Another pedalboard: built-in blocks only, the IR of one's own
    // happens to be there too
    let models = temp("import");
    fs::create_dir_all(models.join("My IRs")).unwrap();
    fs::write(models.join(OWN_IR), "RIFF").unwrap();
    let catalog = catalog();
    let installed = |uri: &str| uri.starts_with("urn:pedalboard:");
    let context = import::Context {
        models_dir: &models,
        catalog: &catalog,
        installed: &installed,
        tone3000: true,
    };
    let plan = import::plan(rig, &context).unwrap();
    let step = |label: &str| {
        plan.items
            .iter()
            .find(|i| i.label == label)
            .map(|i| i.step.clone())
            .unwrap()
    };
    assert_eq!(
        step("ZamComp"),
        Step::Store {
            plugin_id: "zam-comp".into()
        }
    );
    assert_eq!(step("Amp"), Step::Ready);
    assert!(matches!(step("my-fuzz"), Step::Missing(_)));
    assert!(matches!(step("Plexi · Crunch"), Step::Tone3000(_)));
    assert!(matches!(step("V30 · SM57"), Step::Tone3000(_)));
    assert_eq!(step("greenback.wav"), Step::Ready);
    assert_eq!(
        (plan.to_install(), plan.to_download(), plan.missing()),
        (1, 2, 1)
    );

    let mut installer = FakeInstaller {
        models: models.clone(),
        ..FakeInstaller::default()
    };
    let outcome = import::install(&plan, &mut installer).unwrap();
    assert_eq!(installer.installed, ["zam-comp"]);
    assert!(outcome.plugins_installed);
    assert_eq!(outcome.done, ["ZamComp", "Plexi · Crunch"]);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].0, "V30 · SM57");
    assert_eq!(outcome.missing.len(), 1);
    assert_eq!(installer.progress.last(), Some(&(3, 3)));

    let preset = outcome.preset;
    assert_eq!(preset.name, "Plexi Crunch");
    assert_ne!(preset.id, preset_id_of_export());
    let chain = preset.chain.unwrap();
    // Remapped to where it was downloaded; the failed one keeps its path
    assert_eq!(
        chain.files[&2],
        "Tone3000/Amps/Plexi 1959 [41]/Crunch [987].nam"
    );
    assert_eq!(chain.files[&4], OLD_CAB);
    assert_eq!(chain.files[&5], OWN_IR);
    assert_eq!(chain.bpm, 96.0);
    assert_eq!(chain.params_by_symbol[&1]["attack"], 3.0);
    assert_eq!(chain.scenes.len(), 1);
    assert_eq!(chain.nodes.len(), 3);
    fs::remove_dir_all(&models).unwrap();
}

fn preset_id_of_export() -> String {
    let rig = exported(Options::default()).rig;
    rig.preset["id"].as_str().unwrap().to_string()
}

#[test]
fn import_finds_models_already_downloaded() {
    let rig = exported(Options::default()).rig;
    let models = temp("again");
    // The cab, where the library would put it for this rig's title
    let cab = models.join("Tone3000/Cabs/V30 [55]");
    fs::create_dir_all(&cab).unwrap();
    fs::write(cab.join("SM57 [770].wav"), "RIFF").unwrap();
    let installed = |_: &str| true;
    let context = import::Context {
        models_dir: &models,
        catalog: &[],
        installed: &installed,
        tone3000: false,
    };
    let plan = import::plan(rig, &context).unwrap();
    let cab = plan
        .items
        .iter()
        .find(|i| {
            i.subject
                == Subject::File {
                    path: OLD_CAB.into(),
                }
        })
        .unwrap();
    assert_eq!(cab.step, Step::Ready);
    // Without TONE3000 in this build, the amp model cannot be had
    assert!(plan
        .items
        .iter()
        .any(|i| matches!(&i.step, Step::Missing(r) if r.contains("TONE3000"))));
    fs::remove_dir_all(&models).unwrap();
}

#[test]
fn import_refuses_unsafe_rigs() {
    let mut rig = exported(Options::default()).rig;
    rig.preset["chain"]["files"]["2"] = serde_json::json!("../../../home/me/.ssh/id_ed25519");
    rig.dependencies.files[0].path = "../../../home/me/.ssh/id_ed25519".into();
    let report = rig.validate();
    assert!(report
        .at_level(Level::Invalid)
        .any(|i| i.code == Code::UnsafePath));
    let installed = |_: &str| true;
    let context = import::Context {
        models_dir: Path::new("/nonexistent"),
        catalog: &[],
        installed: &installed,
        tone3000: true,
    };
    let error = import::plan(rig, &context).unwrap_err();
    assert!(error.to_string().contains("cannot be used"), "{error}");
}

#[test]
fn preset_survives_the_round_trip() {
    // What the app saves is what the crate's view reads
    let rig = exported(Options { include_midi: true }).rig;
    let view = rig.chain();
    assert_eq!(view.preset_name, "Plexi Crunch");
    assert_eq!(view.bpm, 96.0);
    assert_eq!(view.scenes, [Some("Verse".to_string())]);
    let places: Vec<(String, String)> = view
        .plugins()
        .iter()
        .map(|p| (p.location.clone(), p.block.name.clone()))
        .collect();
    assert_eq!(
        places,
        [
            ("1".to_string(), "ZamComp".to_string()),
            ("2".into(), "Amp".into()),
            ("3/A/1".into(), "Cabinet".into()),
            ("3/B/1".into(), "Cabinet".into()),
            ("3/B/2".into(), "my-fuzz".into()),
        ]
    );
    let preset: Preset = serde_json::from_value(rig.preset.clone()).unwrap();
    let chain = preset.chain.unwrap();
    assert_eq!(chain.midi.len(), 1);
    assert_eq!(chain.files.len(), 3);
}
