//! The web remote in the app: Settings → Remote control, the web server
//! while a password is set, and the commands of the phone or tablet,
//! applied on the UI thread with the touchscreen's own functions (as MIDI
//! commands are) so the UI follows.
//!
//! Without a password nothing runs: no server, no timer; the poll timer
//! only asks `tuner_wanted()` and `poll()`, which return at once.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;
use tracing::{error, info, warn};

use pedalboard::audio::ChainNode;
use pedalboard::midi::Step;
use pedalboard::remote::protocol::{
    self, Block, Command, Param, ParamOption, PresetRef, SceneTile, Setlist, SetlistSlot, Snapshot,
    Telemetry, TunerReading,
};
use pedalboard::remote::{Remote, Server, Status};
use pedalboard::state::{AppState, PresetStep, SCENE_COUNT};
use pedalboard::system_config::Helper;
use pedalboard::ui::slint_bridge as views;

use crate::{App, RemoteControl};

/// The state is compared and sent this often while clients are connected
const SNAPSHOT_MS: u64 = 250;
/// Settings card and status file refresh
const STATUS_MS: u64 = 3000;
/// A knob moved on the touchscreen this recently keeps its value
const TOUCH_HOLD: Duration = Duration::from_millis(300);
/// Written for pedalboard-report (set by the launcher)
const STATUS_FILE_ENV: &str = "PEDALBOARD_REMOTE_STATUS";
const HOTSPOT_STATUS: &str = "/run/pedalboard/remote/hotspot";
const HOTSPOT_ID: &str = pedalboard::remote::bootcfg::HOTSPOT_ID;

/// The server and its timers, while a password is set
struct Running {
    _server: Server,
    _snapshot: slint::Timer,
    _status: slint::Timer,
}

thread_local! {
    static REMOTE: RefCell<Option<Arc<Remote>>> = const { RefCell::new(None) };
    static RUNNING: RefCell<Option<Running>> = const { RefCell::new(None) };
    /// Knobs moved on the touchscreen, and when
    static TOUCHED: RefCell<HashMap<(i32, i32), Instant>> = RefCell::new(HashMap::new());
    /// A command of the remote is being applied
    static APPLYING: Cell<bool> = const { Cell::new(false) };
    static TICK: Cell<u32> = const { Cell::new(0) };
}

/// The remote, when it runs
fn active() -> Option<Arc<Remote>> {
    RUNNING
        .with(|r| r.borrow().is_some())
        .then(|| REMOTE.with(|r| r.borrow().clone()))
        .flatten()
}

/// Whether the tuner must measure for a web client
pub fn tuner_wanted() -> bool {
    RUNNING.with(|r| r.borrow().is_some()) && active().is_some_and(|r| r.tuner_wanted())
}

/// From the poll timer: tuner and meters to the clients, 10 times a second
pub fn poll(app: &App, meters: &views::MeterView) {
    if RUNNING.with(|r| r.borrow().is_none()) {
        return;
    }
    let Some(remote) = active().filter(|r| r.clients() > 0) else {
        return;
    };
    let tick = TICK.with(|t| {
        t.set(t.get().wrapping_add(1));
        t.get()
    });
    if !tick.is_multiple_of(2) {
        return;
    }
    let frequency = app.get_tuner_frequency();
    let telemetry = Telemetry {
        tuner: (remote.tuner_wanted() && frequency > 0.0).then(|| TunerReading {
            frequency,
            note: app.get_tuner_note().to_string(),
            octave: app.get_tuner_octave(),
            cents: app.get_tuner_cents(),
            tuned: app.get_tuner_is_tuned(),
        }),
        meters: meters.bars,
        clip_in: meters.clip_in,
        clip_out: meters.clip_out,
    };
    remote.publish_telemetry(protocol::tagged("telemetry", &telemetry));
}

