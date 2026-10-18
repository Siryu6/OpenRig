use anyhow::Result;
use crossbeam_channel::{bounded, Receiver};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::info;

use super::chain_switcher::ChainControl;
use super::jack_client::{ActiveJack, JackClientBuilder};
use super::routing::{Routing, RoutingControl};
use super::{Looper, Tuner, TunerResult};
use crate::monitor::{DspMeter, LevelMeter};
use crate::plugins::Lv2Host;

/// Running audio engine (JACK client + chain builder)
pub struct AudioEngine {
    jack: ActiveJack,
    tuner_rx: Receiver<TunerResult>,
}

impl AudioEngine {
    /// Connect to JACK and start processing
    pub fn new(
        host: Arc<Lv2Host>,
        tuner: Arc<Mutex<Tuner>>,
        looper: Arc<Mutex<Looper>>,
    ) -> Result<Self> {
        info!("Starting audio engine");
        let (tuner_tx, tuner_rx) = bounded(16);
        let routing = Routing::load(&crate::state::data_dir());
        let jack = JackClientBuilder::new("pedalboard")
            .activate(&routing, host, tuner, looper, tuner_tx)?;
        Ok(Self { jack, tuner_rx })
    }

    /// Handles to send chain specs and live commands to the audio thread
    pub fn control(&self) -> ChainControl {
        self.jack.control.clone()
    }

    /// Handle to change the input feeding the chain and what the outputs
    /// play, with the number of inputs and outputs
    pub fn routing(&self) -> RoutingControl {
        self.jack.routing.clone()
    }

    /// Receiver for tuner results
    pub fn tuner_receiver(&self) -> Receiver<TunerResult> {
        self.tuner_rx.clone()
    }

    pub fn sample_rate(&self) -> f32 {
        self.jack.sample_rate
    }

    pub fn buffer_size(&self) -> usize {
        self.jack.buffer_size
    }

    /// Load of the audio callback and xrun count
    pub fn meter(&self) -> Arc<DspMeter> {
        self.jack.meter.clone()
    }

    /// Input and output peaks, for the level meters
    pub fn levels(&self) -> Arc<LevelMeter> {
        self.jack.levels.clone()
    }

    /// True once the JACK server has shut down: audio stays off until the
    /// app is restarted
    pub fn stopped(&self) -> bool {
        self.jack.stopped.load(Ordering::Relaxed)
    }

    /// DSP load of the whole JACK graph as reported by the server (0..1),
    /// including the other clients
    pub fn jack_load(&self) -> f32 {
        self.jack.client.as_client().cpu_load() / 100.0
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        info!("Shutting down audio engine");
    }
}
