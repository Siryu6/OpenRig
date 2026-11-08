//! Settings → MIDI → Network MIDI, on the pedalboard OS: switch rtpmidid on
//! or off and name its session (through the root helper), follow the
//! sessions computers open (rtpmidid's control socket, polled by a worker
//! thread), and gate them: a session is listened to only once its device
//! is allowed, a new device is asked about on the screen, a blocked one or
//! one outside the local network is dropped. `pedalboard::network_midi`
//! has the rules; the MIDI thread asks the `Gate`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use slint::{ComponentHandle, SharedString, Weak};
use tracing::{error, info, warn};

use pedalboard::network_midi::{
    self, Decision, Device, Devices, Gate, Mode, Session, Status, CONTROL_SOCKET, PORT,
};
use pedalboard::system_config::{self, Helper};

use crate::{model, ui, App, NetMidiDevice};

/// Set by the OS image: where the root helper keeps the switch and the name
const STATE_ENV: &str = "PEDALBOARD_NETWORK_MIDI_DIR";
/// Set by the OS image: the sessions and decisions, for pedalboard-report
const STATUS_ENV: &str = "PEDALBOARD_NETWORK_MIDI_STATUS";
/// How often rtpmidid is asked for its sessions
const POLL: Duration = Duration::from_secs(1);
/// Longest wait for the helper
const TIMEOUT: Duration = Duration::from_secs(10);

enum Command {
    /// Drop sessions: their peers in rtpmidid's router
    Remove(Vec<u64>),
}

struct NetMidi {
    helper: Helper,
    status_file: Option<PathBuf>,
    hostname: String,
    enabled: bool,
    /// Session name set; empty: the host name
    name: String,
    devices: Devices,
    /// What rtpmidid last reported; `Err` while it does not answer
    status: Result<Status, String>,
    /// Device asked about on the screen
    prompt: Option<Device>,
    /// Devices answered "later": not asked again while connected
    later: BTreeSet<Device>,
    /// Peers already removed (until rtpmidid drops them from its status)
    removed: BTreeSet<u64>,
    /// Sessions already logged
    logged: BTreeSet<[u64; 2]>,
    /// What the status file holds
    written: String,
    gate: Arc<Gate>,
    polling: Arc<AtomicBool>,
    worker: mpsc::Sender<Command>,
    /// Request to the helper being followed: id, since, on, name
    request: Option<(String, Instant, bool, String)>,
    timer: slint::Timer,
}

thread_local! {
    static NET_MIDI: RefCell<Option<NetMidi>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut NetMidi) -> R) -> Option<R> {
    NET_MIDI.with(|n| n.borrow_mut().as_mut().map(f))
}