/// A knob was moved by `set-parameter`: from the touchscreen unless a
/// remote command is being applied
pub fn touched(instance_id: i32, index: i32) {
    if APPLYING.with(Cell::get) || RUNNING.with(|r| r.borrow().is_none()) {
        return;
    }
    TOUCHED.with(|t| {
        let mut t = t.borrow_mut();
        t.retain(|_, at| at.elapsed() < TOUCH_HOLD);
        t.insert((instance_id, index), Instant::now());
    });
}

fn held(instance_id: i32, index: i32) -> bool {
    TOUCHED.with(|t| {
        t.borrow()
            .get(&(instance_id, index))
            .is_some_and(|at| at.elapsed() < TOUCH_HOLD)
    })
}

pub fn setup(app: &App, state: &Arc<AppState>, headless: bool) {
    let remote = Remote::new(
        &pedalboard::state::data_dir(),
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            editor: headless,
            port: 0,
        },
    );
    let (weak, st) = (app.as_weak(), state.clone());
    remote.set_dispatcher(Box::new(move |command| {
        let st = st.clone();
        crate::ui(&weak, move |app| apply(app, &st, command));
    }));
    #[cfg(feature = "headless")]
    if headless {
        crate::headless::stream_to(Some(remote.clone()));
    }
    REMOTE.with(|r| *r.borrow_mut() = Some(remote.clone()));

    let global = app.global::<RemoteControl>();
    global.set_available(true);
    global.set_headless(headless);
    global.set_hotspot_mode(hotspot_mode().into());

    let (weak, st) = (app.as_weak(), state.clone());
    global.on_set_password(move |password| {
        let password = password.to_string();
        if let Err(e) = pedalboard::remote::auth::check_password(&password) {
            if let Some(app) = weak.upgrade() {
                show_message(&app, &e.to_string(), true);
            }
            return;
        }
        let Some(remote) = REMOTE.with(|r| r.borrow().clone()) else {
            return;
        };
        let (weak, st) = (weak.clone(), st.clone());
        // Hashing takes a moment: off the UI thread
        crate::spawn("remote-password", move || {
            let saved = remote.set_password(&password);
            if saved.is_ok() {
                hotspot_profile(Some(&password));
            }
            crate::ui(&weak, move |app| match saved {
                Ok(()) => {
                    info!("Remote control password set");
                    start(app, &st);
                    show_message(app, "Password saved: devices sign in again with it", false);
                }
                Err(e) => {
                    error!(
                        error = format_args!("{e:#}"),
                        "Remote control password not saved"
                    );
                    show_message(app, &format!("Not saved: {e}"), true);
                }
            });
        });
    });
    let weak = app.as_weak();
    global.on_turn_off(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(remote) = REMOTE.with(|r| r.borrow().clone()) else {
            return;
        };
        match remote.clear_password() {
            Ok(()) => {
                info!("Remote control turned off");
                stop(&app);
                crate::spawn("remote-hotspot", || hotspot_profile(None));
                show_message(&app, "Remote control off", false);
            }
            Err(e) => show_message(&app, &format!("Not turned off: {e}"), true),
        }
    });
    let weak = app.as_weak();
    global.on_sign_out_all(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(remote) = REMOTE.with(|r| r.borrow().clone()) {
            let n = remote.revoke_all();
            info!(devices = n, "Remote control: devices signed out");
            show_message(&app, "Every device must sign in again", false);
            refresh_card(&app);
        }
    });
    let weak = app.as_weak();
    global.on_set_hotspot_mode(move |mode| {
        let Some(app) = weak.upgrade() else { return };
        let Some(helper) = Helper::from_env() else {
            return;
        };
        match helper.request(&[format!("remote_hotspot={mode}")]) {
            Ok(_) => {
                app.global::<RemoteControl>().set_hotspot_mode(mode.clone());
                info!(%mode, "Hotspot mode changed");
            }
            Err(e) => show_message(&app, &format!("Not changed: {e:#}"), true),
        }
    });
    let weak = app.as_weak();
    global.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            refresh_card(&app);
        }
    });

    if remote.passwords.is_set() {
        start(app, state);
    } else {
        write_status_file("off");
        if headless {
            warn!(
                "Remote control: no password set, and no touchscreen: only footswitches and \
                 MIDI control the pedal. Set remote_password in pedalboard.txt."
            );
        }
    }
}

