use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use jack::{Client, ClientOptions, Control, NotificationHandler, ProcessHandler, ProcessScope};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::chain_switcher::ChainControl;
use super::routing::{OutputRouter, PortMeter, Routing, RoutingControl, MAX_PORTS, MAX_SENDS};
use super::{ChainSwitcher, Looper, Tuner, TunerResult};
use crate::monitor::{DspMeter, LevelMeter};
use crate::plugins::chain::ChainIo;
use crate::plugins::Lv2Host;

/// Preset/chain switch crossfade
const CROSSFADE_MS: f32 = 20.0;

/// Running JACK client plus the handles to control it
pub struct ActiveJack {
    pub client: jack::AsyncClient<Notifications, PedalboardProcessor>,
    pub control: ChainControl,
    pub routing: RoutingControl,
    pub meter: Arc<DspMeter>,
    pub levels: Arc<LevelMeter>,
    /// Peaks of every input and output (Routing view)
    pub ports: Arc<PortMeter>,
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
        routing: &Routing,
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

        // As many inputs and outputs as the interface has (at least the
        // guitar input and a stereo pair)
        let (captures, playbacks) = physical_ports(&client);
        let inputs = captures.len().clamp(1, MAX_PORTS);
        let outputs = playbacks.len().clamp(2, MAX_PORTS);
        let mut in_ports = Vec::with_capacity(inputs);
        for i in 1..=inputs {
            let mut port = client
                .register_port(&format!("in_{i}"), jack::AudioIn::default())
                .with_context(|| format!("Failed to register input {i}"))?;
            if i == 1 {
                // The name of the single input before multi-channel support
                let _ = port.set_alias(&format!("{}:guitar_in", self.client_name));
            }
            in_ports.push(port);
        }
        let mut out_ports = Vec::with_capacity(outputs);
        for i in 1..=outputs {
            let mut port = client
                .register_port(&format!("out_{i}"), jack::AudioOut::default())
                .with_context(|| format!("Failed to register output {i}"))?;
            if let Some(side) = ["out_left", "out_right"].get(i - 1) {
                let _ = port.set_alias(&format!("{}:{side}", self.client_name));
            }
            out_ports.push(port);
        }
        info!("{inputs} inputs, {outputs} outputs");
        // With a single physical output, the routing sees one output, so the
        // main pair is summed on it
        let heard = if playbacks.len() == 1 { 1 } else { outputs };
        let (router, routing_control) = OutputRouter::new(routing, inputs, heard, sample_rate);

        let (chain_switcher, control) =
            ChainSwitcher::new(host, sample_rate, buffer_size, CROSSFADE_MS)
                .context("Failed to create ChainSwitcher")?;

