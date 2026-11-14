//! Glitch-free chain switching.
//!
//! The UI sends a `ChainSpec` whenever the chain structure changes (preset
//! load, plugin added, split inserted…). A background thread instantiates the
//! plugins and hands the finished chain to the audio thread, which crossfades
//! from the old chain to the new one. Old chains are sent back to the
//! background thread to be dropped, so the audio thread never frees memory or
//! calls plugin cleanup code.
//!
//! Live changes (parameters, bypass, split controls) are `ChainCommand`s
//! applied directly on the audio thread. Each carries the generation of the
//! latest spec sent before it; commands newer than a chain being built are
//! replayed on it when it arrives, so no change is lost during a rebuild.
//!
//! A chain that panics (a plugin or a model gone wrong) is dropped by the
//! audio thread, which plays the dry signal and asks the builder for the
//! same chain again; a chain that keeps failing stays out and the user is
//! told. The audio never stops.

use anyhow::Result;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender, TrySendError};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use super::dsp_pool;
use super::latency::EffectsLatency;
use crate::midi::RawMidi;
use crate::plugins::chain::{ChainCommand, ChainIo, ChainSpec, EffectChain};
use crate::plugins::Lv2Host;

/// Number of recent commands kept for replay
const COMMAND_LOG_CAPACITY: usize = 1024;
/// MIDI messages waiting for the audio thread (an audio period at most)
const MIDI_CAPACITY: usize = 256;
/// Rebuilds of a chain that panicked, at most, within `REBUILD_WINDOW`
const MAX_REBUILDS: usize = 3;
const REBUILD_WINDOW: Duration = Duration::from_secs(60);

/// Crossfade state tracking
#[derive(Debug, Clone, Copy, PartialEq)]
enum CrossfadeState {
    Idle,
    Active {
        current_sample: usize,
        duration_samples: usize,
    },
}

impl CrossfadeState {
    fn new() -> Self {
        Self::Idle
    }

    fn is_active(&self) -> bool {
        matches!(self, CrossfadeState::Active { .. })
    }

    fn start(&mut self, duration_samples: usize) {
        *self = CrossfadeState::Active {
            current_sample: 0,
            duration_samples: duration_samples.max(1),
        };
    }

    /// Advance by `samples`; returns true once the crossfade is complete
    fn advance(&mut self, samples: usize) -> bool {
        match self {
            CrossfadeState::Active {
                current_sample,
                duration_samples,
            } => {
                *current_sample += samples;
                if *current_sample >= *duration_samples {
                    *self = CrossfadeState::Idle;
                    true
                } else {
                    false
                }
            }
            CrossfadeState::Idle => true,
        }
    }

    fn progress(&self) -> f32 {
        self.progress_after(0)
    }

    /// Progress `ahead` samples from now, up to 1
    fn progress_after(&self, ahead: usize) -> f32 {
        match self {
            CrossfadeState::Active {
                current_sample,
                duration_samples,
            } => ((current_sample + ahead) as f32 / (*duration_samples as f32).max(1.0)).min(1.0),
            CrossfadeState::Idle => 1.0,
        }
    }
}

/// A chain built by the background thread, tagged with its spec generation
pub struct BuiltChain {
    generation: u64,
    chain: EffectChain,
}

/// Fixed-capacity ring of recent commands (allocation-free once created)
struct CommandLog {
    entries: Vec<(u64, ChainCommand)>,
    next: usize,
}

impl CommandLog {
    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(COMMAND_LOG_CAPACITY),
            next: 0,
        }
    }

    fn push(&mut self, generation: u64, command: ChainCommand) {
        if self.entries.len() < COMMAND_LOG_CAPACITY {
            self.entries.push((generation, command));
        } else {
            self.entries[self.next] = (generation, command);
        }
        self.next = (self.next + 1) % COMMAND_LOG_CAPACITY;
    }

    /// Commands with generation >= `generation`, oldest first
    fn since(&self, generation: u64) -> impl Iterator<Item = &ChainCommand> {
        let (newer, older) = if self.entries.len() < COMMAND_LOG_CAPACITY {
            (&self.entries[..], &self.entries[..0])
        } else {
            (&self.entries[self.next..], &self.entries[..self.next])
        };
        newer
            .iter()
            .chain(older)
            .filter(move |(g, _)| *g >= generation)
            .map(|(_, c)| c)
    }
}

