//! The web server of the remote: the page, sign-in, and the WebSocket.
//!
//! Runs on its own thread (a single-threaded async runtime), so the UI
//! thread never waits for the network. Every request goes through
//! `guard`: local clients only, the Host header names the pedal (no DNS
//! rebinding), and requests that change something carry the page's own
//! Origin (no cross-site requests). The cookie is HttpOnly and
//! SameSite=Strict; the page has a strict Content Security Policy.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::{broadcast, Semaphore};

use super::frames::{self, Rect};
use super::protocol::{ClientMessage, Command};
use super::{net, web, Remote};

const COOKIE: &str = "openrig_session";
/// Knob moves are merged and sent to the UI thread at most this often
const KNOB_FLUSH: Duration = Duration::from_millis(33);
/// Editor frames on the way to a page (sent, not acknowledged yet) at most:
/// past it, frames are held back and their area is sent again, as it is
/// then, once the page caught up. Without it a slow link (weak Wi-Fi) or a
/// slow phone queues frames, and the page shows the pedal seconds late.
const FRAMES_IN_FLIGHT: usize = 32 * 1024;

/// The running server; stopped when dropped
pub struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Server {
    /// Listen on `port` (all addresses), or on the fallback port when
    /// `port` cannot be used (80 needs a capability)
    pub fn start(remote: Arc<Remote>, port: u16) -> anyhow::Result<Server> {
        let std_listener = bind(port).or_else(|e| {
            if port == super::FALLBACK_PORT {
                return Err(e);
            }
            tracing::warn!(
                "Remote control: port {port} unavailable ({e}), using {}",
                super::FALLBACK_PORT
            );
            bind(super::FALLBACK_PORT)
        })?;
        let port = std_listener.local_addr()?.port();
        remote.set_port(port);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let state = Shared {
            remote,
            hashing: Arc::new(Semaphore::new(1)),
        };
        let thread = std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!("Remote control: no runtime: {e}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::TcpListener::from_std(std_listener) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!("Remote control: {e}");
                            return;
                        }
                    };
                    // Small messages (a knob, a pong) leave at once, not
                    // after the acknowledgement of the previous ones
                    let listener = axum::serve::ListenerExt::tap_io(listener, |tcp| {
                        let _ = tcp.set_nodelay(true);
                    });
                    let app = router(state);
                    let served = axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await;
                    if let Err(e) = served {
                        tracing::error!("Remote control: server stopped: {e}");
                    }
                });
            })?;
        tracing::info!("Remote control: listening on port {port}");
        Ok(Server {
            stop: Some(stop),
            thread: Some(thread),
            port,
        })
    }
}

fn bind(port: u16) -> std::io::Result<std::net::TcpListener> {
    // IPv6 socket accepting IPv4 too when the system has IPv6, else IPv4
    let listener =
        std::net::TcpListener::bind(SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)))
            .or_else(|_| {
                std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
            })?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[derive(Clone)]
struct Shared {
    remote: Arc<Remote>,
    /// One password check at a time (Argon2 takes memory and CPU)
    hashing: Arc<Semaphore>,
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(web::index))
        .route("/app.js", get(web::script))
        .route("/app.css", get(web::style))
        .route("/icon.svg", get(web::icon))
        .route("/manifest.webmanifest", get(web::manifest))
        .route("/api/session", get(session))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/logout-all", post(logout_all))
        .route("/api/status", get(status))
        .route("/api/backup", get(download_backup))
        .route(
            "/api/restore",
            post(upload_backup).layer(DefaultBodyLimit::max(
                crate::backup::MAX_REMOTE_BYTES as usize,
            )),
        )
        .route("/ws", get(websocket))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// The address of the peer (IPv4 clients of an IPv6 socket unwrapped)
fn peer(addr: SocketAddr) -> IpAddr {
    match addr.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        ip => ip,
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    (status, text).into_response()
}

