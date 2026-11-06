use super::*;
use serde_json::json;

const FIXTURE: &str = include_str!("../fixtures/plexi-crunch.openrig");

fn fixture() -> Rig {
    Rig::from_json(FIXTURE).unwrap()
}

fn codes(report: &Report) -> Vec<(Level, Code)> {
    report.issues.iter().map(|i| (i.level, i.code)).collect()
}

#[test]
fn reads_the_fixture() {
    let rig = fixture();
    assert_eq!(rig.format, 1);
    assert_eq!(rig.meta.name, "Plexi Crunch");
    assert_eq!(rig.meta.tags, ["crunch", "rock"]);
    assert_eq!(rig.dependencies.plugins.len(), 4);
    assert!(matches!(
        rig.dependencies.plugin("urn:zamaudio:ZamComp").unwrap().source,
        Source::Store { ref package, .. } if package == "zam-plugins"
    ));
    let Source::Tone3000(model) = &rig.dependencies.files[0].source else {
        panic!("not a TONE3000 model");
    };
    assert_eq!((model.tone_id, model.model_id), (41234, Some(987654)));
    assert!(!rig.dependencies.shareable());
}

#[test]
fn chain_view() {
    let view = fixture().chain();
    assert_eq!(view.preset_name, "Plexi Crunch");
    assert_eq!(view.bpm, 96.0);
    assert_eq!(
        view.scenes,
        [Some("Rhythm".to_string()), None, Some("Lead".to_string())]
    );
    assert_eq!(view.stomps, [Some(1), None, Some(6)]);
    assert_eq!(view.node_count(), 6);
    assert_eq!(view.depth(), 1);
    let placed: Vec<(String, &str, bool)> = view
        .plugins()
        .iter()
        .map(|p| (p.location.clone(), p.block.name.as_str(), p.block.enabled))
        .collect();
    assert_eq!(
        placed,
        [
            ("1".into(), "ZamComp", true),
            ("2".into(), "Amp", true),
            ("3/A/1".into(), "Cabinet", true),
            ("3/B/1".into(), "Cabinet", false),
            ("4".into(), "Space", true),
        ]
    );
    let Block::Split(split) = &view.blocks[2] else {
        panic!("not a split");
    };
    assert_eq!(split.kind, SplitKind::Mix);
    assert_eq!(split.mix, 0.6);
    let amp = view.plugins()[1].block.clone();
    assert_eq!(amp.color.as_deref(), Some("red"));
    assert_eq!(
        amp.file.as_deref(),
        Some("Tone3000/Amps/Plexi 1959 [41234]/Crunch [987654].nam")
    );
    assert_eq!(amp.source.as_ref().map(Source::type_name), Some("builtin"));
}

#[test]
fn the_fixture_is_valid_but_not_publishable() {
    let report = fixture().validate();
    assert!(report.is_valid(), "{report:#?}");
    assert!(!report.is_publishable());
    assert_eq!(
        codes(&report),
        [(Level::Unpublishable, Code::LocalDependency)]
    );
    assert_eq!(report.issues[0].at, "/dependencies/files/2");
}

#[test]
fn round_trip() {
    let rig = fixture();
    let bytes = rig.to_vec_pretty().unwrap();
    assert_eq!(Rig::from_slice(&bytes).unwrap(), rig);
}

#[test]
fn unknown_fields_and_sources_are_tolerated() {
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    value["future_field"] = json!({"x": 1});
    value["meta"]["rating"] = json!(5);
    value["dependencies"]["plugins"][0]["source"] =
        json!({"type": "git", "url": "https://example.org"});
    let rig = Rig::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(rig.dependencies.plugins[0].source, Source::Unknown);
    let report = rig.validate();
    assert!(report.is_valid());
    assert!(codes(&report).contains(&(Level::Unpublishable, Code::UnknownSource)));
}