/// Handles to talk to the chain switcher from outside the audio thread
#[derive(Clone)]
pub struct ChainControl {
    pub spec_tx: Sender<ChainSpec>,
    pub command_tx: Sender<(u64, ChainCommand)>,
    /// MIDI for plugins; not logged, so a stream of controller values never
    /// pushes commands out of the replay log
    pub midi_tx: Sender<RawMidi>,
    /// Plugin load errors reported by the background builder
    pub error_rx: Receiver<String>,
}

/// Double-buffered chain switcher with crossfading (owned by the audio thread)
pub struct ChainSwitcher {
    active: EffectChain,
    next: Option<EffectChain>,
    /// Chain that arrived during a crossfade: it waits for the crossfade to
    /// finish, so the chain fading in is never cut off
    pending: Option<EffectChain>,
    crossfade_state: CrossfadeState,
    crossfade_duration_samples: usize,
    max_block: usize,
    /// Output of the next chain during the crossfade (left, right)
    next_out: [Box<[f32]>; 2],
    ready_rx: Receiver<BuiltChain>,
    command_rx: Receiver<(u64, ChainCommand)>,
    midi_rx: Receiver<RawMidi>,
    garbage_tx: Sender<EffectChain>,
    /// Asks the builder for the latest chain again, after a panic
    rebuild_tx: Sender<()>,
    log: CommandLog,
}

impl ChainSwitcher {
    /// Create the switcher and its background builder thread
    pub fn new(
        host: Arc<Lv2Host>,
        sample_rate: f32,
        max_block: usize,
        crossfade_duration_ms: f32,
    ) -> Result<(Self, ChainControl)> {
        let max_block = max_block.max(1);
        let (spec_tx, spec_rx) = unbounded::<ChainSpec>();
        let (ready_tx, ready_rx) = bounded::<BuiltChain>(4);
        let (garbage_tx, garbage_rx) = bounded::<EffectChain>(16);
        let (command_tx, command_rx) = bounded(COMMAND_LOG_CAPACITY);
        let (midi_tx, midi_rx) = bounded(MIDI_CAPACITY);
        let (error_tx, error_rx) = unbounded();
        let (rebuild_tx, rebuild_rx) = bounded(1);

        std::thread::Builder::new()
            .name("chain-builder".to_string())
            .spawn(move || {
                builder_thread(
                    host,
                    Inbox {
                        specs: spec_rx,
                        garbage: garbage_rx,
                        rebuilds: rebuild_rx,
                    },
                    ready_tx,
                    error_tx,
                    sample_rate,
                    max_block,
                )
            })?;

        let switcher = Self {
            active: EffectChain::empty(max_block),
            next: None,
            pending: None,
            crossfade_state: CrossfadeState::new(),
            crossfade_duration_samples: (sample_rate * crossfade_duration_ms / 1000.0) as usize,
            max_block,
            next_out: [
                vec![0.0; max_block].into_boxed_slice(),
                vec![0.0; max_block].into_boxed_slice(),
            ],
            ready_rx,
            command_rx,
            midi_rx,
            garbage_tx,
            rebuild_tx,
            log: CommandLog::new(),
        };
        info!(
            sample_rate,
            max_block,
            crossfade_ms = crossfade_duration_ms,
            "Chain switcher ready"
        );
        Ok((
            switcher,
            ChainControl {
                spec_tx,
                command_tx,
                midi_tx,
                error_rx,
            },
        ))
    }

    fn discard(&self, chain: EffectChain) {
        if let Err(TrySendError::Full(chain)) | Err(TrySendError::Disconnected(chain)) =
            self.garbage_tx.try_send(chain)
        {
            // Should not happen. Dropping would free plugins and join the IR
            // thread on the audio thread: leak it instead
            warn_once_leaked();
            std::mem::forget(chain);
        }
    }