/// On the pedalboard OS: the card, and the gate the MIDI thread asks.
/// Elsewhere network MIDI is the system's: `None`.
pub fn setup(app: &App) -> Option<Arc<Gate>> {
    let helper = Helper::from_env()?;
    let state_dir = PathBuf::from(std::env::var_os(STATE_ENV)?);
    let status_file = std::env::var_os(STATUS_ENV).map(PathBuf::from);
    let read = |file: &str| {
        std::fs::read_to_string(state_dir.join(file))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let enabled = read("enabled") == "on";
    let name = Some(read("name"))
        .filter(|n| network_midi::valid_session_name(n))
        .unwrap_or_default();
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "pedalboard".into());
    let devices = Devices::load(&data_dir()).unwrap_or_else(|e| {
        warn!(
            error = format_args!("{e:#}"),
            "Network MIDI devices not read: asking again"
        );
        Devices::default()
    });

    let gate = Arc::new(Gate::default());
    let polling = Arc::new(AtomicBool::new(enabled));
    let (tx, rx) = mpsc::channel();
    let (weak, on) = (app.as_weak(), polling.clone());
    if !crate::spawn("network-midi", move || worker(rx, on, weak)) {
        return Some(gate);
    }
    NET_MIDI.with(|n| {
        *n.borrow_mut() = Some(NetMidi {
            helper,
            status_file,
            hostname,
            enabled,
            name,
            devices,
            status: Err(String::new()),
            prompt: None,
            later: BTreeSet::new(),
            removed: BTreeSet::new(),
            logged: BTreeSet::new(),
            written: String::new(),
            gate: gate.clone(),
            polling,
            worker: tx,
            request: None,
            timer: slint::Timer::default(),
        })
    });
    app.set_netmidi_available(true);
    info!(enabled, "Network MIDI");

    let weak = app.as_weak();
    app.on_netmidi_set_enabled(move |on| switch(&weak, on, None));
    let weak = app.as_weak();
    app.on_netmidi_rename(move |name| {
        let on = with(|n| n.enabled).unwrap_or_default();
        switch(&weak, on, Some(name.trim().to_string()))
    });
    let weak = app.as_weak();
    app.on_netmidi_set_allow_all(move |all| {
        change(&weak, |devices| {
            devices.mode = if all { Mode::AllowAll } else { Mode::Ask };
            info!(mode = ?devices.mode, "Network MIDI: new devices");
        })
    });
    let weak = app.as_weak();
    app.on_netmidi_allow(move |name, address| {
        let Some(device) = device(&name, &address) else {
            return;
        };
        info!(device = %name, %address, "Network MIDI device allowed");
        change(&weak, |devices| devices.allow(device));
    });
    let weak = app.as_weak();
    app.on_netmidi_block(move |name, address| {
        let Some(device) = device(&name, &address) else {
            return;
        };
        info!(device = %name, %address, "Network MIDI device blocked");
        change(&weak, |devices| devices.block(device));
    });
    let weak = app.as_weak();
    app.on_netmidi_forget(move |name, address| {
        let Some(device) = device(&name, &address) else {
            return;
        };
        info!(device = %name, %address, "Network MIDI device forgotten");
        change(&weak, |devices| devices.forget(&device));
    });
    let weak = app.as_weak();
    app.on_netmidi_later(move || {
        with(|n| {
            if let Some(device) = n.prompt.take() {
                n.later.insert(device);
            }
        });
        if let Some(app) = weak.upgrade() {
            refresh(&app);
        }
    });
    refresh(app);
    Some(gate)
}

fn data_dir() -> PathBuf {
    pedalboard::state::app_state::data_dir()
}

fn device(name: &str, address: &str) -> Option<Device> {
    let ip: IpAddr = address.parse().ok()?;
    Some(Device::new(name, ip))
}

/// Change the devices or the mode, save them, apply them to the sessions
fn change(app: &Weak<App>, f: impl FnOnce(&mut Devices)) {
    let Some(app) = app.upgrade() else { return };
    let saved = with(|n| {
        f(&mut n.devices);
        n.prompt = None;
        n.devices.save(&data_dir())
    });
    if let Some(Err(e)) = saved {
        error!(
            error = format_args!("{e:#}"),
            "Network MIDI devices not saved"
        );
        app.set_netmidi_message(format!("Not saved: {e}").into());
    }
    refresh(&app);
}

/// Switch on or off, or rename (`name`), through the root helper
fn switch(app: &Weak<App>, on: bool, name: Option<String>) {
    let Some(app) = app.upgrade() else { return };
    let message = with(|n| {
        if n.request.is_some() {
            return None;
        }
        let mut name = name.unwrap_or_else(|| n.name.clone());
        if name == n.hostname {
            name.clear();
        }
        if !network_midi::valid_session_name(&name) {
            return Some(format!(
                "Up to {} letters, digits, spaces, dots, dashes or underscores",
                network_midi::NAME_MAX
            ));
        }
        match n.helper.network_midi(on, &name) {
            Ok(id) => {
                info!(on, session = %name, "Network MIDI change requested");
                n.request = Some((id, Instant::now(), on, name));
                let weak = app.as_weak();
                n.timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(200),
                    move || {
                        if let Some(app) = weak.upgrade() {
                            poll_helper(&app);
                        }
                    },
                );
                None
            }
            Err(e) => {
                error!(error = format_args!("{e:#}"), "Network MIDI not switched");
                Some(e.to_string())
            }
        }
    })
    .flatten();
    app.set_netmidi_message(message.unwrap_or_default().into());
    refresh(&app);
}