        let meter = Arc::new(DspMeter::default());
        let levels = Arc::new(LevelMeter::default());
        let ports = Arc::new(PortMeter::default());
        let processor = PedalboardProcessor {
            meter: meter.clone(),
            levels: levels.clone(),
            ports: ports.clone(),
            period_per_frame: Duration::from_secs_f64(1.0 / sample_rate.max(1.0) as f64),
            in_ports,
            out_ports,
            router,
            tuner,
            looper,
            tuner_tx,
            chain_switcher,
            scratch: [vec![0.0; buffer_size.max(1)], vec![0.0; buffer_size.max(1)]],
            sends: std::array::from_fn(|_| {
                [vec![0.0; buffer_size.max(1)], vec![0.0; buffer_size.max(1)]]
            }),
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
        connect_system_ports(client.as_client(), &self.client_name, &captures, &playbacks);

        info!("JACK client activated and processing audio");
        Ok(ActiveJack {
            client,
            control,
            routing: routing_control,
            meter,
            levels,
            ports,
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

/// Physical capture and playback ports (the audio interface's inputs and
/// outputs), in order
fn physical_ports(client: &Client) -> (Vec<String>, Vec<String>) {
    let physical = |flags| client.ports(None, Some("32 bit float mono audio"), flags);
    (
        physical(jack::PortFlags::IS_PHYSICAL | jack::PortFlags::IS_OUTPUT),
        physical(jack::PortFlags::IS_PHYSICAL | jack::PortFlags::IS_INPUT),
    )
}

/// Connect input n to the interface's input n and output n to its output n.
/// Can be disabled with PEDALBOARD_NO_AUTOCONNECT=1.
fn connect_system_ports(client: &Client, name: &str, captures: &[String], playbacks: &[String]) {
    // Which interface port each input and output is wired to, to tell a
    // routing problem from a silent output of the interface (issue #19)
    info!("Interface inputs: {}", list(captures));
    info!("Interface outputs: {}", list(playbacks));
    if std::env::var_os("PEDALBOARD_NO_AUTOCONNECT").is_some() {
        info!("PEDALBOARD_NO_AUTOCONNECT set: ports left unconnected");
        return;
    }
    if captures.len().max(playbacks.len()) > MAX_PORTS {
        warn!("Only the first {MAX_PORTS} inputs and outputs of the interface are used");
    }
    let mut connected = Vec::new();
    let mut connect =
        |source: &str, destination: &str| match client.connect_ports_by_name(source, destination) {
            Ok(()) => connected.push(format!("{source} → {destination}")),
            Err(e) => warn!("Cannot connect {source} to {destination}: {e}"),
        };
    for (i, capture) in captures.iter().take(MAX_PORTS).enumerate() {
        connect(capture, &format!("{name}:in_{}", i + 1));
    }
    for (i, playback) in playbacks.iter().take(MAX_PORTS).enumerate() {
        connect(&format!("{name}:out_{}", i + 1), playback);
    }
    info!("Connected: {}", list(&connected));
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

/// JACK server events
pub struct Notifications {
    meter: Arc<DspMeter>,
    stopped: Arc<AtomicBool>,
}

impl NotificationHandler for Notifications {
    // Runs like a signal handler (async-signal-safe calls only): only set
    // the flag, the UI loop reports it
    unsafe fn shutdown(&mut self, _status: jack::ClientStatus, _reason: &str) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    fn xrun(&mut self, _: &Client) -> Control {
        self.meter.xrun();
        Control::Continue
    }
}

/// Audio callback: input → tuner → effect chain (with its returns and
/// sends) → looper → output mixer (the main mix, the sends and the inputs
/// on the outputs)
pub struct PedalboardProcessor {
    /// Time spent in the callback vs. the buffer period
    meter: Arc<DspMeter>,
    /// Input and output peaks
    levels: Arc<LevelMeter>,
    ports: Arc<PortMeter>,
    period_per_frame: Duration,
    in_ports: Vec<jack::Port<jack::AudioIn>>,
    out_ports: Vec<jack::Port<jack::AudioOut>>,
    router: OutputRouter,
    tuner: Arc<Mutex<Tuner>>,
    looper: Arc<Mutex<Looper>>,
    tuner_tx: Sender<TunerResult>,
    chain_switcher: ChainSwitcher,
    /// Pre-allocated left and right buffers (no allocation in the audio
    /// callback)
    scratch: [Vec<f32>; 2],
    /// Send buses (left, right), filled by the chain every period
    sends: [[Vec<f32>; 2]; MAX_SENDS],
}

impl ProcessHandler for PedalboardProcessor {
    fn process(&mut self, _client: &Client, ps: &ProcessScope) -> Control {
        let start = Instant::now();
        super::flush_denormals();
        self.router.update();
        let input = self.in_ports[self.router.chain_input()].as_slice(ps);
        let n = input.len();
        let [left, right] = &mut self.scratch;
        if left.len() < n {
            // Only after a JACK buffer size increase
            left.resize(n, 0.0);
            right.resize(n, 0.0);
            for bus in self.sends.iter_mut().flatten() {
                bus.resize(n, 0.0);
            }
        }
        let (left, right) = (&mut left[..n], &mut right[..n]);
        for bus in self.sends.iter_mut().flatten() {
            bus[..n].fill(0.0);
        }

        // Tuner (may mute the signal while tuning)
        let mut muted = false;
        if let Ok(mut tuner) = self.tuner.try_lock() {
            if let Some(result) = tuner.process_with_mute(input, left) {
                let _ = self.tuner_tx.try_send(result);
            }
            muted = tuner.is_muted();
        } else {
            left.copy_from_slice(input);
        }

        // Effects: mono in, left and right out. Returns read the inputs,
        // sends fill the send buses. While the tuner mutes, the inputs are
        // silent everywhere: in the returns and played straight on an output.
        let mut inputs: [&[f32]; MAX_PORTS] = [&[]; MAX_PORTS];
        for (slot, port) in inputs.iter_mut().zip(&self.in_ports) {
            *slot = port.as_slice(ps);
        }
        let ports = self.in_ports.len();
        let heard = if muted { 0 } else { ports };
        let switching = {
            let mut io = ChainIo::new(&inputs[..heard], &mut self.sends);
            self.chain_switcher.process_io(left, right, &mut io)
        };

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
        for bus in self.sends.iter_mut().flatten() {
            let bus = &mut bus[..n];
            if !bus.iter().all(|s| s.is_finite()) {
                bus.fill(0.0);
            }
        }
        let passed = |i: u8| inputs[..heard].get(i as usize).copied();
        // The main mix, then each send
        let mut buses: [[&[f32]; 2]; 1 + MAX_SENDS] = [[&[], &[]]; 1 + MAX_SENDS];
        buses[0] = [left, right];
        for (bus, [l, r]) in buses[1..].iter_mut().zip(&self.sends) {
            *bus = [&l[..n], &r[..n]];
        }
        for (port, out) in self.out_ports.iter_mut().enumerate() {
            let out = out.as_mut_slice(ps);
            self.router.render(port, &buses, passed, out);
            self.ports.record_output(port, out);
        }
        for (port, input) in inputs[..ports].iter().enumerate() {
            self.ports.record_input(port, input);
        }
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
