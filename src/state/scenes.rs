//! Scenes within a preset (Helix "snapshots", Quad Cortex "scenes"): the
//! on/off state of the blocks, their knobs and the split settings, recalled
//! with live commands. The chain is not rebuilt, so nothing drops out and
//! delay and reverb tails ring on across the switch.
//!
//! The live chain is the current scene: switching stores it in its slot
//! first, so edits are kept. An empty slot starts as a copy of the scene
//! left.

use anyhow::{anyhow, Result};

use super::app_state::{collect_ids, find_in, find_in_mut, node_id};
use super::EffectChainState;
use crate::audio::ChainNode;
use crate::plugins::chain::is_panned;
use crate::plugins::ChainCommand;
use crate::presets::{SceneSnapshot, SplitSnapshot};

/// Scenes per preset (one per footswitch on two rows of four)
pub const SCENE_COUNT: usize = 8;
/// More changes than this rebuild the chain rather than flood the queue
const MAX_COMMANDS: usize = 512;

/// What recalling a scene asks of the audio chain
#[derive(Debug, Default)]
pub struct SceneSwitch {
    pub commands: Vec<ChainCommand>,
    /// Routing changed (a split panned on or off): rebuild instead
    pub rebuild: bool,
}

pub fn default_scene_name(slot: usize) -> String {
    format!("Scene {}", slot + 1)
}

impl EffectChainState {
    /// The live state of the chain, as a scene
    pub fn capture_scene(&self, name: String) -> SceneSnapshot {
        let mut ids = Vec::new();
        collect_ids(&self.nodes, &mut ids);
        let mut scene = SceneSnapshot {
            name,
            ..Default::default()
        };
        for id in ids {
            match find_in(&self.nodes, id) {
                Some(ChainNode::Plugin { enabled, .. }) => {
                    scene.enabled.insert(id, *enabled);
                    if let Some(values) = self.params.get(&id) {
                        scene.params.insert(id, values.clone());
                    }
                }
                Some(ChainNode::Split {
                    enabled,
                    ab_select_a,
                    mix_ratio,
                    pan,
                    ..
                }) => {
                    scene.enabled.insert(id, *enabled);
                    scene.splits.insert(
                        id,
                        SplitSnapshot {
                            select_a: *ab_select_a,
                            mix: *mix_ratio,
                            pan: *pan,
                        },
                    );
                }
                None => {}
            }
        }
        scene
    }

    /// Name of scene `slot`
    pub fn scene_name(&self, slot: usize) -> String {
        match self.scenes.get(slot) {
            Some(Some(scene)) if !scene.name.trim().is_empty() => scene.name.clone(),
            _ => default_scene_name(slot),
        }
    }

    /// True if scene `slot` holds something (the current one always does)
    pub fn scene_used(&self, slot: usize) -> bool {
        slot == self.scene || matches!(self.scenes.get(slot), Some(Some(_)))
    }

    pub fn rename_scene(&mut self, slot: usize, name: &str) -> Result<()> {
        if slot >= SCENE_COUNT {
            return Err(anyhow!("No scene {}", slot + 1));
        }
        self.store_scene();
        let name = name.trim();
        let name = if name.is_empty() {
            default_scene_name(slot)
        } else {
            name.to_string()
        };
        match &mut self.scenes[slot] {
            Some(scene) => scene.name = name,
            // Nothing captured yet: filled on its first visit
            empty => {
                *empty = Some(SceneSnapshot {
                    name,
                    ..Default::default()
                })
            }
        }
        Ok(())
    }

    /// Keep the live state in the current slot
    fn store_scene(&mut self) {
        if self.scenes.len() < SCENE_COUNT {
            self.scenes.resize(SCENE_COUNT, None);
        }
        let name = self.scene_name(self.scene);
        self.scenes[self.scene] = Some(self.capture_scene(name));
    }

    /// Scenes to save: the current one up to date, blocks that are gone
    /// left out. Empty if scenes were never used.
    pub fn saved_scenes(&self) -> Vec<Option<SceneSnapshot>> {
        if self.scenes.iter().all(Option::is_none) && self.scene == 0 {
            return Vec::new();
        }
        let mut ids = Vec::new();
        collect_ids(&self.nodes, &mut ids);
        let mut scenes = self.scenes.clone();
        scenes.resize(SCENE_COUNT, None);
        scenes[self.scene] = Some(self.capture_scene(self.scene_name(self.scene)));
        for scene in scenes.iter_mut().flatten() {
            scene.enabled.retain(|id, _| ids.contains(id));
            scene.params.retain(|id, _| ids.contains(id));
            scene.splits.retain(|id, _| ids.contains(id));
        }
        scenes
    }