fn show_message(app: &App, text: &str, error: bool) {
    let global = app.global::<RemoteControl>();
    global.set_message(text.into());
    global.set_message_error(error);
}

/// Start serving (the password is set)
fn start(app: &App, state: &Arc<AppState>) {
    let Some(remote) = REMOTE.with(|r| r.borrow().clone()) else {
        return;
    };
    if RUNNING.with(|r| r.borrow().is_some()) {
        refresh_card(app);
        return;
    }
    let server = match Server::start(remote.clone(), pedalboard::remote::port_from_env()) {
        Ok(server) => server,
        Err(e) => {
            error!(error = format_args!("{e:#}"), "Remote control not started");
            show_message(app, &format!("Not started: {e}"), true);
            return;
        }
    };
    let snapshot = slint::Timer::default();
    let (weak, st, r) = (app.as_weak(), state.clone(), remote.clone());
    snapshot.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(SNAPSHOT_MS),
        move || {
            if r.clients() == 0 {
                return;
            }
            if let Some(app) = weak.upgrade() {
                let snapshot = build_snapshot(&app, &st, r.status().editor);
                r.publish_state(protocol::tagged("state", &snapshot));
            }
        },
    );
    // The state now, for the first client
    remote.publish_state(protocol::tagged(
        "state",
        &build_snapshot(app, state, remote.status().editor),
    ));
    let status = slint::Timer::default();
    let weak = app.as_weak();
    status.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(STATUS_MS),
        move || {
            if let Some(app) = weak.upgrade() {
                refresh_card(&app);
            }
        },
    );
    RUNNING.with(|r| {
        *r.borrow_mut() = Some(Running {
            _server: server,
            _snapshot: snapshot,
            _status: status,
        })
    });
    app.global::<RemoteControl>().set_enabled(true);
    refresh_card(app);
    refresh_addresses(app);
}

fn stop(app: &App) {
    RUNNING.with(|r| r.borrow_mut().take());
    let global = app.global::<RemoteControl>();
    global.set_enabled(false);
    global.set_clients(0);
    global.set_addresses("".into());
    write_status_file("off");
}

fn refresh_card(app: &App) {
    let Some(remote) = active() else { return };
    let global = app.global::<RemoteControl>();
    global.set_clients(remote.clients() as i32);
    let hotspot = std::fs::read_to_string(HOTSPOT_STATUS)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    global.set_hotspot(hotspot.into());
    let status = remote.status();
    write_status_file(&format!(
        "on port={} clients={} screen={}",
        status.port,
        remote.clients(),
        if status.editor { "remote" } else { "display" }
    ));
    // Addresses take nmcli: once in a while
    thread_local!(static LAST: Cell<Option<Instant>> = const { Cell::new(None) });
    if LAST.with(|l| {
        l.get()
            .is_none_or(|t| t.elapsed() > Duration::from_secs(15))
    }) {
        LAST.with(|l| l.set(Some(Instant::now())));
        refresh_addresses(app);
    }
}

fn refresh_addresses(app: &App) {
    let Some(remote) = active() else { return };
    let port = remote.status().port;
    let weak = app.as_weak();
    crate::spawn("remote-addresses", move || {
        let suffix = if port == 80 {
            String::new()
        } else {
            format!(":{port}")
        };
        let mut list: Vec<String> = pedalboard::wifi::addresses()
            .into_iter()
            .map(|a| format!("http://{}{suffix}", a.ip))
            .collect();
        list.dedup();
        crate::ui(&weak, move |app| {
            app.global::<RemoteControl>()
                .set_addresses(list.join("  ").into());
        });
    });
}

fn write_status_file(text: &str) {
    if let Some(path) = std::env::var_os(STATUS_FILE_ENV) {
        let _ = std::fs::write(path, format!("{text}\n"));
    }
}

