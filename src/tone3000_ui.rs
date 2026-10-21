//! TONE3000 in the UI: "TONE3000 Connect" in Settings (device-flow sign-in,
//! the `Features.tone3000` flag), the library page (lists, models,
//! downloads on request) and the pictures of tones and creators.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use slint::{
    ComponentHandle, Image, Model as _, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer, SharedString,
};
use tracing::warn;

use pedalboard::audio::ChainNode;
use pedalboard::plugins::scanner::{self, FileBlock, FileKind};
use pedalboard::state::AppState;
use pedalboard::tone3000::images;
use pedalboard::tone3000::library::{self, List, Model, Tone};
use pedalboard::tone3000::{self, AuthError, ConnectEvent, Connection, DeviceCode, Settings};

use crate::{model, App, Features, T3kModel, T3kTone};

/// View index of the settings page
const SETTINGS_VIEW: i32 = 7;

/// What the library page shows
#[derive(Default)]
struct Library {
    list: usize,
    gear: Option<String>,
    page: u32,
    tones: Vec<Tone>,
    /// The open tone and its models
    tone: Option<Tone>,
    models: Vec<Model>,
    downloading: HashSet<u64>,
    /// Block the library was opened from
    target: Option<&'static FileBlock>,
}

/// Side the tone pictures are fetched at (drawn at 48 px in the lists,
/// 88 px in the tone sheet and the block editor)
const PICTURE_SIDE: u32 = 96;
/// Side of the avatars (16 to 26 px)
const AVATAR_SIDE: u32 = 32;
/// Memory kept for pictures, about 400 tones
const PICTURE_CACHE_BYTES: usize = 16 << 20;
/// Pictures fetched at once
const PICTURE_WORKERS: usize = 3;

/// Pictures of tones and creators: fetched when shown, kept in memory only
struct Pictures {
    app: slint::Weak<App>,
    state: Mutex<PictureState>,
    wake: Condvar,
}

#[derive(Default)]
struct PictureState {
    cache: HashMap<String, SharedPixelBuffer<Rgba8Pixel>>,
    /// Cache keys, oldest first
    order: VecDeque<String>,
    bytes: usize,
    /// To fetch: (key, url, side)
    queue: VecDeque<(String, String, u32)>,
    busy: HashSet<String>,
    /// Not available (offline, no such file): not asked again until the
    /// list is reloaded
    failed: HashSet<String>,
    workers: usize,
}

static PICTURES: OnceLock<Arc<Pictures>> = OnceLock::new();

impl Pictures {
    fn key(url: &str, side: u32) -> String {
        format!("{side}:{url}")
    }

    /// The picture at `url` if it is loaded, else an empty image while it
    /// is fetched; with the key the UI keeps to fill it in later
    fn get(self: &Arc<Self>, url: Option<&str>, side: u32) -> (SharedString, Image) {
        let Some(url) = url.filter(|u| images::is_tone3000(u)) else {
            return (SharedString::new(), Image::default());
        };
        let key = Self::key(url, side);
        let mut state = self.state.lock().unwrap();
        if let Some(buffer) = state.cache.get(&key) {
            return (key.into(), Image::from_rgba8(buffer.clone()));
        }
        if !state.failed.contains(&key)
            && !state.busy.contains(&key)
            && !state.queue.iter().any(|(k, ..)| *k == key)
        {
            state.queue.push_back((key.clone(), url.to_string(), side));
            if state.workers < PICTURE_WORKERS {
                state.workers += 1;
                let pictures = self.clone();
                std::thread::spawn(move || pictures.work());
            }
            self.wake.notify_one();
        }
        (key.into(), Image::default())
    }

    /// Forget what is waiting (a new list replaces the old one) and try the
    /// failed pictures again
    fn reset_queue(&self) {
        let mut state = self.state.lock().unwrap();
        state.queue.clear();
        state.failed.clear();
    }