    /// Apply pending commands and pick up freshly built chains (audio thread)
    fn poll(&mut self) {
        while let Ok((generation, command)) = self.command_rx.try_recv() {
            self.log.push(generation, command);
            self.active.apply(&command);
            for chain in self.next.iter_mut().chain(self.pending.iter_mut()) {
                chain.apply(&command);
            }
        }
        while let Ok(message) = self.midi_rx.try_recv() {
            self.active.send_midi(message);
            for chain in self.next.iter_mut().chain(self.pending.iter_mut()) {
                chain.send_midi(message);
            }
        }

        while let Ok(BuiltChain {
            generation,
            mut chain,
        }) = self.ready_rx.try_recv()
        {
            for command in self.log.since(generation) {
                chain.apply(command);
            }
            if self.next.is_some() {
                if let Some(previous) = self.pending.replace(chain) {
                    self.discard(previous);
                }
            } else {
                self.next = Some(chain);
                self.crossfade_state.start(self.crossfade_duration_samples);
            }
        }
    }

    /// `process_io` without inputs or sends
    #[cfg(test)]
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        crate::plugins::chain::no_io(|io| self.process_io(left, right, io))
    }

    /// Process the mono input in `left` into the `left` and `right`
    /// outputs, in place (audio thread), with the inputs of the interface
    /// for the returns and the send buses for the sends. Returns true if two
    /// chains ran (a crossfade to a new chain): a one-off load, not the
    /// chain's own.
    pub fn process_io(&mut self, left: &mut [f32], right: &mut [f32], io: &mut ChainIo) -> bool {
        self.poll();
        let max_block = self.max_block;
        let mut switching = false;
        // The panic itself is logged by the hook set in `main`
        let ran = catch_unwind(AssertUnwindSafe(|| {
            for (k, (l, r)) in left
                .chunks_mut(max_block)
                .zip(right.chunks_mut(max_block))
                .enumerate()
            {
                let offset = k * max_block;
                if self.crossfade_state.is_active() && self.next.is_some() {
                    self.process_crossfade(l, r, io, offset);
                    switching = true;
                } else {
                    io.part(offset, l.len(), 1.0, 1.0);
                    self.active.process_io(l, r, io);
                }
            }
        }));
        if ran.is_err() {
            left.fill(0.0);
            right.fill(0.0);
            self.drop_chains();
        }
        switching
    }

    /// After a panic: the chains may be half-way through anything, so they
    /// go (off this thread), the dry signal plays, and the latest chain is
    /// built again
    fn drop_chains(&mut self) {
        let chains = [
            Some(std::mem::replace(
                &mut self.active,
                EffectChain::empty(self.max_block),
            )),
            self.next.take(),
            self.pending.take(),
        ];
        for chain in chains.into_iter().flatten() {
            self.discard(chain);
        }
        self.crossfade_state = CrossfadeState::new();
        // Full: a rebuild is already asked for
        let _ = self.rebuild_tx.try_send(());
    }

    fn process_crossfade(&mut self, l: &mut [f32], r: &mut [f32], io: &mut ChainIo, offset: usize) {
        let n = l.len();
        let [next_l, next_r] = &mut self.next_out;
        let (next_l, next_r) = (&mut next_l[..n], &mut next_r[..n]);
        next_l.copy_from_slice(l);
        // The sends of each chain follow its share of the crossfade
        let angle =
            |ahead| self.crossfade_state.progress_after(ahead) * std::f32::consts::FRAC_PI_2;
        let (from, to) = (angle(0), angle(n));
        io.part(offset, n, from.cos(), to.cos());
        let active = &mut self.active;
        match &mut self.next {
            // Both chains at once on two cores, when one of them adds to no
            // send bus (only the audio thread does)
            Some(next) if !next.has_send() => {
                let mut next_io = io.without_sends();
                next_io.part(offset, n, from.sin(), to.sin());
                dsp_pool::join(
                    || active.process_io(l, r, io),
                    || next.process_io(next_l, next_r, &mut next_io),
                );
            }
            Some(next) if !active.has_send() => {
                let mut active_io = io.without_sends();
                io.part(offset, n, from.sin(), to.sin());
                dsp_pool::join(
                    || next.process_io(next_l, next_r, io),
                    || active.process_io(l, r, &mut active_io),
                );
            }
            Some(next) => {
                active.process_io(l, r, io);
                io.part(offset, n, from.sin(), to.sin());
                next.process_io(next_l, next_r, io);
            }
            None => active.process_io(l, r, io),
        }

        // Equal-power crossfade, per sample
        for i in 0..n {
            let p = self.crossfade_state.progress() * std::f32::consts::FRAC_PI_2;
            l[i] = l[i] * p.cos() + next_l[i] * p.sin();
            r[i] = r[i] * p.cos() + next_r[i] * p.sin();
            if self.crossfade_state.advance(1) {
                // Remaining samples come from the new chain only
                l[i + 1..n].copy_from_slice(&next_l[i + 1..n]);
                r[i + 1..n].copy_from_slice(&next_r[i + 1..n]);
                break;
            }
        }

        if !self.crossfade_state.is_active() {
            if let Some(next) = self.next.take() {
                let old = std::mem::replace(&mut self.active, next);
                self.discard(old);
            }
            if let Some(pending) = self.pending.take() {
                self.next = Some(pending);
                self.crossfade_state.start(self.crossfade_duration_samples);
            }
        }
    }

    /// Chain heard once the switch in progress, if any, is over
    fn heard(&self) -> &EffectChain {
        self.next.as_ref().unwrap_or(&self.active)
    }

    /// Frames the main mix and the send buses are late by (real-time safe)
    pub fn latency(&self) -> EffectsLatency {
        self.heard().effects_latency()
    }

    #[cfg(test)]
    pub fn is_crossfading(&self) -> bool {
        self.crossfade_state.is_active()
    }
}