/// Follow the helper until it is done
fn poll_helper(app: &App) {
    let done = with(|n| {
        let (id, since, on, name) = n.request.clone()?;
        let status = match n.helper.status(&id) {
            Some(status) => status,
            None if since.elapsed() > TIMEOUT => {
                system_config::Status::Failed("The settings helper did not answer".into())
            }
            None => return None,
        };
        n.request = None;
        n.timer.stop();
        Some(match status {
            system_config::Status::Failed(message) => {
                error!(error = %message, "Network MIDI not switched");
                message
            }
            _ => {
                n.enabled = on;
                n.name = name;
                n.polling.store(on, Ordering::Relaxed);
                n.status = Err(String::new());
                n.prompt = None;
                n.later.clear();
                n.removed.clear();
                n.gate.set_allowed(&[]);
                String::new()
            }
        })
    })
    .flatten();
    if let Some(message) = done {
        app.set_netmidi_message(message.into());
        refresh(app);
    }
}

/// Polls rtpmidid while network MIDI is on, and removes the sessions asked
fn worker(rx: Receiver<Command>, polling: Arc<AtomicBool>, app: Weak<App>) {
    let socket = std::path::Path::new(CONTROL_SOCKET);
    let mut id = 0u64;
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Command::Remove(peers)) => {
                for peer in peers {
                    id += 1;
                    let request =
                        network_midi::request(id, "router.remove", serde_json::json!([peer]));
                    if let Err(e) = network_midi::ask(socket, &request) {
                        warn!(
                            peer,
                            error = format_args!("{e:#}"),
                            "Network MIDI session not removed"
                        );
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !polling.load(Ordering::Relaxed) {
            continue;
        }
        id += 1;
        let request = network_midi::request(id, "status", serde_json::Value::Null);
        let status = network_midi::ask(socket, &request)
            .and_then(|answer| network_midi::parse_status(&answer))
            .map_err(|e| format!("{e:#}"));
        ui(&app, move |app| {
            with(|n| {
                if n.enabled {
                    n.status = status;
                }
            });
            refresh(app);
        });
    }
}

/// What a session is shown as
fn state_name(decision: Decision) -> &'static str {
    match decision {
        Decision::Allow => "allowed",
        Decision::Ask => "waiting",
        Decision::Block => "blocked",
        Decision::Outside => "outside",
    }
}

fn address(session: &Session) -> String {
    session.ip.map_or_else(|| "?".into(), |ip| ip.to_string())
}

