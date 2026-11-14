use crate::sync::MutexExt;
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use jack::{
    Client, ClientOptions, Control, LatencyType, NotificationHandler, Port, ProcessHandler,
    ProcessScope, Unowned,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::chain_switcher::ChainControl;
use super::latency::{LatencyReport, OutputSource};
use super::routing::{OutputRouter, PortMeter, Routing, RoutingControl, MAX_PORTS, MAX_SENDS};
use super::{ChainSwitcher, Looper, Tuner, TunerResult};
use crate::monitor::{DspMeter, LevelMeter};
use crate::plugins::chain::ChainIo;
use crate::plugins::guard::silence_non_finite;
use crate::plugins::Lv2Host;

/// Preset/chain switch crossfade
const CROSSFADE_MS: f32 = 20.0;
/// How often a change of latency is looked for, to tell JACK
const LATENCY_POLL: Duration = Duration::from_millis(250);

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
    /// Latency of the effects and of each output
    pub latency: Arc<LatencyReport>,
    /// Tells JACK when the latency changes. After `client`: dropped once
    /// the client is deactivated, then closes it (its ports hold it open)
    _latency_watch: LatencyWatch,
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
        let (client, _status) = Client::new(&self.client_name, ClientOptions::NO_START_SERVER)
            .context("Failed to create JACK client")?;

        let sample_rate = client.sample_rate() as f32;
        let buffer_size = client.buffer_size() as usize;
        info!(
            client = %self.client_name,
            sample_rate,
            buffer_size,
            "JACK client created"
        );

        // Tuner and looper depend on the server's sample rate
        *tuner.locked() = Tuner::new(sample_rate);
        {
            let mut looper = looper.locked();
            *looper = looper.with_sample_rate(sample_rate);
        }

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
                // Cosmetic: other clients find the port by its name anyway
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
        info!(inputs, outputs, "Ports registered");
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
        let latency = Arc::new(LatencyReport::default());
        let latency_ports = Box::new(LatencyPorts {
            report: latency.clone(),
            inputs: unowned(&client, &in_ports)?,
            outputs: unowned(&client, &out_ports)?,
        });
        // SAFETY: a client not yet active; the callback's data lives until
        // the client is closed, which its ports delay (see `LatencyPorts`)
        let set = unsafe {
            jack_sys::jack_set_latency_callback(
                client.raw(),
                Some(latency_callback),
                &*latency_ports as *const LatencyPorts as *mut std::ffi::c_void,
            )
        };
        if set != 0 {
            warn!("JACK refused the latency callback: other clients will not see the effects' latency");
        }
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
            latency: latency.clone(),
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
        let latency_watch = LatencyWatch::start(client.as_client(), latency.clone(), latency_ports);

        info!("JACK client active");
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
            latency,
            _latency_watch: latency_watch,
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
    info!(
        inputs = %list(captures),
        outputs = %list(playbacks),
        "Audio interface ports"
    );
    if std::env::var_os("PEDALBOARD_NO_AUTOCONNECT").is_some() {
        info!("Ports left unconnected (PEDALBOARD_NO_AUTOCONNECT)");
        return;
    }
    if captures.len().max(playbacks.len()) > MAX_PORTS {
        warn!(
            max = MAX_PORTS,
            "Only the first inputs and outputs of the interface are used"
        );
    }
    let mut connected = Vec::new();
    let mut connect =
        |source: &str, destination: &str| match client.connect_ports_by_name(source, destination) {
            Ok(()) => connected.push(format!("{source} → {destination}")),
            Err(e) => warn!(source, destination, error = %e, "Ports not connected"),
        };
    for (i, capture) in captures.iter().take(MAX_PORTS).enumerate() {
        connect(capture, &format!("{name}:in_{}", i + 1));
    }
    for (i, playback) in playbacks.iter().take(MAX_PORTS).enumerate() {
        connect(&format!("{name}:out_{}", i + 1), playback);
    }
    info!(connections = %list(&connected), "Ports connected");
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

impl ActiveJack {
    /// Input to output, frames, through the effects: what the capture
    /// side, the effects and the limiter, and the playback side add up to
    /// on the first output playing the effects. None when none does.
    pub fn round_trip_latency(&self) -> Option<u32> {
        let ports = &self._latency_watch._ports;
        let (output, own) = ports
            .outputs
            .iter()
            .enumerate()
            .map(|(i, port)| (port, self.latency.output(i)))
            .find(|(_, own)| own.source == OutputSource::Effects)?;
        let capture = ports
            .inputs
            .get(own.input)
            .map_or(0, |input| range(input, LatencyType::Capture).1);
        let playback = range(output, LatencyType::Playback).1;
        Some(capture + own.max + playback)
    }
}

/// What the latency callback reads: the latencies, and the pedalboard's
/// ports. Each port holds the client open, so the client closes only once
/// this is dropped: the callback never outlives it.
struct LatencyPorts {
    report: Arc<LatencyReport>,
    inputs: Vec<Port<Unowned>>,
    outputs: Vec<Port<Unowned>>,
}

/// Handles of `ports` that another thread can read (the process callback
/// owns the ports themselves)
fn unowned<PS>(client: &Client, ports: &[Port<PS>]) -> Result<Vec<Port<Unowned>>> {
    ports
        .iter()
        .map(|port| {
            let name = port.name().context("Port without a name")?;
            client
                .port_by_name(&name)
                .with_context(|| format!("Port {name} not found"))
        })
        .collect()
}

/// Latency range of `port`; nothing for a port gone
fn range(port: &Port<Unowned>, mode: LatencyType) -> (u32, u32) {
    port.get_latency_range(mode).unwrap_or((0, 0))
}

/// JACK asks for the latency of the ports: an output is as late as the
/// input it comes from, plus the effects (or the send) and its limiter;
/// an input is as early as the outputs it feeds need
unsafe extern "C" fn latency_callback(
    mode: jack_sys::jack_latency_callback_mode_t,
    arg: *mut std::ffi::c_void,
) {
    // SAFETY: the `LatencyPorts` registered with the callback, alive as
    // long as the client
    let Some(ports) = (unsafe { (arg as *const LatencyPorts).as_ref() }) else {
        return;
    };
    let report = &ports.report;
    // A port gone has no latency to set
    if mode == jack_sys::JackCaptureLatency {
        let mode = LatencyType::Capture;
        for (index, port) in ports.outputs.iter().enumerate() {
            let own = report.output(index);
            let (min, max) = ports
                .inputs
                .get(own.input)
                .map_or((0, 0), |input| range(input, mode));
            let _ = port.set_latency_range(mode, (min + own.min, max + own.max));
        }
    } else {
        let mode = LatencyType::Playback;
        for (index, port) in ports.inputs.iter().enumerate() {
            let mut fed: Option<(u32, u32)> = None;
            for (out, out_port) in ports.outputs.iter().enumerate() {
                let own = report.output(out);
                if own.source == OutputSource::Silent || own.input != index {
                    continue;
                }
                let (min, max) = range(out_port, mode);
                let (min, max) = (min + own.min, max + own.max);
                fed = Some(fed.map_or((min, max), |(a, b)| (a.min(min), b.max(max))));
            }
            let _ = port.set_latency_range(mode, fed.unwrap_or((0, 0)));
        }
    }
}

/// Thread telling JACK to ask for the latencies again whenever they change
struct LatencyWatch {
    /// The client while it is open; None tells the thread to stop
    client: Arc<Mutex<Option<usize>>>,
    // Read by the latency callback; holds the client open (dropped after
    // `Drop` stopped the thread)
    _ports: Box<LatencyPorts>,
}

impl LatencyWatch {
    fn start(client: &Client, report: Arc<LatencyReport>, ports: Box<LatencyPorts>) -> Self {
        let shared = Arc::new(Mutex::new(Some(client.raw() as usize)));
        let open = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("jack-latency".to_string())
            .spawn(move || {
                let mut seen = report.version();
                loop {
                    std::thread::sleep(LATENCY_POLL);
                    let client = open.locked();
                    let Some(raw) = *client else { break };
                    let version = report.version();
                    if version != seen {
                        seen = version;
                        // SAFETY: the client is open while the lock is held:
                        // `Drop` takes it before `_ports`, which keep the
                        // client open, are dropped
                        unsafe {
                            jack_sys::jack_recompute_total_latencies(
                                raw as *mut jack_sys::jack_client_t,
                            )
                        };
                    }
                }
            });
        if let Err(e) = spawned {
            warn!(error = %e, "No thread to report latency changes to JACK");
        }
        Self {
            client: shared,
            _ports: ports,
        }
    }
}

impl Drop for LatencyWatch {
    fn drop(&mut self) {
        self.client.locked().take();
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
/// on the outputs) → output EQ and limiter
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
    /// Where the latency of each output goes
    latency: Arc<LatencyReport>,
}

impl ProcessHandler for PedalboardProcessor {
    fn process(&mut self, _client: &Client, ps: &ProcessScope) -> Control {
        let start = Instant::now();
        super::flush_denormals();
        self.router.update();
        // The router keeps the input within the ports; the first otherwise
        let Some(port) = self
            .in_ports
            .get(self.router.chain_input())
            .or_else(|| self.in_ports.first())
        else {
            return Control::Continue;
        };
        let input = port.as_slice(ps);
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
            let mut io = ChainIo::new(inputs.get(..heard).unwrap_or(&[]), &mut self.sends);
            self.chain_switcher.process_io(left, right, &mut io)
        };

        // Looper sits after the effects so loops are recorded with the tone
        if let Ok(mut looper) = self.looper.try_lock() {
            looper.process(left, right);
        }
        // Each block's output is checked by its guard; this is the last
        // check before the speakers, for what no block puts out (a sum
        // that overflows, the looper)
        silence_non_finite(&mut [&mut *left, &mut *right]);
        for [l, r] in &mut self.sends {
            silence_non_finite(&mut [&mut l[..n], &mut r[..n]]);
        }
        let passed = |i: u8| inputs.get(..heard)?.get(i as usize).copied();
        // The main mix, then each send
        let mut buses: [[&[f32]; 2]; 1 + MAX_SENDS] = [[&[], &[]]; 1 + MAX_SENDS];
        buses[0] = [left, right];
        for (bus, [l, r]) in buses[1..].iter_mut().zip(&self.sends) {
            *bus = [&l[..n], &r[..n]];
        }
        // Every output filled, then the EQ and limiter of each output or
        // pair (a pair's limiter sees both sides)
        let count = self.out_ports.len().min(MAX_PORTS);
        let mut outs: [&mut [f32]; MAX_PORTS] = Default::default();
        for ((port, out), slot) in self.out_ports.iter_mut().enumerate().zip(outs.iter_mut()) {
            let out = out.as_mut_slice(ps);
            self.router.render(port, &buses, passed, out);
            *slot = out;
        }
        self.router.master(&mut outs[..count], &self.ports);
        for (port, out) in outs[..count].iter().enumerate() {
            self.ports.record_output(port, out);
        }
        for (port, input) in inputs.iter().take(ports).enumerate() {
            self.ports.record_input(port, input);
        }
        self.levels.record(input, left, right);

        // Latency of the effects and of every output, for JACK and the UI
        let effects = self.chain_switcher.latency();
        self.latency.set_chain(effects.chain);
        for port in 0..count {
            let latency = self.router.output_latency(port, &effects);
            self.latency.set_output(port, latency);
        }

        // Running the old and the new chain together while switching costs
        // more than either: left out so the load shown is the chain's
        if !switching {
            self.meter
                .record(start.elapsed(), self.period_per_frame * n as u32);
        }
        Control::Continue
    }
}