    fn work(self: Arc<Self>) {
        loop {
            let (key, url, side) = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if let Some(job) = state.queue.pop_front() {
                        state.busy.insert(job.0.clone());
                        break job;
                    }
                    state = self.wake.wait(state).unwrap();
                }
            };
            let picture = images::fetch(&url, side);
            let mut state = self.state.lock().unwrap();
            state.busy.remove(&key);
            let picture = match picture {
                Ok(picture) => picture,
                Err(e) => {
                    tracing::debug!("TONE3000 picture {}: {:#}", url, e);
                    state.failed.insert(key);
                    continue;
                }
            };
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &picture.rgba,
                picture.side,
                picture.side,
            );
            state.bytes += picture.rgba.len();
            state.cache.insert(key.clone(), buffer.clone());
            state.order.push_back(key.clone());
            while state.bytes > PICTURE_CACHE_BYTES {
                let Some(old) = state.order.pop_front() else {
                    break;
                };
                if let Some(old) = state.cache.remove(&old) {
                    state.bytes -= old.as_bytes().len();
                }
            }
            drop(state);
            let _ = self.app.upgrade_in_event_loop(move |app| {
                fill_picture(&app, &key, Image::from_rgba8(buffer));
            });
        }
    }
}

/// Put a picture just loaded where the UI waits for it
fn fill_picture(app: &App, key: &str, image: Image) {
    let fill = |row: &mut T3kTone| {
        let mut changed = false;
        if row.image_key == key {
            row.image = image.clone();
            changed = true;
        }
        if row.avatar_key == key {
            row.avatar = image.clone();
            changed = true;
        }
        changed
    };
    let tones = app.get_t3k_tones();
    for i in 0..tones.row_count() {
        if let Some(mut row) = tones.row_data(i) {
            if fill(&mut row) {
                tones.set_row_data(i, row);
            }
        }
    }
    let mut detail = app.get_t3k_detail();
    if fill(&mut detail) {
        app.set_t3k_detail(detail);
    }
    if app.get_t3k_avatar_key() == key {
        app.set_t3k_avatar(image);
    }
}

thread_local! {
    /// Pictures saved with downloaded tones, by path, with their date
    static LOCAL_PICTURES: std::cell::RefCell<HashMap<PathBuf, (std::time::SystemTime, Image)>> =
        Default::default();
}

/// The picture saved with a downloaded tone (no network: shown on stage);
/// an empty image when it cannot be read
pub fn local_picture(path: &std::path::Path) -> Image {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    LOCAL_PICTURES.with(|cache| {
        if let Some((date, image)) = cache.borrow().get(path) {
            if *date == modified {
                return image.clone();
            }
        }
        let image = match images::load(path) {
            Ok(picture) => Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                &picture.rgba,
                picture.side,
                picture.side,
            )),
            Err(e) => {
                warn!("Cannot read {}: {:#}", path.display(), e);
                Image::default()
            }
        };
        cache
            .borrow_mut()
            .insert(path.to_path_buf(), (modified, image.clone()));
        image
    })
}

struct Shared {
    settings: Mutex<Settings>,
    /// Held while getting a token: the refresh token rotates, two refreshes
    /// at once would make one fail and sign the user out
    token_lock: Mutex<()>,
    connection: Mutex<Option<Connection>>,
    /// Bumped on every connect / cancel, so late events of an old
    /// connection are ignored
    generation: AtomicU64,
    library: Mutex<Library>,
    /// Bumped when the list or the open tone changes, so late answers are dropped
    list_generation: AtomicU64,
    tone_generation: AtomicU64,
    models_dir: PathBuf,
    state: Arc<AppState>,
    pictures: Arc<Pictures>,
    /// Avatar of the signed-in account: `None` until asked
    account_avatar: Mutex<Option<Option<String>>>,
}

impl Shared {
    /// A valid access token (refreshed if needed). Network: call it off the UI thread.
    fn token(&self) -> Result<String, AuthError> {
        self.with_settings(Settings::access_token)
    }

