//! Use a phone or tablet as the pedal's screen: a web app served by the
//! pedal, for boards without a touchscreen (or without any screen).
//!
//! Off until a password is set (pedalboard.txt or Settings → Remote
//! control): nothing listens on the network before. Two views share one
//! WebSocket:
//!
//! - the full editor, the pedal's own UI drawn in memory and streamed as
//!   pictures (pointer and keys go back), when the app runs without a
//!   touchscreen;
//! - the live view, a portrait page for the stage: presets, scenes,
//!   blocks and knobs, tuner, tap tempo.
//!
//! Commands from both are applied on the UI thread by the functions the
//! touchscreen uses (see the app's `remote_ui`), as MIDI commands are.

pub mod auth;
pub mod bootcfg;
pub mod display;
mod flow;
pub mod frames;
pub mod net;
pub mod protocol;
mod server;
mod web;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};

use crate::sync::MutexExt;
pub use protocol::{ClientMessage, Command, Snapshot, Telemetry};
pub use server::Server;

/// Port of the web remote: 80, so the address alone opens it; another one
/// when 80 cannot be used (PEDALBOARD_REMOTE_PORT, or 8080 as a fallback)
pub const DEFAULT_PORT: u16 = 80;
pub const FALLBACK_PORT: u16 = 8080;

/// The frames of one picture of the full editor
pub type Picture = Arc<[frames::Frame]>;

/// Runs a command on the UI thread
pub type Dispatcher = Box<dyn Fn(Command) + Send + Sync>;

/// Facts shown on the status page
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Status {
    pub version: String,
    /// The app runs without a touchscreen: the full editor is the screen
    pub editor: bool,
    pub port: u16,
}

/// What the web server and the app share
pub struct Remote {
    pub passwords: auth::PasswordStore,
    pub sessions: auth::Sessions,
    limiter: auth::LoginLimiter,
    /// Last state sent to the clients (JSON)
    state: watch::Sender<Arc<str>>,
    telemetry: broadcast::Sender<Arc<str>>,
    frames: broadcast::Sender<Picture>,
    /// Bumped when sessions are revoked: connections check theirs again
    revoked: watch::Sender<u64>,
    clients: AtomicUsize,
    tuner_clients: AtomicUsize,
    stream_clients: AtomicUsize,
    /// What the pages asked of the next picture
    requests: Mutex<frames::Requests>,
    dispatcher: Mutex<Option<Arc<Dispatcher>>>,
    status: Mutex<Status>,
    /// Names of the pedal accepted in the Host header
    names: Vec<String>,
    /// Where backups are read from and restored to
    data_dir: std::path::PathBuf,
    /// Called once a backup uploaded is ready: the app restarts to restore it
    restore_hook: Mutex<Option<Arc<RestoreHook>>>,
}

/// Run on the UI thread's behalf once a restore is staged
pub type RestoreHook = Box<dyn Fn() + Send + Sync>;

impl Remote {
    pub fn new(data_dir: &std::path::Path, status: Status) -> Arc<Self> {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        Arc::new(Self {
            passwords: auth::PasswordStore::open(data_dir),
            sessions: auth::Sessions::default(),
            limiter: auth::LoginLimiter::default(),
            state: watch::Sender::new(Arc::from("")),
            telemetry: broadcast::Sender::new(8),
            frames: broadcast::Sender::new(16),
            revoked: watch::Sender::new(0),
            clients: AtomicUsize::new(0),
            tuner_clients: AtomicUsize::new(0),
            stream_clients: AtomicUsize::new(0),
            requests: Mutex::new(frames::Requests::default()),
            dispatcher: Mutex::new(None),
            status: Mutex::new(status),
            names: net::local_names(&hostname),
            data_dir: data_dir.to_path_buf(),
            restore_hook: Mutex::new(None),
        })
    }

    /// Where commands of the clients go (the UI thread)
    pub fn set_dispatcher(&self, dispatcher: Dispatcher) {
        *self.dispatcher.locked() = Some(Arc::new(dispatcher));
    }

    /// What to do once an uploaded backup is ready to restore
    pub fn set_restore_hook(&self, hook: RestoreHook) {
        *self.restore_hook.locked() = Some(Arc::new(hook));
    }

    fn restore_staged(&self) {
        let hook = self.restore_hook.locked().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn dispatch(&self, command: Command) {
        let dispatcher = self.dispatcher.locked().clone();
        if let Some(dispatch) = dispatcher {
            dispatch(command);
        }
    }

    pub fn status(&self) -> Status {
        self.status.locked().clone()
    }

    pub fn set_port(&self, port: u16) {
        self.status.locked().port = port;
    }

    /// Connected clients (signed in, page open)
    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    /// Clients with the tuner open: the tuner measures for them
    pub fn tuner_wanted(&self) -> bool {
        self.tuner_clients.load(Ordering::Relaxed) > 0
    }

    /// Clients showing the full editor: the UI is drawn for them
    pub fn stream_wanted(&self) -> bool {
        self.stream_clients.load(Ordering::Relaxed) > 0
    }

    /// What the pages asked of the next picture since the last one (and
    /// clear it)
    pub fn take_requests(&self) -> frames::Requests {
        std::mem::take(&mut *self.requests.locked())
    }

    /// A window of this size (a new size sends the whole window)
    fn request_size(&self, width: u32, height: u32) {
        self.requests.locked().size = Some((width, height));
    }

    /// The whole window in the next picture
    fn request_keyframe(&self) {
        self.requests.locked().keyframe = true;
    }

    /// These areas again in the next picture, as they are then
    fn request_repaint(&self, areas: &frames::Areas) {
        let mut requests = self.requests.locked();
        for &rect in areas.rects() {
            requests.repaint.add(rect);
        }
    }

    /// Publish the state, when it changed
    pub fn publish_state(&self, json: String) {
        self.state.send_if_modified(|current| {
            if **current == *json {
                false
            } else {
                *current = Arc::from(json);
                true
            }
        });
    }

    pub fn publish_telemetry(&self, json: String) {
        // No receiver: nobody to send it to
        let _ = self.telemetry.send(Arc::from(json));
    }

    /// The frame messages of one picture of the full editor (see
    /// protocol::frame_header): they go to a page together
    pub fn publish_picture(&self, frames: Vec<frames::Frame>) {
        if !frames.is_empty() {
            let _ = self.frames.send(Arc::from(frames));
        }
    }

    /// Sign every device out (and close their connections)
    pub fn revoke_all(&self) -> usize {
        let n = self.sessions.revoke_all();
        self.revoked.send_modify(|g| *g += 1);
        n
    }

    /// Set a new password: every device signs in again
    pub fn set_password(&self, password: &str) -> anyhow::Result<()> {
        self.passwords.set(password)?;
        self.revoke_all();
        Ok(())
    }

    /// Forget the password: the remote is off
    pub fn clear_password(&self) -> anyhow::Result<()> {
        self.passwords.clear()?;
        self.revoke_all();
        Ok(())
    }
}

/// The port to listen on: PEDALBOARD_REMOTE_PORT, else 80
pub fn port_from_env() -> u16 {
    std::env::var("PEDALBOARD_REMOTE_PORT")
        .ok()
        .and_then(|p| p.trim().parse().ok())
        .filter(|p| *p > 0)
        .unwrap_or(DEFAULT_PORT)
}
