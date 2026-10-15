use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

use super::{store, Preset};

/// Setlist (named Scene for file compatibility): up to 8 preset slots,
/// preloaded so that changing song is a crossfade with no disk access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,

    /// Preset IDs in this scene (max 8 for 8 footswitches)
    pub preset_slots: Vec<Option<String>>,
}

impl Scene {
    pub fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            description: String::new(),
            preset_slots: vec![None; 8], // 8 slots for 8 footswitches
        }
    }

    pub fn set_preset_slot(&mut self, slot: usize, preset_id: Option<String>) -> Result<()> {
        if slot >= self.preset_slots.len() {
            return Err(anyhow::anyhow!("Slot {} out of range", slot));
        }
        self.preset_slots[slot] = preset_id;
        Ok(())
    }

    pub fn get_preset_slot(&self, slot: usize) -> Option<&String> {
        self.preset_slots.get(slot).and_then(|opt| opt.as_ref())
    }
}

/// SceneManager - Manages scenes and handles preset preloading
pub struct SceneManager {
    scenes_dir: PathBuf,
    scenes: HashMap<String, Scene>,
    /// File each scene was loaded from or saved to
    files: HashMap<String, PathBuf>,
    current_scene_id: Option<String>,
    /// Slot last switched to in the current scene
    active_slot: Option<usize>,

    /// Preloaded preset states for active scene
    /// This allows instant switching without loading from disk
    preloaded_presets: HashMap<String, Preset>,
}

impl SceneManager {
    /// Open (or create) a scenes directory and load its scenes
    pub fn new(scenes_dir: impl Into<PathBuf>) -> Result<Self> {
        let scenes_dir = scenes_dir.into();
        let mut manager = Self {
            scenes: HashMap::new(),
            files: HashMap::new(),
            current_scene_id: None,
            active_slot: None,
            preloaded_presets: HashMap::new(),
            scenes_dir,
        };
        for (path, scene) in store::load_dir::<Scene>(&manager.scenes_dir)? {
            manager.files.insert(scene.id.clone(), path);
            manager.scenes.insert(scene.id.clone(), scene);
        }
        info!(
            "Loaded {} scenes from {:?}",
            manager.scenes.len(),
            manager.scenes_dir
        );
        Ok(manager)
    }

    /// Save a scene to disk (new or updated)
    pub fn save_scene(&mut self, scene: &Scene) -> Result<()> {
        let previous = self.files.get(&scene.id).map(PathBuf::as_path);
        let path = store::save(&self.scenes_dir, &scene.id, scene, previous)?;
        info!("Saved scene '{}' to {:?}", scene.name, path);
        self.files.insert(scene.id.clone(), path);
        self.scenes.insert(scene.id.clone(), scene.clone());
        Ok(())
    }

    /// Create new scene
    pub fn create_scene(&mut self, name: String) -> Result<String> {
        let scene = Scene::new(name);
        let id = scene.id.clone();

        self.save_scene(&scene)?;

        Ok(id)
    }

    /// Delete scene
    pub fn delete_scene(&mut self, id: &str) -> Result<()> {
        let scene = self
            .scenes
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Scene not found: {}", id))?;

        let scene_name = scene.name.clone();
        let path = self
            .files
            .remove(id)
            .unwrap_or_else(|| store::file_for(&self.scenes_dir, id));
        if path.exists() {
            fs::remove_file(&path).context("Failed to delete scene file")?;
        }

        self.scenes.remove(id);

        if self.current_scene_id.as_ref() == Some(&id.to_string()) {
            self.current_scene_id = None;
            self.preloaded_presets.clear();
        }

        info!("Deleted scene '{}'", scene_name);
        Ok(())
    }

    /// Get all scenes
    pub fn list_scenes(&self) -> Vec<&Scene> {
        self.scenes.values().collect()
    }

    /// Get current scene
    pub fn current_scene(&self) -> Option<&Scene> {
        self.current_scene_id
            .as_ref()
            .and_then(|id| self.scenes.get(id))
    }