    /// Run an API call with the account token; when TONE3000 refuses it,
    /// refresh the token and try once more before giving up
    fn call<T>(&self, mut f: impl FnMut(&str) -> Result<T, AuthError>) -> Result<T, AuthError> {
        let token = self.token()?;
        match f(&token) {
            Err(AuthError::Revoked) => {
                f(&self.with_settings(|settings| settings.access_token_after(&token))?)
            }
            result => result,
        }
    }

    /// Get a token from a copy of the settings (the network calls are made
    /// without holding them), then keep what changed
    fn with_settings(
        &self,
        get: impl FnOnce(&mut Settings) -> Result<String, AuthError>,
    ) -> Result<String, AuthError> {
        let _guard = self.token_lock.lock().unwrap();
        let mut settings = self.settings.lock().unwrap().clone();
        let token = get(&mut settings)?;
        let mut current = self.settings.lock().unwrap();
        // Keep the rotated tokens unless the user signed out meanwhile
        if current.connected() {
            *current = settings;
        }
        Ok(token)
    }
}

/// Show the account state and turn the TONE3000 features on or off
fn apply(app: &App, settings: &Settings) {
    app.set_t3k_client_id(settings.client_id.as_str().into());
    match &settings.session {
        Some(session) => {
            app.set_t3k_status("connected".into());
            app.set_t3k_username(session.username.as_str().into());
        }
        None => {
            app.set_t3k_status("signed-out".into());
            app.set_t3k_username(SharedString::new());
            app.set_t3k_avatar_key(SharedString::new());
            app.set_t3k_avatar(Image::default());
        }
    }
    app.global::<Features>().set_tone3000(settings.connected());
}

/// QR code of `text`, black on white with a quiet zone
fn qr_image(text: &str) -> Image {
    use qrcodegen::{QrCode, QrCodeEcc};
    let Ok(code) = QrCode::encode_text(text, QrCodeEcc::Medium) else {
        return Image::default();
    };
    const BORDER: i32 = 2;
    const SCALE: i32 = 4;
    let side = (code.size() + 2 * BORDER) * SCALE;
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(side as u32, side as u32);
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = (i as i32 % side) / SCALE - BORDER;
        let y = (i as i32 / side) / SCALE - BORDER;
        let value = if code.get_module(x, y) { 0 } else { 255 };
        *pixel = Rgb8Pixel::new(value, value, value);
    }
    Image::from_rgb8(buffer)
}

/// Checks of the stored session at start-up while the network is not up,
/// and the wait between them (about a minute in all)
const STARTUP_ATTEMPTS: u32 = 12;
const STARTUP_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

fn show_code(app: &App, code: &DeviceCode) {
    let url = code.verification_uri.trim_start_matches("https://");
    app.set_t3k_url(url.trim_end_matches('/').into());
    app.set_t3k_user_code(code.user_code.as_str().into());
    app.set_t3k_qr(qr_image(code.qr_url()));
    app.set_t3k_status("waiting".into());
}

