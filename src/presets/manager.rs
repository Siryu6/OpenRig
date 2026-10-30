use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

use super::snapshot::{PluginConfig, Preset};
use super::store;

/// Presets saved in a directory, one file each
pub struct PresetManager {
    presets_dir: PathBuf,
    presets: HashMap<String, Preset>,
    /// File each preset was loaded from or saved to
    files: HashMap<String, PathBuf>,
    current_preset_id: Option<String>,
}

impl PresetManager {
    /// Open (or create) a presets directory and load its presets
    pub fn new(presets_dir: impl Into<PathBuf>) -> Result<Self> {
        let presets_dir = presets_dir.into();
        let mut manager = Self {
            presets: HashMap::new(),
            files: HashMap::new(),
            current_preset_id: None,
            presets_dir,
        };
        for (path, preset) in store::load_dir::<Preset>(&manager.presets_dir)? {
            manager.files.insert(preset.id.clone(), path);
            manager.presets.insert(preset.id.clone(), preset);
        }
        info!(
            "Loaded {} presets from {:?}",
            manager.presets.len(),
            manager.presets_dir
        );
        Ok(manager)
    }

    /// Save a preset to disk (new or updated)
    pub fn save_preset(&mut self, preset: &Preset) -> Result<()> {
        let previous = self.files.get(&preset.id).map(PathBuf::as_path);
        let path = store::save(&self.presets_dir, &preset.id, preset, previous)?;
        info!("Saved preset '{}' to {:?}", preset.name, path);
        self.files.insert(preset.id.clone(), path);
        self.presets.insert(preset.id.clone(), preset.clone());
        Ok(())
    }

    /// Create a preset from plugin configs and make it the current one
    pub fn create_preset(
        &mut self,
        name: String,
        plugins: Vec<PluginConfig>,
        stereo_mode: bool,
    ) -> Result<String> {
        let mut preset = Preset::new(name);
        preset.plugins = plugins;
        preset.stereo_mode = stereo_mode;
        self.add_preset(preset)
    }

    /// Save a new preset and make it the current one
    pub fn add_preset(&mut self, preset: Preset) -> Result<String> {
        let id = preset.id.clone();
        self.save_preset(&preset)?;
        self.current_preset_id = Some(id.clone());
        Ok(id)
    }

    /// Make a preset the current one and return it
    pub fn load_preset(&mut self, id: &str) -> Result<Preset> {
        let preset = self
            .presets
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Preset not found: {}", id))?;
        self.current_preset_id = Some(id.to_string());
        info!("Loaded preset '{}'", preset.name);
        Ok(preset)
    }

    pub fn delete_preset(&mut self, id: &str) -> Result<()> {
        if !self.presets.contains_key(id) {
            return Err(anyhow!("Preset not found: {}", id));
        }
        // The file first: if it cannot be deleted, the preset stays listed
        // rather than coming back at the next start
        let path = self
            .files
            .get(id)
            .cloned()
            .unwrap_or_else(|| store::file_for(&self.presets_dir, id));
        if path.exists() {
            std::fs::remove_file(&path)?;
            if let Some(dir) = path.parent() {
                crate::persist::sync_dir(dir);
            }
        }
        self.files.remove(id);
        let preset = self.presets.remove(id).expect("checked above");
        if self.current_preset_id.as_deref() == Some(id) {
            self.current_preset_id = None;
        }
        info!("Deleted preset '{}'", preset.name);
        Ok(())
    }

    /// All presets, most recently modified first
    pub fn list_presets(&self) -> Vec<&Preset> {
        let mut presets: Vec<_> = self.presets.values().collect();
        presets.sort_by_key(|p| std::cmp::Reverse(p.modified_at));
        presets
    }

    pub fn get(&self, id: &str) -> Option<&Preset> {
        self.presets.get(id)
    }

    pub fn current_preset(&self) -> Option<&Preset> {
        self.current_preset_id
            .as_ref()
            .and_then(|id| self.presets.get(id))
    }
}