    /// Load scene and preload all its presets
    /// This is the key feature: all presets in the scene are loaded into memory
    pub fn load_scene(
        &mut self,
        id: &str,
        preset_loader: impl Fn(&str) -> Result<Preset>,
    ) -> Result<()> {
        let scene = self
            .scenes
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Scene not found: {}", id))?;

        info!(
            "Loading scene '{}' with {} slots",
            scene.name,
            scene.preset_slots.len()
        );

        // Preload all presets in the scene
        self.preloaded_presets.clear();
        let mut loaded_count = 0;

        for slot_preset_id in scene.preset_slots.iter().flatten() {
            match preset_loader(slot_preset_id) {
                Ok(preset) => {
                    self.preloaded_presets.insert(preset.id.clone(), preset);
                    loaded_count += 1;
                }
                Err(e) => {
                    warn!("Failed to preload preset {}: {}", slot_preset_id, e);
                }
            }
        }

        self.current_scene_id = Some(id.to_string());
        self.active_slot = None;

        info!(
            "Scene '{}' loaded with {}/{} presets preloaded",
            scene.name,
            loaded_count,
            scene.preset_slots.iter().flatten().count()
        );

        Ok(())
    }

    /// Leave the current scene and free its preloaded presets
    pub fn close_scene(&mut self) {
        self.current_scene_id = None;
        self.active_slot = None;
        self.preloaded_presets.clear();
    }

    /// Slot last switched to in the current scene
    pub fn active_slot(&self) -> Option<usize> {
        self.active_slot
    }

    /// Switch to a preset slot in the current scene (instant, no disk I/O)
    pub fn switch_to_slot(&mut self, slot: usize) -> Result<Option<Preset>> {
        let scene = self
            .current_scene()
            .ok_or_else(|| anyhow::anyhow!("No scene loaded"))?;

        if slot >= scene.preset_slots.len() {
            return Err(anyhow::anyhow!("Slot {} out of range", slot));
        }

        match scene.get_preset_slot(slot) {
            Some(preset_id) => {
                let preset = self
                    .preloaded_presets
                    .get(preset_id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Preset not preloaded: {}", preset_id))?;

                info!("Switched to slot {} (preset '{}')", slot, preset.name);
                self.active_slot = Some(slot);
                Ok(Some(preset))
            }
            None => {
                info!("Slot {} is empty", slot);
                Ok(None)
            }
        }
    }

    /// A preset was deleted: its preloaded copy no longer plays
    pub fn forget_preset(&mut self, id: &str) -> Result<()> {
        self.preloaded_presets.remove(id);
        // Empty its slots in every setlist, so Next / Previous never land
        // on it
        let mut changed: Vec<Scene> = self
            .scenes
            .values()
            .filter(|s| s.preset_slots.iter().flatten().any(|p| p == id))
            .cloned()
            .collect();
        for scene in &mut changed {
            for slot in &mut scene.preset_slots {
                if slot.as_deref() == Some(id) {
                    *slot = None;
                }
            }
            self.save_scene(scene)?;
        }
        Ok(())
    }

    /// A preset was saved again: setlists play the new version
    pub fn refresh_preset(&mut self, preset: &Preset) {
        if let Some(copy) = self.preloaded_presets.get_mut(&preset.id) {
            *copy = preset.clone();
        }
    }

    /// Slots of the current setlist that can be played: assigned to a
    /// preset that is loaded
    pub fn playable_slots(&self) -> Vec<usize> {
        let Some(scene) = self.current_scene() else {
            return Vec::new();
        };
        scene
            .preset_slots
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.as_ref()
                    .is_some_and(|id| self.preloaded_presets.contains_key(id))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Update scene slot
    pub fn update_scene_slot(
        &mut self,
        scene_id: &str,
        slot: usize,
        preset_id: Option<String>,
    ) -> Result<()> {
        let mut scene = self
            .scenes
            .get(scene_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Scene not found: {}", scene_id))?;

        scene.set_preset_slot(slot, preset_id)?;
        self.save_scene(&scene)?;

        Ok(())
    }
}