/// Report a failed API call; a revoked link signs out
fn fail(weak: &slint::Weak<App>, shared: &Arc<Shared>, error: AuthError) {
    warn!("{}", error);
    let shared = shared.clone();
    let _ = weak.upgrade_in_event_loop(move |app| {
        if matches!(error, AuthError::Revoked) {
            let mut settings = shared.settings.lock().unwrap();
            if let Err(e) = settings.disconnect() {
                warn!("{:#}", e);
            }
            apply(&app, &settings);
            if app.get_current_view() > SETTINGS_VIEW {
                app.set_current_view(SETTINGS_VIEW);
            }
        }
        crate::show_status(&app, error.to_string().into());
    });
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let models_dir = state.models_dir();
    let settings = Settings::load();
    app.set_t3k_available(true);
    apply(app, &settings);
    let verify = settings.connected();
    let pictures = PICTURES
        .get_or_init(|| {
            Arc::new(Pictures {
                app: app.as_weak(),
                state: Mutex::new(PictureState::default()),
                wake: Condvar::new(),
            })
        })
        .clone();
    let shared = Arc::new(Shared {
        settings: Mutex::new(settings),
        token_lock: Mutex::new(()),
        connection: Mutex::new(None),
        generation: AtomicU64::new(0),
        library: Mutex::new(Library::default()),
        list_generation: AtomicU64::new(0),
        tone_generation: AtomicU64::new(0),
        models_dir,
        state: state.clone(),
        pictures,
        account_avatar: Mutex::new(None),
    });

    // Check the stored session in the background; offline is fine, only a
    // revoked link signs out. At boot the network comes up after the app:
    // retry quietly for a while instead of reporting it unreachable
    if verify {
        let weak = app.as_weak();
        let shared = shared.clone();
        std::thread::spawn(move || {
            for attempt in 1.. {
                match shared.token() {
                    Ok(_) => break,
                    Err(AuthError::Unavailable(e)) => {
                        if attempt >= STARTUP_ATTEMPTS {
                            warn!("TONE3000 session not checked, offline: {}", e);
                            break;
                        }
                        std::thread::sleep(STARTUP_RETRY);
                    }
                    Err(e) => {
                        fail(&weak, &shared, e);
                        break;
                    }
                }
            }
        });
    }

    setup_library(app, &shared);

    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_connect(move |key| {
        let Some(app) = weak.upgrade() else { return };
        // No key: the OpenRig application
        let key = match key.trim() {
            "" => String::new(),
            key => match tone3000::check_client_id(key) {
                Ok(key) if key == tone3000::OPENRIG_CLIENT_ID => String::new(),
                Ok(key) => key,
                Err(e) => {
                    app.set_t3k_error(e.to_string().into());
                    return;
                }
            },
        };
        {
            let mut settings = state.settings.lock().unwrap();
            settings.client_id = key.clone();
            if let Err(e) = settings.save() {
                app.set_t3k_error(format!("{e:#}").into());
                return;
            }
            apply(&app, &settings);
        }
        app.set_t3k_error(SharedString::new());
        app.set_t3k_status("starting".into());

        let generation = state.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let weak = app.as_weak();
        let events = state.clone();
        let connection = Connection::start(key, move |event| {
            let events = events.clone();
            let _ = weak.upgrade_in_event_loop(move |app| {
                if events.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                match event {
                    ConnectEvent::Code(code) => show_code(&app, &code),
                    ConnectEvent::Connected(settings) => {
                        events.connection.lock().unwrap().take();
                        events.account_avatar.lock().unwrap().take();
                        let user = settings.session.as_ref().map(|s| s.username.clone());
                        apply(&app, &settings);
                        *events.settings.lock().unwrap() = settings;
                        let message =
                            format!("Connected to TONE3000 as @{}", user.unwrap_or_default());
                        crate::show_status(&app, message.into());
                    }
                    ConnectEvent::Failed(message) => {
                        events.connection.lock().unwrap().take();
                        apply(&app, &events.settings.lock().unwrap());
                        app.set_t3k_error(message.into());
                    }
                    ConnectEvent::Cancelled => {}
                }
            });
        });
        *state.connection.lock().unwrap() = Some(connection);
    });

    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_cancel(move || {
        let Some(app) = weak.upgrade() else { return };
        state.generation.fetch_add(1, Ordering::Relaxed);
        state.connection.lock().unwrap().take();
        apply(&app, &state.settings.lock().unwrap());
    });

    let weak = app.as_weak();
    let state = shared;
    app.on_t3k_disconnect(move || {
        let Some(app) = weak.upgrade() else { return };
        let mut settings = state.settings.lock().unwrap();
        if let Err(e) = settings.disconnect() {
            app.set_t3k_error(format!("{e:#}").into());
        }
        state.account_avatar.lock().unwrap().take();
        apply(&app, &settings);
        crate::show_status(&app, "Disconnected from TONE3000".into());
    });
}