/// remote_hotspot of pedalboard.txt, as read at boot
fn hotspot_mode() -> String {
    std::fs::read_to_string(pedalboard::system_config::CONFIG_FILE)
        .ok()
        .and_then(|text| {
            text.lines().rev().find_map(|l| {
                l.trim()
                    .strip_prefix("remote_hotspot=")
                    .map(|v| v.trim_matches('\'').to_string())
            })
        })
        .filter(|m| matches!(m.as_str(), "auto" | "on" | "off"))
        .unwrap_or_else(|| "auto".into())
}

/// The hotspot profile follows the password (on the pedalboard OS only):
/// its key is the password, gone with it
fn hotspot_profile(password: Option<&str>) {
    if Helper::from_env().is_none() {
        return;
    }
    let nmcli = |args: &[&str]| {
        // The log never shows the key (the last argument)
        let shown = args
            .iter()
            .take_while(|a| **a != "wifi-sec.psk")
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        match Process::new("nmcli").args(args).output() {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                warn!(
                    args = %shown,
                    error = %String::from_utf8_lossy(&out.stderr).trim(),
                    "nmcli failed"
                );
                false
            }
            Err(e) => {
                warn!(args = %shown, error = %e, "nmcli not run");
                false
            }
        }
    };
    let exists = Process::new("nmcli")
        .args(["-t", "-f", "NAME", "connection", "show"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == HOTSPOT_ID)
        })
        .unwrap_or(false);
    match password {
        None => {
            if exists {
                nmcli(&["connection", "delete", "id", HOTSPOT_ID]);
            }
        }
        Some(password) if exists => {
            if nmcli(&[
                "connection",
                "modify",
                "id",
                HOTSPOT_ID,
                "wifi-sec.psk",
                password,
            ]) {
                // Up again with the new key when it is up
                let active = Process::new("nmcli")
                    .args(["-t", "-f", "NAME", "connection", "show", "--active"])
                    .output()
                    .map(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .any(|l| l == HOTSPOT_ID)
                    })
                    .unwrap_or(false);
                if active {
                    nmcli(&["connection", "up", "id", HOTSPOT_ID]);
                }
            }
        }
        Some(password) => {
            nmcli(&[
                "connection",
                "add",
                "type",
                "wifi",
                "ifname",
                "*",
                "con-name",
                HOTSPOT_ID,
                "autoconnect",
                "no",
                "ssid",
                "OpenRig",
                "mode",
                "ap",
                "802-11-wireless.band",
                "bg",
                "ipv4.method",
                "shared",
                "ipv4.addresses",
                "10.42.0.1/24",
                "ipv6.method",
                "disabled",
                "wifi-sec.key-mgmt",
                "wpa-psk",
                "wifi-sec.proto",
                "rsn",
                "wifi-sec.pairwise",
                "ccmp",
                "wifi-sec.group",
                "ccmp",
                "wifi-sec.pmf",
                "disable",
                "wifi-sec.psk",
                password,
            ]);
        }
    }
}

/// Apply a command of the remote, on the UI thread
fn apply(app: &App, state: &AppState, command: Command) {
    APPLYING.with(|a| a.set(true));
    run(app, state, command);
    APPLYING.with(|a| a.set(false));
}

/// The block `block` still is plugin `uri` (no preset loaded meanwhile)
fn same_block(state: &AppState, block: usize, uri: &str) -> bool {
    crate::block_uri(&state.chain(), block).is_some_and(|u| u == uri)
}

