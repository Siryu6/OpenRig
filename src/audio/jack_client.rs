use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use jack::{Client, ClientOptions, Control, NotificationHandler, ProcessHandler, ProcessScope};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::chain_switcher::ChainControl;
use super::{ChainSwitcher, Looper, Tuner, TunerResult};
use crate::monitor::{DspMeter, LevelMeter};
use crate::plugins::Lv2Host;

/// Preset/chain switch crossfade
const CROSSFADE_MS: f32 = 20.0;

/// Running JACK client plus the handles to control it
pub struct ActiveJack {
    pub client: jack::AsyncClient<Notifications, PedalboardProcessor>,
    pub control: ChainControl,
    pub meter: Arc<DspMeter>,
    pub levels: Arc<LevelMeter>,
    /// Set when the server shuts down or drops the client
    pub stopped: Arc<AtomicBool>,
    pub sample_rate: f32,
    pub buffer_size: usize,
}

/// JACK client builder that creates and activates the audio client
pub struct JackClientBuilder {
    client_name: String,
}

impl JackClientBuilder {
    pub fn new(client_name: impl Into<String>) -> Self {
        Self {
            client_name: client_name.into(),
        }
    }

    /// Connect to the JACK server, register ports and start processing
    pub fn activate(
        self,
        host: Arc<Lv2Host>,
        tuner: Arc<Mutex<Tuner>>,
        looper: Arc<Mutex<Looper>>,
        tuner_tx: Sender<TunerResult>,
    ) -> Result<ActiveJack> {
        info!("Initializing JACK client: {}", self.client_name);

        let (client, _status) = Client::new(&self.client_name, ClientOptions::NO_START_SERVER)
            .context("Failed to create JACK client")?;

        let sample_rate = client.sample_rate() as f32;
        let buffer_size = client.buffer_size() as usize;
        info!(
            "JACK client created - Sample rate: {}Hz, Buffer size: {} samples",
            sample_rate, buffer_size
        );

        // Tuner and looper depend on the server's sample rate
        *tuner.lock().unwrap() = Tuner::new(sample_rate);
        *looper.lock().unwrap() = Looper::new(sample_rate);

        let in_port = client
            .register_port("guitar_in", jack::AudioIn)
            .context("Failed to register input port")?;
        let out_ports = [
            client
                .register_port("out_left", jack::AudioOut)
                .context("Failed to register the left output port")?,
            client
                .register_port("out_right", jack::AudioOut)
                .context("Failed to register the right output port")?,
        ];

        let (chain_switcher, control) =
            ChainSwitcher::new(host, sample_rate, buffer_size, CROSSFADE_MS)
                .context("Failed to create ChainSwitcher")?;

        let meter = Arc::new(DspMeter::default());
        let levels = Arc::new(LevelMeter::default());
        let processor = PedalboardProcessor {
            meter: meter.clone(),
            levels: levels.clone(),
            period_per_frame: Duration::from_secs_f64(1.0 / sample_rate.max(1.0) as f64),
            in_port,
            out_ports,
            tuner,
            looper,
            tuner_tx,
            chain_switcher,
            scratch: [vec![0.0; buffer_size.max(1)], vec![0.0; buffer_size.max(1)]],
        };

        let stopped = Arc::new(AtomicBool::new(false));
        let client = client
            .activate_async(
                Notifications {
                    meter: meter.clone(),
                    stopped: stopped.clone(),
                },
                processor,
            )
            .context("Failed to activate JACK client")?;
        connect_system_ports(client.as_client(), &self.client_name);

        info!("JACK client activated and processing audio");
        Ok(ActiveJack {
            client,
            control,
            meter,
            levels,
            stopped,
            sample_rate,
            buffer_size,
        })
    }
}

/// True when a JACK server accepts clients
pub fn server_running() -> bool {
    Client::new("pedalboard-probe", ClientOptions::NO_START_SERVER).is_ok()
}