#[test]
fn refuses_other_formats_and_sizes() {
    assert!(matches!(
        Rig::from_json(r#"{"format": 2, "preset": {}}"#),
        Err(Error::UnsupportedFormat(2))
    ));
    assert!(matches!(
        Rig::from_json(r#"{"format": 0, "preset": {}}"#),
        Err(Error::UnsupportedFormat(0))
    ));
    assert!(matches!(
        Rig::from_json(r#"{"preset": {}}"#),
        Err(Error::NotARig)
    ));
    assert!(matches!(Rig::from_json("not json"), Err(Error::Json(_))));
    let big = vec![b' '; MAX_RIG_BYTES + 1];
    assert!(matches!(Rig::from_slice(&big), Err(Error::TooLarge(_))));
}

#[test]
fn finds_unsafe_paths_and_missing_entries() {
    let mut rig = fixture();
    rig.preset["chain"]["files"]["2"] = json!("../../etc/passwd");
    rig.preset["chain"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"Plugin": {"instance_id": 9, "uri": "urn:other", "enabled": true}}));
    rig.dependencies.files[1].path = "/abs.wav".into();
    let report = rig.validate();
    assert!(!report.is_valid());
    let found = codes(&report);
    assert!(found.contains(&(Level::Invalid, Code::UnsafePath)));
    assert!(found.contains(&(Level::Invalid, Code::MissingDependency)));
    // The amp's model entry is no longer used
    assert!(found.contains(&(Level::Warning, Code::UnusedDependency)));
}

#[test]
fn checks_sources_and_meta() {
    let mut rig = fixture();
    rig.meta.name = " ".into();
    rig.meta.tags = vec![String::new()];
    rig.dependencies.plugins[1].source = Source::Tone3000(Tone3000Model::default());
    rig.dependencies.files[0].source = Source::Builtin { kind: "nam".into() };
    if let Source::Tone3000(model) = &mut rig.dependencies.files[1].source {
        model.model_id = None;
    }
    let found = codes(&rig.validate());
    assert!(found.contains(&(Level::Invalid, Code::MissingName)));
    assert!(found.contains(&(Level::Invalid, Code::TooLong)));
    assert!(found.contains(&(Level::Invalid, Code::BadSource)));
    assert!(found.contains(&(Level::Warning, Code::MissingModelId)));
}

#[test]
fn limits_blocks_and_depth() {
    let mut node = json!({"Plugin": {"instance_id": 0, "uri": "urn:pedalboard:nam"}});
    for i in 1..=limits::DEPTH + 1 {
        node = json!({"Split": {"instance_id": i, "split_type": "Ab", "top_chain": [node], "bottom_chain": []}});
    }
    let preset = json!({"name": "Deep", "chain": {"nodes": [node]}});
    let deps = Dependencies {
        plugins: vec![PluginDependency {
            uri: "urn:pedalboard:nam".into(),
            name: "Amp".into(),
            source: Source::Builtin { kind: "nam".into() },
        }],
        files: vec![],
    };
    let rig = Rig::new(Meta::named("Deep"), preset, deps.clone());
    assert_eq!(rig.chain().depth(), limits::DEPTH + 1);
    assert!(codes(&rig.validate()).contains(&(Level::Invalid, Code::TooMany)));

    let nodes: Vec<_> = (0..=limits::BLOCKS)
        .map(|i| json!({"Plugin": {"instance_id": i, "uri": "urn:pedalboard:nam"}}))
        .collect();
    let rig = Rig::new(
        Meta::named("Long"),
        json!({"chain": {"nodes": nodes}}),
        deps,
    );
    assert!(codes(&rig.validate()).contains(&(Level::Invalid, Code::TooMany)));
}

#[test]
fn reads_odd_presets_without_failing() {
    let rig = Rig::new(Meta::named("Odd"), json!([1, 2]), Dependencies::default());
    assert!(rig.chain().blocks.is_empty());
    assert!(!rig.validate().is_valid());

    let rig = Rig::new(
        Meta::named("Future"),
        json!({"chain": {"nodes": [{"Matrix": {}}], "bpm": "fast"}}),
        Dependencies::default(),
    );
    let view = rig.chain();
    assert_eq!(view.blocks, [Block::Unknown]);
    assert_eq!(view.bpm, 120.0);
    assert!(codes(&rig.validate()).contains(&(Level::Invalid, Code::InvalidPreset)));

    // First presets: a flat list, no chain
    let rig = Rig::new(
        Meta::named("Old"),
        json!({"plugins": [{"uri": "urn:a", "name": "A", "bypassed": true}]}),
        Dependencies::default(),
    );
    let view = rig.chain();
    assert_eq!(view.plugins().len(), 1);
    assert_eq!(view.plugins()[0].block.name, "A");
    assert!(!view.plugins()[0].block.enabled);
}

#[test]
fn file_refs_cover_models_and_plugin_files() {
    let rig = Rig::new(
        Meta::named("Files"),
        json!({"chain": {
            "files": {"1": "a.nam"},
            "param_files": {"2": {"urn:p#model": "b/c.json"}}
        }}),
        Dependencies::default(),
    );
    assert_eq!(
        rig.file_refs(),
        [
            FileRef {
                instance_id: 1,
                param: None,
                path: "a.nam".into()
            },
            FileRef {
                instance_id: 2,
                param: Some("urn:p#model".into()),
                path: "b/c.json".into()
            },
        ]
    );
}

#[test]
fn source_json_shape() {
    let source = Source::Store {
        package: "zam-plugins".into(),
        version: "4.5".into(),
        plugin_id: String::new(),
    };
    assert_eq!(
        serde_json::to_value(&source).unwrap(),
        json!({"type": "store", "package": "zam-plugins", "version": "4.5"})
    );
    let local = Source::Local {
        what: "plugin".into(),
        name: "My fuzz".into(),
    };
    assert_eq!(
        serde_json::to_value(&local).unwrap(),
        json!({"type": "local", "what": "plugin", "name": "My fuzz"})
    );
}