/// What the builder thread receives
struct Inbox {
    /// Chains to build, from the UI
    specs: Receiver<ChainSpec>,
    /// Old chains to drop, from the audio thread
    garbage: Receiver<EffectChain>,
    /// The latest chain again, from the audio thread after a panic
    rebuilds: Receiver<()>,
}

fn builder_thread(
    host: Arc<Lv2Host>,
    inbox: Inbox,
    ready_tx: Sender<BuiltChain>,
    error_tx: Sender<String>,
    sample_rate: f32,
    max_block: usize,
) {
    debug!("Chain builder thread started");
    let report = |message: &str| {
        // The UI is gone only when the app quits
        let _ = error_tx.send(message.to_string());
    };
    let build = |spec: &ChainSpec| match catch_unwind(AssertUnwindSafe(|| {
        EffectChain::build(spec, &host, sample_rate, max_block)
    })) {
        Ok((chain, errors)) => {
            for error in errors {
                report(&error);
            }
            debug!(
                generation = spec.generation,
                plugins = chain.len(),
                "Chain built"
            );
            Some(chain)
        }
        Err(_) => {
            error!(
                generation = spec.generation,
                "Chain not built: a plugin panicked"
            );
            report("The effect chain could not be built: a plugin failed");
            None
        }
    };
    let mut latest: Option<ChainSpec> = None;
    let mut rebuilds: VecDeque<Instant> = VecDeque::new();
    loop {
        let chain = select! {
            recv(inbox.specs) -> spec => {
                let Ok(mut spec) = spec else { break };
                // Only the latest spec matters
                while let Ok(newer) = inbox.specs.try_recv() {
                    spec = newer;
                }
                let chain = build(&spec).map(|chain| (spec.generation, chain));
                latest = Some(spec);
                chain
            }
            recv(inbox.rebuilds) -> request => {
                if request.is_err() {
                    break;
                }
                let now = Instant::now();
                rebuilds.retain(|at| now.saturating_duration_since(*at) < REBUILD_WINDOW);
                let Some(spec) = latest.as_ref() else { continue };
                if rebuilds.len() >= MAX_REBUILDS {
                    error!(
                        generation = spec.generation,
                        "Effect chain keeps failing: dry signal until the next change"
                    );
                    report("The effect chain keeps failing: playing the dry signal until the next change");
                    continue;
                }
                rebuilds.push_back(now);
                warn!(generation = spec.generation, "Effect chain failed: rebuilding it");
                build(spec).map(|chain| (spec.generation, chain))
            }
            recv(inbox.garbage) -> chain => {
                let Ok(chain) = chain else { break };
                // Dropped here, off the audio thread; plugin cleanup code
                // that panics loses nothing more
                if catch_unwind(AssertUnwindSafe(|| drop(chain))).is_err() {
                    warn!("A plugin panicked while being freed");
                }
                None
            }
        };
        if let Some((generation, chain)) = chain {
            if ready_tx.send(BuiltChain { generation, chain }).is_err() {
                break;
            }
        }
    }
    debug!("Chain builder thread stopped");
}

