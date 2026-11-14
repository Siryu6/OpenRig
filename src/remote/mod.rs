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
pub mod frames;
pub mod net;
pub mod protocol;
mod server;
mod web;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};

use crate::sync::MutexExt;
pub use protocol::{ClientMessage, Command, Snapshot, Telemetry};
pub use server::Server;

/// Port of the web remote: 80, so the address alone opens it; another one
/// when 80 cannot be used (PEDALBOARD_REMOTE_PORT, or 8080 as a fallback)
pub const DEFAULT_PORT: u16 = 80;
pub const FALLBACK_PORT: u16 = 8080;

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
    frames: broadcast::Sender<Arc<[u8]>>,
    /// Bumped when sessions are revoked: connections check theirs again
    revoked: watch::Sender<u64>,
    clients: AtomicUsize,
    tuner_clients: AtomicUsize,
    stream_clients: AtomicUsize,
    /// A client needs a whole picture (it just connected, or missed frames)
    keyframe: AtomicBool,
    /// Areas a client did not get (it was behind): sent again, from the
    /// window as it is now
    repaint: Mutex<Option<frames::Rect>>,
    /// Window size the last streaming client asked for
    stream_size: Mutex<Option<(u32, u32)>>,
    dispatcher: Mutex<Option<Arc<Dispatcher>>>,
    status: Mutex<Status>,
    /// Names of the pedal accepted in the Host header
    names: Vec<String>,
}

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
            keyframe: AtomicBool::new(false),
            repaint: Mutex::new(None),
            stream_size: Mutex::new(None),
            dispatcher: Mutex::new(None),
            status: Mutex::new(status),
            names: net::local_names(&hostname),
        })
    }

    /// Where commands of the clients go (the UI thread)
    pub fn set_dispatcher(&self, dispatcher: Dispatcher) {
        *self.dispatcher.locked() = Some(Arc::new(dispatcher));
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

    /// Window size wanted by the last client that asked, once
    pub fn take_stream_size(&self) -> Option<(u32, u32)> {
        self.stream_size.locked().take()
    }

    /// Whether the next frame must be the whole window (and clear it)
    pub fn take_keyframe(&self) -> bool {
        self.keyframe.swap(false, Ordering::Relaxed)
    }

    /// Area to send again, if a client missed some (and clear it)
    pub fn take_repaint(&self) -> Option<frames::Rect> {
        self.repaint.locked().take()
    }

    fn request_repaint(&self, rect: frames::Rect) {
        let mut repaint = self.repaint.locked();
        *repaint = Some(match *repaint {
            Some(r) => r.union(rect),
            None => rect,
        });
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

    /// A frame message of the full editor (see protocol::frame_header)
    pub fn publish_frame(&self, frame: Vec<u8>) {
        let _ = self.frames.send(Arc::from(frame));
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
