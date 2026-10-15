// Integration test for preset save/load functionality
use std::fs;

// Import necessary types from the main crate
use pedalboard::presets::{PluginConfig, PresetManager, SceneManager};

#[test]
fn test_preset_creation_and_persistence() {
    // Setup test directory
    let test_dir = std::env::temp_dir().join(format!(
        "pedalboard-test_presets_integration-{}",
        std::process::id()
    ));
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).unwrap();
    }
    fs::create_dir_all(&test_dir).unwrap();

    // Create preset manager
    let mut manager = PresetManager::new(&test_dir).expect("Failed to create preset manager");

    // Create a test preset with plugins
    let plugins = vec![
        PluginConfig::new(
            "http://guitarix.org/plugins/gx_ts9".to_string(),
            "TubeScreamer".to_string(),
        ),
        PluginConfig::new(
            "http://calf.sourceforge.net/plugins/Reverb".to_string(),
            "Calf Reverb".to_string(),
        ),
    ];

    // Save the preset
    let preset_id = manager
        .create_preset("Test Preset".to_string(), plugins, false)
        .expect("Failed to create preset");

    // Verify UUID format (should be 36 chars with dashes)
    assert_eq!(preset_id.len(), 36, "UUID should be 36 characters");
    assert!(preset_id.contains('-'), "UUID should contain dashes");

    // Verify file was created
    let files: Vec<_> = fs::read_dir(&test_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();

    assert_eq!(files.len(), 1, "Should have exactly one preset file");

    // Verify file contains valid JSON
    let preset_file = files[0].path();
    let json_content = fs::read_to_string(&preset_file).expect("Failed to read preset file");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_content).expect("Invalid JSON in preset file");

    // Verify JSON structure
    assert_eq!(parsed["id"].as_str().unwrap(), preset_id);
    assert_eq!(parsed["name"].as_str().unwrap(), "Test Preset");
    assert_eq!(parsed["plugins"].as_array().unwrap().len(), 2);
    assert!(!parsed["stereo_mode"].as_bool().unwrap());

    // Test loading the preset back
    let loaded_preset = manager
        .load_preset(&preset_id)
        .expect("Failed to load preset");
    assert_eq!(loaded_preset.id, preset_id);
    assert_eq!(loaded_preset.name, "Test Preset");
    assert_eq!(loaded_preset.plugins.len(), 2);
    assert_eq!(loaded_preset.plugins[0].name, "TubeScreamer");
    assert_eq!(loaded_preset.plugins[1].name, "Calf Reverb");

    // Test listing presets
    let presets = manager.list_presets();
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].id, preset_id);

    // Test deleting preset
    manager
        .delete_preset(&preset_id)
        .expect("Failed to delete preset");
    let files_after_delete: Vec<_> = fs::read_dir(&test_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files_after_delete.len(), 0, "File should be deleted");

    // Cleanup
    fs::remove_dir_all(&test_dir).unwrap();
}

#[test]
fn test_setlist_system() {
    // Setup test directories
    let scenes_dir = std::env::temp_dir().join(format!(
        "pedalboard-test_scenes_integration-{}",
        std::process::id()
    ));
    let presets_dir = std::env::temp_dir().join(format!(
        "pedalboard-test_presets_for_scenes-{}",
        std::process::id()
    ));

    if scenes_dir.exists() {
        fs::remove_dir_all(&scenes_dir).unwrap();
    }
    if presets_dir.exists() {
        fs::remove_dir_all(&presets_dir).unwrap();
    }
    fs::create_dir_all(&scenes_dir).unwrap();
    fs::create_dir_all(&presets_dir).unwrap();

    // Create some test presets first
    let mut preset_manager =
        PresetManager::new(&presets_dir).expect("Failed to create preset manager");

    let preset1_id = preset_manager
        .create_preset(
            "Clean Tone".to_string(),
            vec![PluginConfig::new(
                "plugin1".to_string(),
                "Reverb".to_string(),
            )],
            false,
        )
        .expect("Failed to create preset 1");

    let preset2_id = preset_manager
        .create_preset(
            "Distortion".to_string(),
            vec![PluginConfig::new(
                "plugin2".to_string(),
                "Distortion".to_string(),
            )],
            false,
        )
        .expect("Failed to create preset 2");

    // Create scene manager
    let mut scene_manager = SceneManager::new(&scenes_dir).expect("Failed to create scene manager");

    // Create a scene
    let scene_id = scene_manager
        .create_scene("Live Set 1".to_string())
        .expect("Failed to create scene");

    assert_eq!(scene_id.len(), 36, "Scene ID should be a UUID");

    // Assign presets to scene slots
    scene_manager
        .update_scene_slot(&scene_id, 0, Some(preset1_id.clone()))
        .expect("Failed to update slot 0");

    scene_manager
        .update_scene_slot(&scene_id, 1, Some(preset2_id.clone()))
        .expect("Failed to update slot 1");

    // Verify scene file was created
    let scene_files: Vec<_> = fs::read_dir(&scenes_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(scene_files.len(), 1, "Should have exactly one scene file");

    // Load the scene with preset preloading
    let preset_map: std::collections::HashMap<String, _> = preset_manager
        .list_presets()
        .into_iter()
        .map(|p| (p.id.clone(), p.clone()))
        .collect();

    let preset_loader = |preset_id: &str| -> anyhow::Result<_> {
        preset_map
            .get(preset_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Preset not found"))
    };

    scene_manager
        .load_scene(&scene_id, preset_loader)
        .expect("Failed to load scene");

    // Test instant switching between slots
    let preset_from_slot_0 = scene_manager
        .switch_to_slot(0)
        .expect("Failed to switch to slot 0")
        .expect("Slot 0 should have a preset");

    assert_eq!(preset_from_slot_0.id, preset1_id);
    assert_eq!(preset_from_slot_0.name, "Clean Tone");

    let preset_from_slot_1 = scene_manager
        .switch_to_slot(1)
        .expect("Failed to switch to slot 1")
        .expect("Slot 1 should have a preset");

    assert_eq!(preset_from_slot_1.id, preset2_id);
    assert_eq!(preset_from_slot_1.name, "Distortion");

    // Test switching to empty slot
    let empty_slot = scene_manager
        .switch_to_slot(2)
        .expect("Failed to switch to slot 2");
    assert!(empty_slot.is_none(), "Slot 2 should be empty");

    // Cleanup
    fs::remove_dir_all(&scenes_dir).unwrap();
    fs::remove_dir_all(&presets_dir).unwrap();
}