/// Logging allocates on the audio thread: only report the first leak
fn warn_once_leaked() {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warn!("Chain garbage queue full: an old chain is leaked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::split_chain::ChainNode;
    use std::time::{Duration, Instant};

    #[test]
    fn test_crossfade_state() {
        let mut state = CrossfadeState::new();
        assert!(!state.is_active());
        assert_eq!(state.progress(), 1.0);

        state.start(100);
        assert!(state.is_active());
        assert_eq!(state.progress(), 0.0);

        state.advance(50);
        assert!((state.progress() - 0.5).abs() < 0.01);

        assert!(state.advance(50));
        assert!(!state.is_active());
    }

    #[test]
    fn test_command_log_wraps() {
        let mut log = CommandLog::new();
        for i in 0..(COMMAND_LOG_CAPACITY + 10) {
            log.push(
                i as u64,
                ChainCommand::SetSplitMix {
                    instance_id: i,
                    ratio: 0.0,
                },
            );
        }
        let replayed: Vec<_> = log.since(COMMAND_LOG_CAPACITY as u64).collect();
        assert_eq!(replayed.len(), 10);
        assert_eq!(
            *replayed[0],
            ChainCommand::SetSplitMix {
                instance_id: COMMAND_LOG_CAPACITY,
                ratio: 0.0
            }
        );
    }

    /// A switcher, sent a chain of one demo reverb (not built yet)
    fn with_reverb() -> (ChainSwitcher, ChainControl) {
        let host = Arc::new(Lv2Host::demo());
        let (switcher, control) = ChainSwitcher::new(host, 48000.0, 128, 5.0).unwrap();
        control
            .spec_tx
            .send(ChainSpec {
                generation: 1,
                nodes: vec![ChainNode::Plugin {
                    instance_id: 0,
                    uri: "urn:pedalboard:demo:reverb".to_string(),
                    enabled: true,
                    num_inputs: 2,
                    num_outputs: 2,
                }],
                ..Default::default()
            })
            .unwrap();
        (switcher, control)
    }

    #[test]
    fn switches_to_a_new_chain() {
        let (mut switcher, _control) = with_reverb();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut switched = false;
        while switcher.active.is_empty() {
            assert!(Instant::now() < deadline, "chain never arrived");
            let (mut left, mut right) = (vec![0.5; 256], vec![0.0; 256]);
            switched |= switcher.process(&mut left, &mut right);
            assert!(left.iter().chain(&right).all(|s| s.abs() <= 0.5 * 1.42));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(switcher.active.len(), 1);
        assert!(!switcher.is_crossfading());
        // The crossfade is reported, then plain cycles again
        assert!(switched);
        let (mut left, mut right) = (vec![0.5; 256], vec![0.0; 256]);
        assert!(!switcher.process(&mut left, &mut right));
    }

    /// Plays until the active chain has `len` blocks
    fn wait_for(switcher: &mut ChainSwitcher, len: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while switcher.active.len() != len {
            assert!(Instant::now() < deadline, "chain never arrived");
            let (mut left, mut right) = (vec![0.5; 256], vec![0.0; 256]);
            switcher.process(&mut left, &mut right);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_chain_that_panicked_is_built_again_a_few_times() {
        let (mut switcher, control) = with_reverb();
        wait_for(&mut switcher, 1);
        for _ in 0..MAX_REBUILDS {
            // What the audio thread does after a panic
            switcher.drop_chains();
            assert!(switcher.active.is_empty());
            wait_for(&mut switcher, 1);
        }
        // Once more within the window: it stays out, and the user is told
        switcher.drop_chains();
        let deadline = Instant::now() + Duration::from_secs(5);
        let message = loop {
            if let Ok(message) = control.error_rx.try_recv() {
                break message;
            }
            assert!(Instant::now() < deadline, "failure never reported");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(message.contains("keeps failing"));
        assert!(switcher.active.is_empty());
    }
}
