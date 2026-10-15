//! MIDI CCs learned by the blocks of the chain (see `crate::midi`). As on
//! Helix, one controller can drive several targets (an expression pedal
//! opening a wah and closing a volume), a target follows one controller,
//! and the assignments are saved with the preset.

use anyhow::{anyhow, Result};

use super::app_state::{collect_ids, EffectChainState};
use crate::midi::{is_global, MidiAssign, MidiTarget, TOE};

impl EffectChainState {
    /// Assign `controller` to a target of a block, replacing what that
    /// target followed (its range is kept)
    pub fn learn_midi(
        &mut self,
        controller: u8,
        instance_id: usize,
        target: MidiTarget,
    ) -> Result<()> {
        if controller > 127 || is_global(controller) {
            return Err(anyhow!("CC {} is a global command", controller));
        }
        if self.find(instance_id).is_none() {
            return Err(anyhow!("Block {} not found", instance_id));
        }
        let range = self
            .midi_assign(instance_id, target)
            .map(|a| (a.heel, a.toe));
        self.clear_midi(instance_id, target);
        let mut assign = MidiAssign::new(controller, instance_id, target);
        if let Some((heel, toe)) = range {
            (assign.heel, assign.toe) = (heel, toe);
        }
        self.midi.push(assign);
        Ok(())
    }

    /// Set where the controller of a target puts it at 0 and 127 (percent
    /// of its range); returns whether the target has a controller
    pub fn set_midi_range(
        &mut self,
        instance_id: usize,
        target: MidiTarget,
        heel: u8,
        toe: u8,
    ) -> bool {
        let found = self
            .midi
            .iter_mut()
            .find(|a| a.instance_id == instance_id && a.target == target);
        match found {
            Some(assign) => {
                assign.heel = heel.min(TOE);
                assign.toe = toe.min(TOE);
                true
            }
            None => false,
        }
    }

    /// Forget the controller of a target; returns whether it had one
    pub fn clear_midi(&mut self, instance_id: usize, target: MidiTarget) -> bool {
        let before = self.midi.len();
        self.midi
            .retain(|a| !(a.instance_id == instance_id && a.target == target));
        self.midi.len() != before
    }

    /// Assignment of a target, if any
    pub fn midi_assign(&self, instance_id: usize, target: MidiTarget) -> Option<MidiAssign> {
        self.midi
            .iter()
            .find(|a| a.instance_id == instance_id && a.target == target)
            .copied()
    }

    /// Controller of a target, if any
    pub fn midi_controller(&self, instance_id: usize, target: MidiTarget) -> Option<u8> {
        self.midi_assign(instance_id, target).map(|a| a.controller)
    }

    /// Assignments following `controller`, on blocks still in the chain
    pub fn midi_targets(&self, controller: u8) -> Vec<MidiAssign> {
        self.midi
            .iter()
            .filter(|a| a.controller == controller && self.find(a.instance_id).is_some())
            .copied()
            .collect()
    }

    /// Assignments to save: those of blocks still in the chain
    pub fn saved_midi(&self) -> Vec<MidiAssign> {
        let mut ids = Vec::new();
        collect_ids(&self.nodes, &mut ids);
        self.midi
            .iter()
            .filter(|a| ids.contains(&a.instance_id))
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::ChainNode;

    fn chain() -> EffectChainState {
        let plugin = |instance_id| ChainNode::Plugin {
            instance_id,
            uri: "urn:test".into(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        };
        EffectChainState {
            nodes: vec![plugin(1), plugin(2)],
            ..Default::default()
        }
    }

    #[test]
    fn one_controller_drives_several_targets() {
        let mut c = chain();
        c.learn_midi(1, 1, MidiTarget::Parameter(0)).unwrap();
        c.learn_midi(1, 2, MidiTarget::Parameter(3)).unwrap();
        c.learn_midi(11, 2, MidiTarget::Bypass).unwrap();
        let targets = |c: &EffectChainState, cc| {
            c.midi_targets(cc)
                .iter()
                .map(|a| (a.instance_id, a.target))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            targets(&c, 1),
            vec![(1, MidiTarget::Parameter(0)), (2, MidiTarget::Parameter(3))]
        );
        // Learning again moves the target to the new controller, range kept
        assert!(c.set_midi_range(1, MidiTarget::Parameter(0), 100, 0));
        c.learn_midi(2, 1, MidiTarget::Parameter(0)).unwrap();
        assert_eq!(targets(&c, 1), vec![(2, MidiTarget::Parameter(3))]);
        assert_eq!(c.midi_controller(1, MidiTarget::Parameter(0)), Some(2));
        let assign = c.midi_assign(1, MidiTarget::Parameter(0)).unwrap();
        assert_eq!((assign.heel, assign.toe), (100, 0));
        assert!(!c.set_midi_range(1, MidiTarget::Bypass, 0, 50));
        assert!(c.clear_midi(2, MidiTarget::Bypass));
        assert_eq!(c.midi_controller(2, MidiTarget::Bypass), None);
    }

    #[test]
    fn global_commands_and_missing_blocks_are_refused() {
        let mut c = chain();
        assert!(c.learn_midi(69, 1, MidiTarget::Bypass).is_err());
        assert!(c.learn_midi(64, 1, MidiTarget::Bypass).is_err());
        assert!(c.learn_midi(7, 9, MidiTarget::Bypass).is_err());
    }

    #[test]
    fn assignments_of_removed_blocks_are_not_saved() {
        let mut c = chain();
        c.learn_midi(1, 2, MidiTarget::Bypass).unwrap();
        c.nodes.pop();
        assert!(c.saved_midi().is_empty());
        assert!(c.midi_targets(1).is_empty());
    }
}