fn tone_detail(tone: &Tone) -> String {
    if tone.format == "ir" {
        let count = tone.irs_count.max(tone.models_count);
        return if count == 1 {
            "1 IR".into()
        } else {
            format!("{count} IRs")
        };
    }
    let count = if tone.models_count == 1 {
        "1 model".to_string()
    } else {
        format!("{} models", tone.models_count)
    };
    let a1 = tone.a1_models_count + tone.custom_models_count > 0;
    let a2 = tone.a2_models_count > 0;
    match (a1, a2) {
        (true, true) => format!("{count} · A1 + A2"),
        (false, true) => format!("{count} · A2"),
        _ => count,
    }
}

/// The block made for `tone`, if any
fn block_for(tone: &Tone) -> Option<&'static FileBlock> {
    scanner::FILE_BLOCKS.iter().find(|b| fits(b, tone))
}

/// Whether `tone` can be loaded in `block`
fn fits(block: &FileBlock, tone: &Tone) -> bool {
    let kind = match tone.format.as_str() {
        "nam" => FileKind::Neural,
        "ir" => FileKind::ImpulseResponse,
        _ => return false,
    };
    block.kind == kind && block.gears.contains(&tone.gear.as_str())
}

fn tone_row(shared: &Shared, library: &Library, tone: &Tone) -> T3kTone {
    let (image_key, image) = shared.pictures.get(tone.image(), PICTURE_SIDE);
    let (avatar_key, avatar) = shared.pictures.get(tone.avatar(), AVATAR_SIDE);
    T3kTone {
        title: tone.title.as_str().into(),
        creator: tone.creator().into(),
        gear: library::gear_label(&tone.gear).into(),
        format: tone.format.as_str().into(),
        license: tone.license.clone().unwrap_or_default().into(),
        detail: tone_detail(tone).into(),
        supported: tone.supported(),
        on_board: library::is_downloaded(&shared.models_dir, tone),
        usable: library.target.is_some_and(|b| fits(b, tone)),
        block_uri: block_for(tone).map(|b| b.uri).unwrap_or_default().into(),
        block_name: block_for(tone).map(|b| b.name).unwrap_or_default().into(),
        image,
        avatar,
        image_key,
        avatar_key,
    }
}

/// Show the signed-in account's avatar, asking TONE3000 for it once
fn show_account_avatar(app: &App, shared: &Arc<Shared>) {
    let known = shared.account_avatar.lock().unwrap().clone();
    match known {
        Some(url) => {
            let (key, image) = shared.pictures.get(url.as_deref(), AVATAR_SIDE);
            app.set_t3k_avatar_key(key);
            app.set_t3k_avatar(image);
        }
        None => {
            let weak = app.as_weak();
            let shared = shared.clone();
            std::thread::spawn(move || {
                // Not reachable: the "@" placeholder stays, asked again next time
                let Ok(url) = shared.call(tone3000::avatar) else {
                    return;
                };
                *shared.account_avatar.lock().unwrap() = Some(url);
                let _ = weak.upgrade_in_event_loop(move |app| show_account_avatar(&app, &shared));
            });
        }
    }
}

fn show_tones(app: &App, shared: &Shared, library: &Library) {
    let rows = library
        .tones
        .iter()
        .map(|t| tone_row(shared, library, t))
        .collect();
    app.set_t3k_tones(model(rows));
}

