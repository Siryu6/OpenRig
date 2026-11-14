//! Settings → Backup: write a backup to a USB drive (or the pedal when none
//! is mounted), list the backups found, restore one or undo the last
//! restore (both take effect at a restart, see `pedalboard::backup`), and
//! download again the store plugins and TONE3000 models a restore listed.
//! Files and network run on worker threads.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slint::ComponentHandle;
use tracing::{info, warn};

use pedalboard::backup::{self, Manifest, Options, Part};
use pedalboard::state::{data_dir, AppState};

use crate::{model, show_status, spawn, ui, App, Backup, BackupFile, BackupPart};

/// A backup file found, with its manifest or why it cannot be read
struct Found {
    path: PathBuf,
    usb: bool,
    manifest: Result<Manifest, String>,
}

thread_local! {
    static FOUND: RefCell<Vec<Found>> = const { RefCell::new(Vec::new()) };
    /// The drive the message is about ("Saved on <drive>…")
    static MESSAGE_DRIVE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Where backups are written and looked for: the drives, then the pedal
fn places(data: &Path) -> Vec<(PathBuf, bool)> {
    let mut places: Vec<(PathBuf, bool)> = pedalboard::rigs::removable_drives()
        .iter()
        .flat_map(|d| pedalboard::drives::folders(d))
        .map(|d| (d, true))
        .collect();
    places.push((data.join(backup::LOCAL_DIR), false));
    places
}

fn read(path: &Path) -> Result<Manifest, String> {
    std::fs::File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|f| {
            backup::read_manifest(std::io::BufReader::new(f)).map_err(|e| format!("{e:#}"))
        })
}