/// Apply the devices to the sessions (gate, removals, prompt) and show it all
fn refresh(app: &App) {
    let shown = with(|n| {
        let sessions: Vec<(Session, Decision)> = match &n.status {
            Ok(status) => status
                .sessions
                .iter()
                .map(|s| (s.clone(), n.devices.decide(s)))
                .collect(),
            Err(_) => Vec::new(),
        };

        // Listened to: the allowed ones only
        let allowed: Vec<&Session> = sessions
            .iter()
            .filter(|(_, d)| *d == Decision::Allow)
            .map(|(s, _)| s)
            .collect();
        n.gate.set_allowed(&allowed);

        // Blocked or from outside: dropped
        let current: BTreeSet<u64> = sessions.iter().flat_map(|(s, _)| s.peers).collect();
        n.removed.retain(|p| current.contains(p));
        let mut remove = Vec::new();
        for (session, decision) in &sessions {
            if matches!(decision, Decision::Block | Decision::Outside)
                && !n.removed.contains(&session.peers[0])
            {
                warn!(device = %session.name, address = %address(session), decision = state_name(*decision), "Network MIDI session dropped");
                n.removed.extend(session.peers);
                remove.extend(session.peers);
            }
        }
        if !remove.is_empty() {
            // The worker only stops with the app
            let _ = n.worker.send(Command::Remove(remove));
        }

        // Logged once each
        let keys: BTreeSet<[u64; 2]> = sessions.iter().map(|(s, _)| s.peers).collect();
        for (session, decision) in &sessions {
            if n.logged.insert(session.peers) {
                info!(device = %session.name, address = %address(session), udp_port = session.udp_port, decision = state_name(*decision), "Network MIDI session");
            }
        }
        n.logged.retain(|k| keys.contains(k));

        // Asked about: the first waiting device not put off
        let waiting: Vec<Device> = sessions
            .iter()
            .filter(|(_, d)| *d == Decision::Ask)
            .filter_map(|(s, _)| s.device())
            .collect();
        n.later.retain(|d| waiting.contains(d));
        if n.prompt.as_ref().is_some_and(|d| !waiting.contains(d)) {
            n.prompt = None;
        }
        if n.prompt.is_none() {
            n.prompt = waiting.iter().find(|d| !n.later.contains(d)).cloned();
        }

        let summary = match &n.status {
            Ok(status) => match status.announced.first() {
                Some((name, port)) => format!(
                    "Announced as \"{name}\" on UDP {port}–{}. {} connected.",
                    port + 1,
                    sessions.len()
                ),
                None => format!(
                    "Listening on UDP {PORT}–{} (not announced yet). {} connected.",
                    PORT + 1,
                    sessions.len()
                ),
            },
            Err(_) => "Starting…".into(),
        };
        write_status(n, &summary, &sessions);

        let sessions: Vec<NetMidiDevice> = sessions
            .iter()
            .map(|(s, d)| NetMidiDevice {
                name: s.name.clone().into(),
                address: address(s).into(),
                state: state_name(*d).into(),
            })
            .collect();
        let remembered = |list: &[Device], state: &str| {
            list.iter()
                .rev()
                .map(|d| NetMidiDevice {
                    name: d.name.clone().into(),
                    address: d.address.to_string().into(),
                    state: state.into(),
                })
                .collect::<Vec<_>>()
        };
        let mut devices = remembered(&n.devices.allowed, "allowed");
        devices.extend(remembered(&n.devices.blocked, "blocked"));
        (
            n.enabled,
            n.request.is_some(),
            n.name.clone(),
            n.hostname.clone(),
            summary,
            sessions,
            devices,
            n.devices.mode == Mode::AllowAll,
            n.prompt.clone(),
        )
    });
    let Some((enabled, busy, name, hostname, summary, sessions, devices, all, prompt)) = shown
    else {
        return;
    };
    app.set_netmidi_enabled(enabled);
    app.set_netmidi_busy(busy);
    app.set_netmidi_name(name.into());
    app.set_netmidi_hostname(hostname.into());
    app.set_netmidi_summary(summary.into());
    if !same(&app.get_netmidi_sessions(), &sessions) {
        app.set_netmidi_sessions(model(sessions));
    }
    if !same(&app.get_netmidi_devices(), &devices) {
        app.set_netmidi_devices(model(devices));
    }
    app.set_netmidi_allow_all(all);
    let (name, address) = prompt.map_or_else(Default::default, |d| {
        (SharedString::from(d.name), d.address.to_string().into())
    });
    app.set_netmidi_prompt_name(name);
    app.set_netmidi_prompt_address(address);
}

/// Whether the model already shows `items` (not rebuilt every second)
fn same(model: &slint::ModelRc<NetMidiDevice>, items: &[NetMidiDevice]) -> bool {
    use slint::Model;
    model.row_count() == items.len() && model.iter().zip(items).all(|(a, b)| a == *b)
}

/// The sessions and what is done with them, for pedalboard-report: names
/// and addresses only
fn write_status(n: &mut NetMidi, summary: &str, sessions: &[(Session, Decision)]) {
    let Some(file) = &n.status_file else { return };
    let mut text = format!(
        "rtpmidid: {}\nnew devices: {}\n",
        if n.enabled { summary } else { "off" },
        match n.devices.mode {
            Mode::Ask => "ask",
            Mode::AllowAll => "allow anyone on the local network",
        }
    );
    for (session, decision) in sessions {
        text.push_str(&format!(
            "session \"{}\" from {} port {}: {}\n",
            session.name,
            address(session),
            session.udp_port,
            state_name(*decision)
        ));
    }
    if text != n.written {
        if let Err(e) = pedalboard::persist::write_atomic(file, text.as_bytes()) {
            warn!(
                error = format_args!("{e:#}"),
                "Network MIDI status not written"
            );
        }
        n.written = text;
    }
}