    /// Recall scene `slot`: the chain takes its state, and the commands that
    /// bring the audio chain there are returned. Blocks the scene does not
    /// know (added since) keep their state.
    pub fn select_scene(&mut self, slot: usize) -> Result<SceneSwitch> {
        if slot >= SCENE_COUNT {
            return Err(anyhow!("No scene {}", slot + 1));
        }
        self.store_scene();
        self.scene = slot;
        let Some(target) = self.scenes[slot].clone().filter(|s| !s.enabled.is_empty()) else {
            // First visit: a copy of the scene left
            let name = self.scene_name(slot);
            self.scenes[slot] = Some(self.capture_scene(name));
            return Ok(SceneSwitch::default());
        };
        let mut switch = SceneSwitch::default();
        let mut ids = Vec::new();
        collect_ids(&self.nodes, &mut ids);
        let stereo = self.stereo_mode;
        for id in ids {
            let params = self.params.get_mut(&id);
            let Some(node) = find_in_mut(&mut self.nodes, id) else {
                continue;
            };
            let instance_id = node_id(node);
            match node {
                ChainNode::Plugin { enabled, .. } => {
                    if let Some(&on) = target.enabled.get(&id) {
                        if *enabled != on {
                            *enabled = on;
                            switch.commands.push(ChainCommand::SetEnabled {
                                instance_id,
                                enabled: on,
                            });
                        }
                    }
                    if let (Some(values), Some(wanted)) = (params, target.params.get(&id)) {
                        for (index, (value, wanted)) in values.iter_mut().zip(wanted).enumerate() {
                            if value != wanted {
                                *value = *wanted;
                                switch.commands.push(ChainCommand::SetParameter {
                                    instance_id,
                                    index,
                                    value: *wanted,
                                });
                            }
                        }
                    }
                }
                ChainNode::Split {
                    enabled,
                    split_type,
                    ab_select_a,
                    mix_ratio,
                    pan,
                    ..
                } => {
                    if let Some(&on) = target.enabled.get(&id) {
                        if *enabled != on {
                            *enabled = on;
                            switch.commands.push(ChainCommand::SetEnabled {
                                instance_id,
                                enabled: on,
                            });
                        }
                    }
                    let Some(wanted) = target.splits.get(&id) else {
                        continue;
                    };
                    if *ab_select_a != wanted.select_a {
                        *ab_select_a = wanted.select_a;
                        switch.commands.push(ChainCommand::SetSplitAb {
                            instance_id,
                            select_a: wanted.select_a,
                        });
                    }
                    if *mix_ratio != wanted.mix {
                        *mix_ratio = wanted.mix;
                        switch.commands.push(ChainCommand::SetSplitMix {
                            instance_id,
                            ratio: wanted.mix,
                        });
                    }
                    if *pan != wanted.pan {
                        let was = is_panned(*split_type, *pan);
                        *pan = wanted.pan;
                        switch.rebuild |= stereo && was != is_panned(*split_type, *pan);
                        switch.commands.push(ChainCommand::SetSplitPan {
                            instance_id,
                            pan: wanted.pan,
                        });
                    }
                }
            }
        }
        if switch.commands.len() > MAX_COMMANDS {
            switch.rebuild = true;
        }
        Ok(switch)
    }

    /// Block of stomp switch `slot`: the one assigned to it, else the n-th
    /// of the blocks no switch is assigned to
    pub fn stomp_target(&self, slot: usize) -> Option<usize> {
        let exists = |id: &usize| self.find(*id).is_some();
        if let Some(Some(id)) = self
            .stomps
            .get(slot)
            .filter(|s| s.is_some_and(|id| exists(&id)))
        {
            return Some(*id);
        }
        let assigned: Vec<usize> = self
            .stomps
            .iter()
            .flatten()
            .copied()
            .filter(exists)
            .collect();
        // Automatic switches take the free blocks in order
        let automatic = (0..slot)
            .filter(|s| !matches!(self.stomps.get(*s), Some(Some(id)) if exists(id)))
            .count();
        self.plugins()
            .into_iter()
            .map(node_id)
            .filter(|id| !assigned.contains(id))
            .nth(automatic)
    }