fn local_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn drive_name(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_row(found: &Found) -> BackupFile {
    let file_name = found
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let place = if found.usb {
        format!("USB {}", drive_name(&found.path))
    } else {
        "on the pedal".to_string()
    };
    match &found.manifest {
        Ok(m) => BackupFile {
            name: local_time(&m.created).into(),
            detail: format!(
                "OpenRig {} · {} · {}",
                m.app_version,
                backup::human_size(std::fs::metadata(&found.path).map_or(m.bytes, |x| x.len())),
                place
            )
            .into(),
            usb: found.usb,
            full: m.has(Part::Models) || m.has(Part::Plugins),
            readable: true,
        },
        Err(e) => BackupFile {
            name: file_name.into(),
            detail: e.clone().into(),
            usb: found.usb,
            full: false,
            readable: false,
        },
    }
}

/// What the panel shows, read off the UI thread
struct Scan {
    found: Vec<Found>,
    destination: String,
    /// The drive backups go to (and Eject ejects), "" for the pedal
    drive: String,
    usb: bool,
    large: String,
    local: String,
    undo: Option<Manifest>,
    missing: (usize, usize),
}

fn scan(data: &Path) -> Scan {
    let mut found = Vec::new();
    for (dir, usb) in places(data) {
        let mut paths = backup::list_dir(&dir);
        // Newest first
        paths.sort();
        paths.reverse();
        for path in paths {
            let manifest = read(&path);
            found.push(Found {
                path,
                usb,
                manifest,
            });
        }
    }
    let drives = pedalboard::rigs::removable_drives();
    let drive = drives
        .first()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (usb, destination) = match drives.first() {
        Some(d) => (true, format!("To {}", pedalboard::drives::name(d))),
        None => (
            false,
            format!(
                "No USB drive: kept on the pedal (the last {}). Download it from the remote control.",
                backup::LOCAL_KEPT
            ),
        ),
    };
    let (models, plugins) = backup::sizes(data);
    let large = format!(
        "Models {}, plugins {}",
        backup::human_size(models),
        backup::human_size(plugins)
    );
    let (manifest, _) = backup::plan(data, Options::default());
    let mut only_here = Vec::new();
    if !manifest.local_models.is_empty() {
        only_here.push(plural(manifest.local_models.len(), "model"));
    }
    if !manifest.local_plugins.is_empty() {
        only_here.push(plural(manifest.local_plugins.len(), "plugin"));
    }
    let local = if only_here.is_empty() {
        String::new()
    } else {
        format!(
            "{} of your own cannot be downloaded again. Turn this on to keep them.",
            only_here.join(" and ")
        )
    };
    let (p, m) = backup::missing(data);
    Scan {
        found,
        destination,
        drive,
        usb,
        large,
        local,
        undo: backup::undo_manifest(data),
        missing: (p.len(), m.len()),
    }
}

fn show(app: &App, scan: Scan) {
    let g = app.global::<Backup>();
    g.set_files(model(scan.found.iter().map(file_row).collect()));
    g.set_destination(scan.destination.into());
    g.set_usb(scan.usb);
    g.set_drive(scan.drive.into());
    g.set_large_size(scan.large.into());
    g.set_local_note(scan.local.into());
    g.set_can_undo(scan.undo.is_some());
    if let Some(undo) = &scan.undo {
        g.set_undo_detail(
            format!(
                "Data before the last restore ({})",
                local_time(&undo.created)
            )
            .into(),
        );
    }
    let (plugins, models) = scan.missing;
    g.set_missing((plugins + models) as i32);
    let mut parts = Vec::new();
    if plugins > 0 {
        parts.push(plural(plugins, "store plugin"));
    }
    if models > 0 {
        parts.push(plural(models, "TONE3000 model"));
    }
    g.set_missing_detail(format!("Restored presets need {}.", parts.join(" and ")).into());
    FOUND.with(|f| *f.borrow_mut() = scan.found);
}

fn refresh(app: &App) {
    let weak = app.as_weak();
    spawn("backup-scan", move || {
        let scan = scan(&data_dir());
        ui(&weak, move |app| show(app, scan));
    });
}

/// A USB drive was plugged in, ejected or pulled out: the open panel
/// looks again (a backup being written says itself how it went)
pub(crate) fn drives_changed(app: &App, now: &[PathBuf]) {
    // "Saved on <drive>" no longer holds once the drive is gone
    let stale = MESSAGE_DRIVE.with(|d| {
        d.borrow()
            .as_deref()
            .is_some_and(|d| pedalboard::drives::gone(d, now))
    });
    if stale {
        message(app, "", false);
    }
    let g = app.global::<Backup>();
    if g.get_open() && !g.get_busy() {
        refresh(app);
    }
}

fn message(app: &App, text: &str, error: bool) {
    MESSAGE_DRIVE.with(|d| *d.borrow_mut() = None);
    let g = app.global::<Backup>();
    g.set_message(text.into());
    g.set_message_error(error);
}

/// Files of `part` in the data directory now
fn count_now(data: &Path, part: Part) -> usize {
    backup::files(data, &[part]).len()
}

/// What restoring `manifest` replaces, part by part
fn parts(data: &Path, manifest: &Manifest) -> Vec<BackupPart> {
    Part::ALL
        .into_iter()
        .map(|part| {
            let now = count_now(data, part);
            let other_cpu = part == Part::Plugins
                && manifest.has(part)
                && manifest.arch != std::env::consts::ARCH;
            let (replaced, detail) = if other_cpu {
                (
                    false,
                    format!("Kept: the backup's are built for {}", manifest.arch),
                )
            } else if !manifest.has(part) {
                (false, "Kept as it is".to_string())
            } else if part == Part::Settings {
                (
                    true,
                    "Replaced: session, routing, hardware, MIDI, layouts".to_string(),
                )
            } else {
                (
                    true,
                    format!(
                        "Replaced: {} in the backup, {} now",
                        plural(manifest.count(part), "file"),
                        now
                    ),
                )
            };
            BackupPart {
                label: part.label().into(),
                detail: detail.into(),
                replaced,
            }
        })
        .collect()
}

const CONFIRM_NOTE: &str = "The pedal restarts to restore. What it replaces is kept for one undo. Passwords, accounts and Wi-Fi stay as they are.";

fn select(app: &App, index: i32) {
    let Ok(i) = usize::try_from(index) else {
        return;
    };
    let Some(manifest) = FOUND.with(|f| f.borrow().get(i).and_then(|f| f.manifest.clone().ok()))
    else {
        return;
    };
    let g = app.global::<Backup>();
    let mut note = CONFIRM_NOTE.to_string();
    if !manifest.has(Part::Models) && !manifest.has(Part::Plugins) {
        let needs = manifest.store_plugins.len() + manifest.tone3000_models.len();
        if needs > 0 {
            note.push_str(&format!(
                " Then download the {} it lists from here.",
                plural(needs, "plugin or model")
            ));
        }
    }
    g.set_confirm_title(format!("Restore the backup of {}?", local_time(&manifest.created)).into());
    g.set_confirm_parts(model(parts(&data_dir(), &manifest)));
    g.set_confirm_note(note.into());
    g.set_confirm_undo(false);
    g.set_selected(index);
}

fn ask_undo(app: &App) {
    let data = data_dir();
    let Some(undo) = backup::undo_manifest(&data) else {
        return;
    };
    let g = app.global::<Backup>();
    g.set_confirm_title("Undo the last restore?".into());
    g.set_confirm_parts(model(parts(&data, &undo)));
    g.set_confirm_note(
        "The pedal restarts with the data it had before the last restore. The restored data is kept for one undo in turn."
            .into(),
    );
    g.set_selected(-1);
    g.set_confirm_undo(true);
}

fn confirm(app: &App, state: &Arc<AppState>) {
    let g = app.global::<Backup>();
    if g.get_busy() {
        return;
    }
    let undo = g.get_confirm_undo();
    let path = usize::try_from(g.get_selected())
        .ok()
        .and_then(|i| FOUND.with(|f| f.borrow().get(i).map(|f| f.path.clone())));
    if !undo && path.is_none() {
        return;
    }
    g.set_busy(true);
    g.set_progress("Checking the backup…".into());
    let (weak, _st) = (app.as_weak(), state.clone());
    let started = spawn("backup-restore", move || {
        let data = data_dir();
        let result = match &path {
            _ if undo => backup::stage_undo(&data).map(|_| ()),
            Some(path) => std::fs::File::open(path)
                .map_err(anyhow::Error::from)
                .and_then(|f| backup::stage(&data, std::io::BufReader::new(f)).map(|_| ())),
            None => Ok(()),
        };
        ui(&weak, move |app| {
            let g = app.global::<Backup>();
            g.set_busy(false);
            g.set_selected(-1);
            g.set_confirm_undo(false);
            match result {
                Ok(()) => {
                    g.set_progress("Restarting to restore…".into());
                    crate::restart(app);
                }
                Err(e) => {
                    warn!(error = format_args!("{e:#}"), "Backup not restored");
                    g.set_progress("".into());
                    message(app, &format!("Not restored: {e:#}"), true);
                }
            }
        });
    });
    if !started {
        g.set_busy(false);
        g.set_progress("".into());
    }
}

fn back_up(app: &App, state: &Arc<AppState>) {
    let g = app.global::<Backup>();
    if g.get_busy() {
        return;
    }
    // The backup holds the chain as it is now
    if let Err(e) = state.save_session() {
        warn!(
            error = format_args!("{e:#}"),
            "Session not saved before the backup"
        );
    }
    let options = if g.get_include_large() {
        Options {
            models: true,
            plugins: true,
        }
    } else {
        Options::default()
    };
    let shown = g.get_drive();
    g.set_busy(true);
    g.set_progress("Backing up…".into());
    message(app, "", false);
    let weak = app.as_weak();
    let started = spawn("backup-write", move || {
        let data = data_dir();
        // The drive the panel shows; the pedal when there was none (or it is gone)
        let drive = pedalboard::drives::chosen(&pedalboard::rigs::removable_drives(), &shown);
        let dir = drive
            .as_deref()
            .map(pedalboard::drives::write_folder)
            .unwrap_or_else(|| data.join(backup::LOCAL_DIR));
        let result = backup::write_to_dir(&data, options, &dir);
        if result.is_ok() && drive.is_none() {
            backup::prune_local(&data);
        }
        let scan = scan(&data);
        ui(&weak, move |app| {
            let g = app.global::<Backup>();
            g.set_busy(false);
            g.set_progress("".into());
            match result {
                Ok((path, manifest)) => {
                    info!(path = %path.display(), files = manifest.files.len(), "Backup written");
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let place = match &drive {
                        Some(d) => format!("on {}", pedalboard::drives::name(d)),
                        None => "on the pedal".to_string(),
                    };
                    message(
                        app,
                        &format!("Saved {place}: {name} ({})", backup::human_size(size)),
                        false,
                    );
                    MESSAGE_DRIVE.with(|d| *d.borrow_mut() = drive);
                }
                Err(e) => {
                    warn!(error = format_args!("{e:#}"), "Backup not written");
                    message(app, &format!("Not saved: {e:#}"), true);
                }
            }
            show(app, scan);
        });
    });
    if !started {
        g.set_busy(false);
        g.set_progress("".into());
    }
}

/// Install the store plugins and download the TONE3000 models the last
/// restore listed, going on after a failure
fn get_missing(app: &App, state: &Arc<AppState>) {
    let g = app.global::<Backup>();
    if g.get_busy() {
        return;
    }
    g.set_busy(true);
    g.set_progress("Starting…".into());
    message(app, "", false);
    let (weak, st) = (app.as_weak(), state.clone());
    let started = spawn("backup-missing", move || {
        let data = data_dir();
        let (plugins, models) = backup::missing(&data);
        let total = plugins.len() + models.len();
        let progress = |done: usize, label: &str| {
            let text = format!("{} of {total}: {label}", done + 1);
            ui(&weak, move |app| {
                app.global::<Backup>().set_progress(text.into())
            });
        };
        let mut failed: Vec<String> = Vec::new();
        #[cfg_attr(not(feature = "store"), allow(unused_mut))]
        let mut installed = 0;
        let mut done = 0;
        #[cfg(feature = "store")]
        if !plugins.is_empty() {
            let store = pedalboard::store::Store::new(&data);
            let index = match store.cached().filter(|i| !i.is_stale()) {
                Some(index) => Ok(index),
                None => store.refresh(),
            };
            for plugin in &plugins {
                progress(done, &plugin.bundle);
                done += 1;
                let result = index
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e:#}"))
                    .and_then(|index| {
                        let id = index
                            .entries()
                            .find(|(p, e)| p.id == plugin.package && e.bundle == plugin.bundle)
                            .map(|(_, e)| e.id.clone())
                            .ok_or_else(|| anyhow::anyhow!("No longer in the plugin store"))?;
                        store.install(index, &id, |_, _| {})
                    });
                match result {
                    Ok(()) => installed += 1,
                    Err(e) => failed.push(format!("{}: {e:#}", plugin.bundle)),
                }
            }
        }
        #[cfg(not(feature = "store"))]
        for plugin in &plugins {
            failed.push(format!(
                "{}: this OpenRig has no plugin store",
                plugin.bundle
            ));
        }
        let mut downloaded = 0;
        for m in &models {
            progress(done, &m.name);
            done += 1;
            match download(&st, m) {
                Ok(()) => downloaded += 1,
                Err(e) => failed.push(format!("{}: {e:#}", m.name)),
            }
        }
        if installed > 0 {
            st.lv2_host.rescan();
        }
        let scan = scan(&data);
        ui(&weak, move |app| {
            let g = app.global::<Backup>();
            g.set_busy(false);
            g.set_progress("".into());
            if installed > 0 {
                crate::set_available_plugins(app, &st);
            }
            let mut text = format!(
                "{} installed, {} downloaded",
                plural(installed, "plugin"),
                plural(downloaded, "model")
            );
            if let Some(first) = failed.first() {
                text.push_str(&format!(". {} failed: {first}", failed.len()));
            }
            message(app, &text, !failed.is_empty());
            show(app, scan);
            if installed + downloaded > 0 {
                show_status(app, "Downloaded: reload the preset to use them".into());
            }
        });
    });
    if !started {
        g.set_busy(false);
        g.set_progress("".into());
    }
}

#[cfg(feature = "tone3000")]
fn download(state: &AppState, m: &backup::Tone3000File) -> anyhow::Result<()> {
    use pedalboard::tone3000::library;
    let models_dir = state.models_dir();
    let path = crate::tone3000_ui::with_account(|token| {
        library::download_for_rig(token, &models_dir, m.tone_id, Some(m.model_id), &m.name)
    })?;
    let got = path.to_string_lossy().replace('\\', "/");
    if got != m.path {
        warn!(wanted = %m.path, got = %got, "Model downloaded under another name");
    }
    Ok(())
}

#[cfg(not(feature = "tone3000"))]
fn download(_state: &AppState, _m: &backup::Tone3000File) -> anyhow::Result<()> {
    anyhow::bail!("This OpenRig has no TONE3000 support")
}

/// Said once the UI is up, after a restore applied at this start
pub fn restored(app: &App, manifest: &Manifest) {
    let (plugins, models) = backup::missing(&data_dir());
    let n = plugins.len() + models.len();
    let text = if manifest.undo {
        "Last restore undone".to_string()
    } else if n > 0 {
        format!(
            "Backup of {} restored. {} to download: Settings → Backup",
            local_time(&manifest.created),
            plural(n, "plugin or model")
        )
    } else {
        format!("Backup of {} restored", local_time(&manifest.created))
    };
    // Once the window is up: a toast shown before would be gone already
    let weak = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
        if let Some(app) = weak.upgrade() {
            crate::show_status_pinned(&app, text.into());
        }
    });
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let g = app.global::<Backup>();
    let weak = app.as_weak();
    g.on_opened(move || {
        let Some(app) = weak.upgrade() else { return };
        message(&app, "", false);
        refresh(&app);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    g.on_back_up(move || {
        if let Some(app) = weak.upgrade() {
            back_up(&app, &st);
        }
    });
    let weak = app.as_weak();
    g.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            select(&app, i);
        }
    });
    let weak = app.as_weak();
    g.on_ask_undo(move || {
        if let Some(app) = weak.upgrade() {
            ask_undo(&app);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    g.on_confirm(move || {
        if let Some(app) = weak.upgrade() {
            confirm(&app, &st);
        }
    });
    let weak = app.as_weak();
    g.on_cancel(move || {
        if let Some(app) = weak.upgrade() {
            let g = app.global::<Backup>();
            g.set_selected(-1);
            g.set_confirm_undo(false);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    g.on_get_missing(move || {
        if let Some(app) = weak.upgrade() {
            get_missing(&app, &st);
        }
    });
}