fn show_models(app: &App, shared: &Shared, library: &Library) {
    let Some(tone) = &library.tone else { return };
    let rows = library
        .models
        .iter()
        .map(|m| {
            let path = m.relative_path(tone);
            let local = path
                .as_ref()
                .is_some_and(|p| shared.models_dir.join(p).is_file());
            T3kModel {
                name: m.name.as_str().into(),
                detail: m.description(tone).into(),
                state: if local {
                    "local"
                } else if library.downloading.contains(&m.id) {
                    "downloading"
                } else {
                    "remote"
                }
                .into(),
                path: path
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect();
    app.set_t3k_models(model(rows));
}

/// Download `models` of `tone` one after the other
/// Download one model, on request only (the free API tier allows no bulk
/// download)
fn download(app: &App, shared: &Arc<Shared>, tone: Tone, model: Model) {
    {
        let mut library = shared.library.lock().unwrap();
        if !library.downloading.insert(model.id) {
            return;
        }
        show_models(app, shared, &library);
    }
    let weak = app.as_weak();
    let shared = shared.clone();
    std::thread::spawn(move || {
        // Network and file errors are reported apart from the account ones
        let mut failure = None;
        let result = shared.call(|token| {
            library::download(token, &shared.models_dir, &tone, &model).map_err(|e| {
                e.downcast::<AuthError>().unwrap_or_else(|e| {
                    failure = Some(e);
                    AuthError::Unavailable(String::new())
                })
            })
        });
        let message = match (result, failure) {
            (Ok(_), _) => format!("Downloaded {}", model.name),
            (Err(_), Some(e)) => {
                warn!("{:#}", e);
                format!("{e:#}")
            }
            (Err(e), None) => {
                finish_download(&weak, &shared, model.id, None);
                return fail(&weak, &shared, e);
            }
        };
        finish_download(&weak, &shared, model.id, Some(message));
    });
}

fn finish_download(
    weak: &slint::Weak<App>,
    shared: &Arc<Shared>,
    id: u64,
    message: Option<String>,
) {
    let shared = shared.clone();
    let _ = weak.upgrade_in_event_loop(move |app| {
        let mut library = shared.library.lock().unwrap();
        library.downloading.remove(&id);
        show_models(&app, &shared, &library);
        show_tones(&app, &shared, &library);
        drop(library);
        // A block may use another preset of this tone: its editor lists the
        // presets downloaded so far
        crate::refresh_chain(&app, &shared.state);
        if let Some(message) = message {
            crate::show_status(&app, message.into());
        }
    });
}

fn setup_library(app: &App, shared: &Arc<Shared>) {
    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_open(move |target_id| {
        let Some(app) = weak.upgrade() else { return };
        let uri =
            usize::try_from(target_id)
                .ok()
                .and_then(|id| match state.state.chain().find(id) {
                    Some(ChainNode::Plugin { uri, .. }) => Some(uri.clone()),
                    _ => None,
                });
        let block = uri.as_deref().and_then(scanner::file_block);
        let gears: Vec<&str> = match block {
            Some(block) => block.gears.to_vec(),
            None => std::iter::once("")
                .chain(library::GEARS.iter().map(|(id, ..)| *id))
                .collect(),
        };
        let names = gears
            .iter()
            .map(|g| match *g {
                "" => SharedString::from("All"),
                g => library::gear_label(g).into(),
            })
            .collect();
        app.set_t3k_gear_ids(model(
            gears.iter().map(|g| SharedString::from(*g)).collect(),
        ));
        app.set_t3k_gear_names(model(names));
        app.set_t3k_target_id(if block.is_some() { target_id } else { -1 });
        show_account_avatar(&app, &state);
        app.set_t3k_target_name(block.map(|b| b.name).unwrap_or_default().into());

        let (tab, gear) = {
            let mut library = state.library.lock().unwrap();
            library.target = block;
            let current = library.gear.clone().unwrap_or_default();
            let gear = if gears.contains(&current.as_str()) {
                current
            } else {
                gears[0].to_string()
            };
            (library.list as i32, gear)
        };
        app.invoke_t3k_load(tab, gear.into(), false);
    });

    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_load(move |list, gear, more| {
        let Some(app) = weak.upgrade() else { return };
        let list = usize::try_from(list).unwrap_or(0).min(List::ALL.len() - 1);
        let gear = (!gear.is_empty()).then(|| gear.to_string());
        let page = {
            let mut library = state.library.lock().unwrap();
            if !more {
                state.pictures.reset_queue();
                library.list = list;
                library.gear = gear.clone();
                library.page = 0;
                library.tones.clear();
                show_tones(&app, &state, &library);
            }
            library.page + 1
        };
        app.set_t3k_tab(list as i32);
        app.set_t3k_gear(gear.clone().unwrap_or_default().into());
        app.set_t3k_loading(true);
        app.set_t3k_list_error(SharedString::new());
        if !more {
            app.set_t3k_has_more(false);
        }
        let generation = state.list_generation.fetch_add(1, Ordering::Relaxed) + 1;

        let weak = app.as_weak();
        let shared = state.clone();
        std::thread::spawn(move || {
            let result =
                shared.call(|token| library::list(token, List::ALL[list], gear.as_deref(), page));
            let revoked = matches!(result, Err(AuthError::Revoked));
            if revoked {
                fail(&weak, &shared, AuthError::Revoked);
            }
            let state = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |app| {
                if state.list_generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                app.set_t3k_loading(false);
                match result {
                    Ok(page) => {
                        let mut library = state.library.lock().unwrap();
                        library.page = page.page;
                        library.tones.extend(page.tones);
                        app.set_t3k_has_more(page.has_more);
                        show_tones(&app, &state, &library);
                    }
                    Err(e) if !revoked => app.set_t3k_list_error(e.to_string().into()),
                    Err(_) => {}
                }
            });
        });
    });

    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_open_tone(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let tone = {
            let mut library = state.library.lock().unwrap();
            let Some(tone) = usize::try_from(index)
                .ok()
                .and_then(|i| library.tones.get(i))
            else {
                return;
            };
            let tone = tone.clone();
            library.tone = Some(tone.clone());
            library.models.clear();
            show_models(&app, &state, &library);
            tone
        };
        let row = tone_row(&state, &state.library.lock().unwrap(), &tone);
        app.set_t3k_detail(row.clone());
        app.set_t3k_detail_url(tone.page_url().trim_start_matches("https://").into());
        app.set_t3k_models_loading(tone.supported());
        app.set_t3k_detail_open(true);
        // Tones downloaded before their picture was kept get it now
        if row.on_board {
            let weak = app.as_weak();
            let shared = state.clone();
            let tone = tone.clone();
            std::thread::spawn(move || {
                if library::save_picture(&shared.models_dir, &tone) {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        crate::refresh_chain(&app, &shared.state);
                    });
                }
            });
        }
        if !tone.supported() {
            return;
        }
        let generation = state.tone_generation.fetch_add(1, Ordering::Relaxed) + 1;

        let weak = app.as_weak();
        let shared = state.clone();
        std::thread::spawn(move || {
            let result = shared.call(|token| library::models(token, &tone));
            let revoked = matches!(result, Err(AuthError::Revoked));
            if revoked {
                fail(&weak, &shared, AuthError::Revoked);
            }
            let state = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |app| {
                if state.tone_generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                app.set_t3k_models_loading(false);
                match result {
                    Ok(models) => {
                        let mut library = state.library.lock().unwrap();
                        library.models = models;
                        show_models(&app, &state, &library);
                    }
                    Err(e) if !revoked => crate::show_status(&app, e.to_string().into()),
                    Err(_) => {}
                }
            });
        });
    });

    let weak = app.as_weak();
    let state = shared.clone();
    app.on_t3k_download(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let job = {
            let library = state.library.lock().unwrap();
            let model = usize::try_from(index)
                .ok()
                .and_then(|i| library.models.get(i));
            library.tone.clone().zip(model.cloned())
        };
        if let Some((tone, model)) = job {
            download(&app, &state, tone, model);
        }
    });
}