    /// Set every parameter that follows the tempo to `bpm`; the commands
    /// for the audio chain
    pub fn apply_tempo(
        &mut self,
        bpm: f32,
        tempo_params: impl Fn(&str) -> Vec<usize>,
    ) -> Vec<ChainCommand> {
        self.bpm = bpm;
        let plugins: Vec<(usize, String)> = self
            .plugins()
            .into_iter()
            .filter_map(|n| match n {
                ChainNode::Plugin {
                    instance_id, uri, ..
                } => Some((*instance_id, uri.clone())),
                _ => None,
            })
            .collect();
        let mut commands = Vec::new();
        for (instance_id, uri) in plugins {
            let Some(values) = self.params.get_mut(&instance_id) else {
                continue;
            };
            for index in tempo_params(&uri) {
                if let Some(value) = values.get_mut(index) {
                    if *value != bpm {
                        *value = bpm;
                        commands.push(ChainCommand::SetParameter {
                            instance_id,
                            index,
                            value: bpm,
                        });
                    }
                }
            }
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SplitType;

    fn chain() -> EffectChainState {
        let mut chain = EffectChainState::new();
        chain.add_plugin("urn:a", vec![0.5, 1.0]);
        let split = chain.add_split(SplitType::Mix);
        chain.add_to_split(split, true, "urn:b", vec![0.0]).unwrap();
        chain
    }

    #[test]
    fn scenes_recall_bypass_knobs_and_splits_live() {
        let mut chain = chain();
        // Scene 2 starts as a copy of scene 1, then is edited
        assert!(chain.select_scene(1).unwrap().commands.is_empty());
        chain.set_enabled(0, false).unwrap();
        chain.set_parameter(0, 1, 0.25).unwrap();
        chain.set_split_mix(1, 0.8).unwrap();

        let back = chain.select_scene(0).unwrap();
        assert!(!back.rebuild);
        assert_eq!(back.commands.len(), 3);
        assert_eq!(chain.is_enabled(0), Some(true));
        assert_eq!(chain.parameters(0), &[0.5, 1.0]);

        // Scene 2 kept its edits
        let again = chain.select_scene(1).unwrap();
        assert_eq!(again.commands.len(), 3);
        assert_eq!(chain.is_enabled(0), Some(false));
        assert_eq!(chain.parameters(0), &[0.5, 0.25]);
        assert_eq!(chain.scene, 1);
        assert!(chain.select_scene(SCENE_COUNT).is_err());
    }

    #[test]
    fn panning_on_in_stereo_rebuilds() {
        let mut chain = chain();
        chain.stereo_mode = true;
        chain.select_scene(1).unwrap();
        chain.set_split_pan(1, [-1.0, 1.0]).unwrap();
        assert!(chain.select_scene(0).unwrap().rebuild);
    }

    #[test]
    fn scenes_are_named_and_saved_without_removed_blocks() {
        let mut chain = chain();
        assert!(chain.saved_scenes().is_empty());
        chain.rename_scene(2, " Solo ").unwrap();
        assert_eq!(chain.scene_name(2), "Solo");
        assert_eq!(chain.scene_name(3), "Scene 4");
        // Renamed but never visited: filled on its first visit
        assert!(chain.select_scene(2).unwrap().commands.is_empty());
        assert_eq!(chain.scene_name(2), "Solo");

        chain.remove(0).unwrap();
        let saved = chain.saved_scenes();
        assert_eq!(saved.len(), SCENE_COUNT);
        assert!(saved
            .iter()
            .flatten()
            .all(|s| !s.enabled.contains_key(&0) && !s.params.contains_key(&0)));
        assert!(saved[2].is_some() && saved[0].is_some() && saved[5].is_none());
    }

    #[test]
    fn stomp_switches_take_assigned_then_free_blocks() {
        let mut chain = chain();
        chain.add_plugin("urn:c", vec![]);
        // Blocks in order: 0, 2 (in the split), 3
        assert_eq!(chain.stomp_target(0), Some(0));
        assert_eq!(chain.stomp_target(2), Some(3));
        assert_eq!(chain.stomp_target(3), None);
        chain.stomps = vec![None, Some(3)];
        assert_eq!(chain.stomp_target(0), Some(0));
        assert_eq!(chain.stomp_target(1), Some(3));
        assert_eq!(chain.stomp_target(2), Some(2));
        // A removed block frees its switch
        chain.remove(3).unwrap();
        assert_eq!(chain.stomp_target(1), Some(2));
    }

    #[test]
    fn tempo_sets_bpm_parameters() {
        let mut chain = chain();
        let commands = chain.apply_tempo(90.0, |uri| if uri == "urn:a" { vec![1] } else { vec![] });
        assert_eq!(commands.len(), 1);
        assert_eq!(chain.parameters(0), &[0.5, 90.0]);
        assert_eq!(chain.bpm, 90.0);
        assert!(chain.apply_tempo(90.0, |_| vec![1]).is_empty());
    }
}