/// Auto-connect to the first physical capture/playback ports, if any.
/// Can be disabled with PEDALBOARD_NO_AUTOCONNECT=1.
fn connect_system_ports(client: &Client, name: &str) {
    if std::env::var_os("PEDALBOARD_NO_AUTOCONNECT").is_some() {
        return;
    }
    let physical = |flags| client.ports(None, Some("32 bit float mono audio"), flags);
    let captures = physical(jack::PortFlags::IS_PHYSICAL | jack::PortFlags::IS_OUTPUT);
    let playbacks = physical(jack::PortFlags::IS_PHYSICAL | jack::PortFlags::IS_INPUT);

    if let Some(capture) = captures.first() {
        let _ = client.connect_ports_by_name(capture, &format!("{name}:guitar_in"));
    }
    // Left and right on the first two playback ports. With a single one,
    // only the left side is connected: use the mono mode.
    for (side, playback) in ["out_left", "out_right"].iter().zip(&playbacks) {
        let _ = client.connect_ports_by_name(&format!("{name}:{side}"), playback);
    }
}

/// JACK server events
pub struct Notifications {
    meter: Arc<DspMeter>,
    stopped: Arc<AtomicBool>,
}

impl NotificationHandler for Notifications {
    fn shutdown(&mut self, _status: jack::ClientStatus, reason: &str) {
        warn!("JACK server stopped: {}", reason);
        self.stopped.store(true, Ordering::Relaxed);
    }

    fn xrun(&mut self, _: &Client) -> Control {
        self.meter.xrun();
        Control::Continue
    }
}

/// Audio callback: input → tuner → effect chain → looper → left and right
/// outputs
pub struct PedalboardProcessor {
    /// Time spent in the callback vs. the buffer period
    meter: Arc<DspMeter>,
    /// Input and output peaks
    levels: Arc<LevelMeter>,
    period_per_frame: Duration,
    in_port: jack::Port<jack::AudioIn>,
    out_ports: [jack::Port<jack::AudioOut>; 2],
    tuner: Arc<Mutex<Tuner>>,
    looper: Arc<Mutex<Looper>>,
    tuner_tx: Sender<TunerResult>,
    chain_switcher: ChainSwitcher,
    /// Pre-allocated left and right buffers (no allocation in the audio
    /// callback)
    scratch: [Vec<f32>; 2],
}

impl ProcessHandler for PedalboardProcessor {
    fn process(&mut self, _client: &Client, ps: &ProcessScope) -> Control {
        let start = Instant::now();
        super::flush_denormals();
        let input = self.in_port.as_slice(ps);
        let n = input.len();
        let [left, right] = &mut self.scratch;
        if left.len() < n {
            // Only after a JACK buffer size increase
            left.resize(n, 0.0);
            right.resize(n, 0.0);
        }
        let (left, right) = (&mut left[..n], &mut right[..n]);

        // Tuner (may mute the signal while tuning)
        if let Ok(mut tuner) = self.tuner.try_lock() {
            if let Some(result) = tuner.process_with_mute(input, left) {
                let _ = self.tuner_tx.try_send(result);
            }
        } else {
            left.copy_from_slice(input);
        }

        // Effects: mono in, left and right out
        let switching = self.chain_switcher.process(left, right);

        // Looper sits after the effects so loops are recorded with the tone
        if let Ok(mut looper) = self.looper.try_lock() {
            looper.process(left, right);
        }
        // A plugin or model gone wrong (NaN, infinity) would send garbage,
        // possibly at full scale, to the speakers: silence instead
        if !left.iter().chain(right.iter()).all(|s| s.is_finite()) {
            left.fill(0.0);
            right.fill(0.0);
        }
        let [out_left, out_right] = &mut self.out_ports;
        out_left.as_mut_slice(ps).copy_from_slice(left);
        out_right.as_mut_slice(ps).copy_from_slice(right);
        self.levels.record(input, left, right);

        // Running the old and the new chain together while switching costs
        // more than either: left out so the load shown is the chain's
        if !switching {
            self.meter
                .record(start.elapsed(), self.period_per_frame * n as u32);
        }
        Control::Continue
    }
}