fn run(app: &App, state: &AppState, command: Command) {
    match command {
        Command::LoadPreset { id } => {
            if state.current_preset_id().as_deref() != Some(id.as_str()) {
                crate::load_preset(app, state, &id);
            }
        }
        Command::PresetStep { next } => {
            let step = if next { Step::Next } else { Step::Previous };
            match state.preset_step(step) {
                Some(PresetStep::Slot(slot)) => crate::setlist_slot(app, state, slot),
                Some(PresetStep::Preset(id)) => crate::load_preset(app, state, &id),
                None => {}
            }
        }
        Command::SetlistSlot { slot } => crate::setlist_slot(app, state, slot),
        Command::SelectScene { slot } if slot < SCENE_COUNT => {
            crate::live_ui::select_scene(app, state, slot)
        }
        Command::SelectScene { .. } => {}
        Command::ToggleBypass { block, uri } => {
            if same_block(state, block, &uri) {
                app.invoke_toggle_node_bypass(block as i32);
                crate::live_ui::refresh(app, state);
            }
        }
        Command::SetParameter {
            block,
            uri,
            index,
            value,
        } => {
            let (Ok(id), Ok(index)) = (i32::try_from(block), i32::try_from(index)) else {
                return;
            };
            // A knob held on the touchscreen wins
            if same_block(state, block, &uri) && !held(id, index) {
                app.invoke_set_parameter(id, index, value);
            }
        }
        Command::Tap => crate::live_ui::tap(app, state),
        Command::SetTempo { bpm } => {
            let bpm = state.set_tempo(bpm);
            crate::live_ui::show_tempo(app, state, bpm);
        }
        #[cfg(feature = "headless")]
        input @ (Command::Pointer { .. } | Command::Key { .. }) => crate::headless::input(input),
        _ => {}
    }
}

/// Everything the web clients show
fn build_snapshot(app: &App, state: &AppState, editor: bool) -> Snapshot {
    let chain = state.chain();
    let current = state.current_preset_id();
    let setlist = state.current_scene_id().map(|id| {
        let name = state
            .list_scenes()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.name)
            .unwrap_or_default();
        Setlist {
            id,
            name,
            slots: views::scene_slot_views(state)
                .into_iter()
                .map(|s| SetlistSlot {
                    slot: s.slot,
                    name: s.preset_name,
                    empty: s.is_empty,
                    active: s.active,
                })
                .collect(),
        }
    });
    let blocks = chain
        .plugins()
        .into_iter()
        .filter_map(|node| match node {
            ChainNode::Plugin {
                instance_id,
                uri,
                enabled,
                ..
            } => Some((*instance_id, uri.clone(), *enabled)),
            ChainNode::Split { .. } => None,
        })
        .map(|(id, uri, enabled)| {
            let params = views::selected_node_view(state, id)
                .map(|view| {
                    view.parameters
                        .into_iter()
                        .map(|p| Param {
                            index: p.index,
                            symbol: p.symbol,
                            name: p.name,
                            min: p.min,
                            max: p.max,
                            default: p.default,
                            value: p.value,
                            unit: p.unit,
                            kind: p.kind.as_str().to_string(),
                            options: p
                                .options
                                .into_iter()
                                .zip(p.option_values)
                                .map(|(label, value)| ParamOption { label, value })
                                .collect(),
                            strip: p.in_strip,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let tint = crate::block_tint(state, &chain, id, &uri);
            Block {
                id,
                name: crate::block_name(state, &chain, id, &uri),
                color: format!("#{:06x}", tint & 0xff_ffff),
                bypassed: !enabled,
                params,
                uri,
            }
        })
        .collect();
    Snapshot {
        version: env!("CARGO_PKG_VERSION").to_string(),
        editor,
        audio: app.get_audio_running(),
        preset: current.map(|id| PresetRef {
            id,
            name: state.current_preset_name(),
        }),
        presets: state
            .list_presets()
            .into_iter()
            .map(|p| PresetRef {
                id: p.id,
                name: p.name,
            })
            .collect(),
        setlist,
        scenes: (0..SCENE_COUNT)
            .map(|slot| SceneTile {
                slot,
                name: chain.scene_name(slot),
                used: chain.scene_used(slot),
                active: slot == chain.scene,
            })
            .collect(),
        scene_name: chain.scene_name(chain.scene),
        bpm: chain.bpm,
        blocks,
        status: app.get_status_message().to_string(),
    }
}