/// Checks every request, then adds the security headers to the answer
async fn guard(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !net::is_local_client(peer(addr)) {
        return plain(StatusCode::FORBIDDEN, "Forbidden");
    }
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !net::is_allowed_host(&host, &state.remote.names) {
        return plain(StatusCode::MISDIRECTED_REQUEST, "Unknown host");
    }
    let changes = request.method() != Method::GET && request.method() != Method::HEAD;
    let upgrade = headers.contains_key(header::UPGRADE);
    if changes || upgrade {
        let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
        if !net::is_same_origin(origin, &host) {
            return plain(StatusCode::FORBIDDEN, "Forbidden");
        }
    }
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    let csp = format!(
        "default-src 'self'; connect-src 'self' ws://{host} wss://{host}; img-src 'self' data: blob:; \
         style-src 'self'; script-src 'self'; object-src 'none'; frame-ancestors 'none'; \
         base-uri 'none'; form-action 'self'"
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "Cross-Origin-Opener-Policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "Cross-Origin-Resource-Policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

/// The session token of the cookie
fn token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

fn signed_in(state: &Shared, headers: &HeaderMap) -> bool {
    state.remote.passwords.is_set()
        && token(headers).is_some_and(|t| state.remote.sessions.check(&t))
}

async fn session(State(state): State<Shared>, headers: HeaderMap) -> Response {
    Json(serde_json::json!({ "signed_in": signed_in(&state, &headers) })).into_response()
}

#[derive(serde::Deserialize)]
struct Login {
    password: String,
}

async fn login(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> Response {
    let ip = peer(addr);
    let refused = |status: StatusCode, message: &str| {
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    };
    if let Some(wait) = state.remote.limiter.wait(ip) {
        let mut response = refused(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts: wait a moment",
        );
        if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return response;
    }
    let Ok(Login { password }) = serde_json::from_slice::<Login>(&body) else {
        return refused(StatusCode::BAD_REQUEST, "Bad request");
    };
    let Ok(_permit) = state.hashing.acquire().await else {
        return refused(StatusCode::SERVICE_UNAVAILABLE, "Try again");
    };
    let remote = state.remote.clone();
    let ok = tokio::task::spawn_blocking(move || {
        password.len() <= 256 && remote.passwords.verify(&password)
    })
    .await
    .unwrap_or(false);
    if !ok {
        state.remote.limiter.failed(ip);
        tracing::info!("Remote control: sign-in refused from {ip}");
        return refused(StatusCode::UNAUTHORIZED, "Wrong password");
    }
    state.remote.limiter.succeeded(ip);
    let Ok(token) = state.remote.sessions.create(ip) else {
        return refused(StatusCode::INTERNAL_SERVER_ERROR, "Try again");
    };
    tracing::info!("Remote control: device signed in from {ip}");
    let cookie = format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        super::auth::SESSION_IDLE.as_secs()
    );
    let mut response = Json(serde_json::json!({ "signed_in": true })).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, v);
    }
    response
}

fn clear_cookie(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("openrig_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    response
}

async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(t) = token(&headers) {
        state.remote.sessions.remove(&t);
        state.remote.revoked.send_modify(|g| *g += 1);
    }
    clear_cookie(Json(serde_json::json!({ "signed_in": false })).into_response())
}

async fn logout_all(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if !signed_in(&state, &headers) {
        return plain(StatusCode::UNAUTHORIZED, "Sign in");
    }
    let n = state.remote.revoke_all();
    tracing::info!("Remote control: {n} device(s) signed out");
    clear_cookie(Json(serde_json::json!({ "signed_in": false })).into_response())
}

async fn status(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if !signed_in(&state, &headers) {
        return plain(StatusCode::UNAUTHORIZED, "Sign in");
    }
    let addresses = tokio::task::spawn_blocking(crate::wifi::addresses)
        .await
        .unwrap_or_default();
    let hotspot = std::fs::read_to_string("/run/pedalboard/remote/hotspot")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let status = state.remote.status();
    Json(serde_json::json!({
        "version": status.version,
        "editor": status.editor,
        "port": status.port,
        "clients": state.remote.clients(),
        "hotspot": hotspot,
        "addresses": addresses
            .iter()
            .map(|a| serde_json::json!({ "kind": a.kind, "device": a.device, "address": a.ip }))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct BackupQuery {
    /// With models and plugins
    #[serde(default)]
    full: bool,
}

/// A backup of the pedal, as a file to save (held in memory, capped)
async fn download_backup(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<BackupQuery>,
) -> Response {
    if !signed_in(&state, &headers) {
        return plain(StatusCode::UNAUTHORIZED, "Sign in first");
    }
    let data_dir = state.remote.data_dir.clone();
    let options = crate::backup::Options {
        models: query.full,
        plugins: query.full,
    };
    let made =
        tokio::task::spawn_blocking(move || crate::backup::write_to_memory(&data_dir, options))
            .await;
    match made {
        Ok(Ok(bytes)) => {
            let name = crate::backup::file_name(chrono::Local::now());
            tracing::info!(bytes = bytes.len(), "Remote control: backup downloaded");
            let mut response = bytes.into_response();
            let h = response.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/gzip"),
            );
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            response
        }
        Ok(Err(e)) => {
            tracing::warn!(
                error = format_args!("{e:#}"),
                "Remote control: backup not made"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
        Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, "Backup not made"),
    }
}

/// A backup sent by the browser: checked and staged, then the app
/// restarts to restore it
async fn upload_backup(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !signed_in(&state, &headers) {
        return plain(StatusCode::UNAUTHORIZED, "Sign in first");
    }
    let remote = state.remote.clone();
    let staged =
        tokio::task::spawn_blocking(move || crate::backup::stage(&remote.data_dir, body.as_ref()))
            .await;
    match staged {
        Ok(Ok(manifest)) => {
            tracing::info!(created = %manifest.created, "Remote control: backup uploaded");
            state.remote.restore_staged();
            Json(serde_json::json!({ "restoring": true, "created": manifest.created }))
                .into_response()
        }
        Ok(Err(e)) => {
            tracing::warn!(
                error = format_args!("{e:#}"),
                "Remote control: backup refused"
            );
            (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response()
        }
        Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, "Backup not restored"),
    }
}

async fn websocket(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(token) = token(&headers).filter(|_| signed_in(&state, &headers)) else {
        return plain(StatusCode::UNAUTHORIZED, "Sign in");
    };
    let ip = peer(addr);
    upgrade
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let remote = state.remote.clone();
            remote.clients.fetch_add(1, Ordering::Relaxed);
            tracing::info!("Remote control: {ip} connected");
            let mut client = Client::default();
            client.run(&remote, socket, &token).await;
            client.set_tuner(&remote, false);
            client.set_stream(&remote, false);
            remote.clients.fetch_sub(1, Ordering::Relaxed);
            tracing::info!("Remote control: {ip} disconnected");
        })
}

/// One WebSocket connection
#[derive(Default)]
struct Client {
    tuner: bool,
    stream: bool,
    /// Knob moves not sent yet, the last value of each knob
    knobs: HashMap<(usize, usize), Command>,
    /// The page acknowledges frames: flow control (FRAMES_IN_FLIGHT)
    acks: bool,
    /// Sizes of the frames sent and not acknowledged yet, and their sum
    in_flight: VecDeque<usize>,
    in_flight_bytes: usize,
    /// Area of the frames held back
    missed: Option<Rect>,
}

impl Client {
    fn set_tuner(&mut self, remote: &Remote, on: bool) {
        if on != self.tuner {
            self.tuner = on;
            if on {
                remote.tuner_clients.fetch_add(1, Ordering::Relaxed);
            } else {
                remote.tuner_clients.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    fn set_stream(&mut self, remote: &Remote, on: bool) {
        if on && !remote.status().editor {
            return;
        }
        if on != self.stream {
            self.stream = on;
            if on {
                remote.stream_clients.fetch_add(1, Ordering::Relaxed);
            } else {
                remote.stream_clients.fetch_sub(1, Ordering::Relaxed);
            }
        }
        if on {
            remote.keyframe.store(true, Ordering::Relaxed);
        }
    }

    /// Whether to send this frame now: always without acknowledgements,
    /// else while little is on the way (one frame at least)
    fn admit(&mut self, frame: &[u8]) -> bool {
        if !self.acks {
            return true;
        }
        if self.in_flight_bytes > 0 && self.in_flight_bytes + frame.len() > FRAMES_IN_FLIGHT {
            if let Some(rect) = frames::rect_of(frame) {
                self.missed = Some(self.missed.map_or(rect, |m| m.union(rect)));
            }
            return false;
        }
        self.in_flight.push_back(frame.len());
        self.in_flight_bytes += frame.len();
        true
    }

    /// The page drew a frame: once all are, what it missed is sent again
    fn acked(&mut self, remote: &Remote) {
        if let Some(size) = self.in_flight.pop_front() {
            self.in_flight_bytes -= size;
        }
        if self.in_flight.is_empty() {
            if let Some(rect) = self.missed.take() {
                remote.request_repaint(rect);
            }
        }
    }

    fn flush(&mut self, remote: &Remote) {
        for (_, command) in self.knobs.drain() {
            remote.dispatch(command);
        }
    }

    fn handle(&mut self, remote: &Remote, text: &str) {
        let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
            return;
        };
        match message {
            ClientMessage::Tuner { on } => self.set_tuner(remote, on),
            ClientMessage::Ack => self.acked(remote),
            ClientMessage::Stream {
                on,
                width,
                height,
                acks,
            } => {
                if on {
                    self.acks = acks;
                }
                self.set_stream(remote, on);
                if on && width > 0 && height > 0 {
                    if let Some(Command::Resize { width, height }) =
                        (Command::Resize { width, height }).sanitized()
                    {
                        *remote.stream_size.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some((width, height));
                    }
                }
            }
            ClientMessage::Command(command) => {
                let Some(command) = command.sanitized() else {
                    return;
                };
                if matches!(
                    command,
                    Command::Pointer { .. } | Command::Key { .. } | Command::Resize { .. }
                ) && !self.stream
                {
                    return;
                }
                if let Command::Resize { width, height } = command {
                    *remote.stream_size.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((width, height));
                    remote.keyframe.store(true, Ordering::Relaxed);
                    return;
                }
                match command.knob() {
                    Some(knob) => {
                        self.knobs.insert(knob, command);
                    }
                    None => {
                        self.flush(remote);
                        remote.dispatch(command);
                    }
                }
            }
        }
    }

    async fn run(&mut self, remote: &Remote, mut socket: WebSocket, token: &str) {
        let mut state = remote.state.subscribe();
        let mut telemetry = remote.telemetry.subscribe();
        let mut frames = remote.frames.subscribe();
        let mut revoked = remote.revoked.subscribe();
        let mut tick = tokio::time::interval(KNOB_FLUSH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The state now, then each change
        let first = state.borrow_and_update().clone();
        if !first.is_empty() && socket.send(Message::Text((*first).into())).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => self.handle(remote, text.as_str()),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
                _ = tick.tick(), if !self.knobs.is_empty() => self.flush(remote),
                changed = state.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let json = state.borrow_and_update().clone();
                    if socket.send(Message::Text((*json).into())).await.is_err() {
                        break;
                    }
                }
                values = telemetry.recv() => match values {
                    Ok(json) => {
                        if socket.send(Message::Text((*json).into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                frame = frames.recv() => match frame {
                    Ok(frame) if self.stream && self.admit(&frame) => {
                        if socket.send(Message::Binary(frame.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    // Frames missed: the next one is the whole window
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        remote.keyframe.store(true, Ordering::Relaxed);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = revoked.changed() => {
                    if !remote.sessions.check(token) {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
        self.flush(remote);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; openrig_session=abc; b=2"),
        );
        assert_eq!(token(&headers).as_deref(), Some("abc"));
        assert_eq!(token(&HeaderMap::new()), None);
    }

    #[test]
    fn ipv4_peers_are_unwrapped() {
        let mapped: SocketAddr = "[::ffff:10.42.0.5]:5000"
            .parse()
            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        assert_eq!(peer(mapped), IpAddr::V4(Ipv4Addr::new(10, 42, 0, 5)));
    }

    fn remote() -> (
        Arc<Remote>,
        std::path::PathBuf,
        Arc<std::sync::Mutex<Vec<Command>>>,
    ) {
        let dir = std::env::temp_dir().join(format!(
            "openrig-server-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let remote = Remote::new(
            &dir,
            super::super::Status {
                version: "test".into(),
                editor: true,
                port: 0,
            },
        );
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        remote.set_dispatcher(Box::new(move |c| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).push(c)
        }));
        (remote, dir, seen)
    }

    #[test]
    fn commands_reach_the_dispatcher_and_knobs_merge() {
        let (remote, dir, seen) = remote();
        let mut client = Client::default();
        for v in [0.1, 0.2, 0.3] {
            client.handle(
                &remote,
                &format!(r#"{{"type":"set_parameter","block":1,"uri":"u","index":2,"value":{v}}}"#),
            );
        }
        assert!(
            seen.lock().map(|s| s.is_empty()).unwrap_or(false),
            "merged, not sent yet"
        );
        client.handle(&remote, r#"{"type":"tap"}"#);
        let commands = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert!(
            matches!(commands[0], Command::SetParameter { value, .. } if (value - 0.3).abs() < 1e-6)
        );
        assert_eq!(commands[1], Command::Tap);
        // Pointer events only while streaming
        client.handle(&remote, r#"{"type":"pointer","kind":"down","x":1,"y":1}"#);
        assert_eq!(seen.lock().map(|s| s.len()).unwrap_or(0), 2);
        client.handle(
            &remote,
            r#"{"type":"stream","on":true,"width":10,"height":10}"#,
        );
        assert!(remote.stream_wanted());
        assert_eq!(remote.take_stream_size(), Some((800, 480)));
        assert!(remote.take_keyframe());
        client.handle(&remote, r#"{"type":"pointer","kind":"down","x":1,"y":1}"#);
        assert_eq!(seen.lock().map(|s| s.len()).unwrap_or(0), 3);
        client.handle(&remote, r#"{"type":"tuner","on":true}"#);
        assert!(remote.tuner_wanted());
        client.set_tuner(&remote, false);
        client.set_stream(&remote, false);
        assert!(!remote.tuner_wanted());
        assert!(!remote.stream_wanted());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A frame message of `bytes` bytes showing `rect`
    fn frame(rect: Rect, bytes: usize) -> Vec<u8> {
        let fit = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        let mut message = super::super::protocol::frame_header(
            super::super::protocol::FrameFormat::Png,
            [fit(rect.x), fit(rect.y), fit(rect.width), fit(rect.height)],
            [800, 480],
        )
        .to_vec();
        message.resize(bytes, 0);
        message
    }

    #[test]
    fn frames_held_back_while_the_page_is_behind() {
        let (remote, dir, _) = remote();
        let mut client = Client::default();
        let big = FRAMES_IN_FLIGHT / 2 + 1;
        let a = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let b = Rect {
            x: 100,
            y: 50,
            width: 20,
            height: 5,
        };
        // A page without acknowledgements gets everything, as before
        for _ in 0..10 {
            assert!(client.admit(&frame(a, big)));
        }
        client.handle(
            &remote,
            r#"{"type":"stream","on":true,"width":800,"height":480,"acks":true}"#,
        );
        assert!(client.acks);
        // One frame always goes, even larger than the budget
        assert!(client.admit(&frame(a, FRAMES_IN_FLIGHT * 2)));
        client.handle(&remote, r#"{"type":"ack"}"#);
        assert!(client.admit(&frame(a, big)));
        // Over the budget: held back, its area remembered
        assert!(!client.admit(&frame(a, big)));
        assert!(!client.admit(&frame(b, big)));
        // A small one still fits
        assert!(client.admit(&frame(b, 100)));
        assert_eq!(remote.take_repaint(), None, "not before the page caught up");
        client.handle(&remote, r#"{"type":"ack"}"#);
        assert_eq!(remote.take_repaint(), None, "one still on the way");
        client.handle(&remote, r#"{"type":"ack"}"#);
        assert_eq!(remote.take_repaint(), Some(a.union(b)));
        assert_eq!(remote.take_repaint(), None);
        // Acknowledgements without frames change nothing
        client.handle(&remote, r#"{"type":"ack"}"#);
        assert_eq!(client.in_flight_bytes, 0);
        client.set_stream(&remote, false);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn revoking_signs_out() {
        let (remote, dir, _) = remote();
        assert!(remote.set_password("long enough").is_ok());
        let ip: IpAddr = [10, 42, 0, 2].into();
        let token = remote.sessions.create(ip).unwrap_or_default();
        assert!(remote.sessions.check(&token));
        assert!(remote.set_password("another one").is_ok());
        assert!(
            !remote.sessions.check(&token),
            "a new password signs everyone out"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The whole server over HTTP: the guard, sign-in and the cookie
    #[test]
    fn http_flow() {
        let (remote, dir, _) = remote();
        assert!(remote.set_password("long enough").is_ok());
        let server = Server::start(remote.clone(), 0);
        let Ok(server) = server else {
            panic!("server not started");
        };
        let port = server.port;
        let request = |text: String| -> String {
            use std::io::{Read, Write};
            let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
                return String::new();
            };
            let _ = stream.write_all(text.as_bytes());
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        };
        let get = |path: &str, host: &str, extra: &str| {
            request(format!(
                "GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"
            ))
        };
        let post = |path: &str, origin: &str, body: &str, extra: &str| {
            request(format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                body.len()
            ))
        };
        let host = format!("127.0.0.1:{port}");
        let origin = format!("http://{host}");

        let page = get("/", &host, "");
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("content-security-policy: default-src 'self'"));
        assert!(page.contains("x-frame-options: DENY"));
        // DNS rebinding: another name for the pedal's address
        assert!(get("/", "evil.example", "").starts_with("HTTP/1.1 421"));
        // Signed out
        assert!(get("/api/status", &host, "").starts_with("HTTP/1.1 401"));
        assert!(get("/ws", &host, "").starts_with("HTTP/1.1 4"));
        // Cross-site sign-in
        let cross = post(
            "/api/login",
            "http://evil.example",
            r#"{"password":"long enough"}"#,
            "",
        );
        assert!(cross.starts_with("HTTP/1.1 403"), "{cross}");
        // Wrong password: generic answer
        let wrong = post("/api/login", &origin, r#"{"password":"long enougH"}"#, "");
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        assert!(!wrong.to_ascii_lowercase().contains("set-cookie"));
        // Right one
        let ok = post("/api/login", &origin, r#"{"password":"long enough"}"#, "");
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        let cookie = ok
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
            .unwrap_or_default()
            .to_string();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        let value = cookie
            .split_once(':')
            .map(|(_, v)| v.trim().split(';').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        let with_cookie = format!("Cookie: {value}\r\n");
        let status = get("/api/status", &host, &with_cookie);
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        assert!(status.contains("\"version\":\"test\""));
        // Backups: signed in, same origin, checked before anything changes
        assert!(get("/api/backup", &host, "").starts_with("HTTP/1.1 401"));
        let _ = std::fs::create_dir_all(dir.join("presets"));
        let _ = std::fs::write(dir.join("presets").join("Lead.json"), "{}");
        let download = {
            use std::io::{Read, Write};
            let mut out = Vec::new();
            if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let _ = stream.write_all(
                    format!("GET /api/backup HTTP/1.1\r\nHost: {host}\r\n{with_cookie}Connection: close\r\n\r\n")
                        .as_bytes(),
                );
                let _ = stream.read_to_end(&mut out);
            }
            out
        };
        let head = String::from_utf8_lossy(&download[..download.len().min(400)]).to_string();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(
            head.contains("attachment; filename=\"openrig-backup-"),
            "{head}"
        );
        let restored = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = restored.clone();
        remote.set_restore_hook(Box::new(move || flag.store(true, Ordering::Relaxed)));
        let upload = |origin: &str, body: &[u8], extra: &str| -> String {
            use std::io::{Read, Write};
            let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
                return String::new();
            };
            let head = format!(
                "POST /api/restore HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\n\
                 Content-Type: application/gzip\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        };
        assert!(upload(&origin, b"x", "").starts_with("HTTP/1.1 401"));
        let cross = upload("http://evil.example", b"x", &with_cookie);
        assert!(cross.starts_with("HTTP/1.1 403"), "{cross}");
        let garbage = upload(&origin, b"not a backup", &with_cookie);
        assert!(garbage.starts_with("HTTP/1.1 400"), "{garbage}");
        assert!(!restored.load(Ordering::Relaxed));
        let Ok(archive) = crate::backup::write_to_memory(&dir, crate::backup::Options::default())
        else {
            panic!("backup not made");
        };
        let ok = upload(&origin, &archive, &with_cookie);
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(restored.load(Ordering::Relaxed));
        assert!(crate::backup::pending(&dir));
        // WebSocket without the page's Origin
        let ws = get(
            "/ws",
            &host,
            &format!("{with_cookie}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"),
        );
        assert!(ws.starts_with("HTTP/1.1 403"), "{ws}");
        // Too many failures: wait
        for _ in 0..4 {
            let _ = post(
                "/api/login",
                &origin,
                r#"{"password":"wrong password"}"#,
                "",
            );
        }
        let limited = post("/api/login", &origin, r#"{"password":"long enough"}"#, "");
        assert!(limited.starts_with("HTTP/1.1 429"), "{limited}");
        // Signing everyone out from the pedal revokes the cookie
        remote.revoke_all();
        assert!(get("/api/status", &host, &with_cookie).starts_with("HTTP/1.1 401"));
        drop(server);
        let _ = std::fs::remove_dir_all(dir);
    }
}
