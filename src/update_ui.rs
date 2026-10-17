//! "Check for updates" in Settings. Checking, downloading and installing
//! take from seconds to minutes, so one worker thread runs them in order and
//! publishes the progress to the UI.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Weak};
use tracing::{info, warn};

use pedalboard::update::{self, Download, Installer, Kind, Plan, Status, System};

use crate::{App, Features};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Longest wait for the installer (a whole system on a slow card)
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// While these show, Check does nothing ("installed": the update waits for
/// a restart)
const BUSY: [&str; 4] = ["checking", "downloading", "installing", "installed"];

enum Command {
    Check,
    Install,
}

pub fn setup(app: &App) {
    app.global::<Features>().set_update(true);
    // Installable only where the system installer runs (pedalboard OS)
    let installer = Installer::from_env();
    let system = installer.as_ref().and_then(|_| System::detect());
    app.set_update_current(
        match &system {
            Some(system) => format!("OpenRig {VERSION} · system {}", system.version),
            None => format!("OpenRig {VERSION}"),
        }
        .into(),
    );

    let (tx, rx) = mpsc::channel();
    let weak = app.as_weak();
    if let Err(e) = thread::Builder::new()
        .name("update".into())
        .spawn(move || worker(rx, weak, system, installer))
    {
        warn!("Updates: cannot start the worker: {e}");
        return;
    }
    // The status changes here, before the worker runs the command: a second
    // press meanwhile is ignored, rather than queued after it
    let (t, weak) = (tx.clone(), app.as_weak());
    app.on_update_check(move || {
        let Some(app) = weak.upgrade() else { return };
        if BUSY.contains(&app.get_update_status().as_str()) {
            return;
        }
        app.set_update_status("checking".into());
        let _ = t.send(Command::Check);
    });
    let weak = app.as_weak();
    app.on_update_install(move || {
        let Some(app) = weak.upgrade() else { return };
        if app.get_update_status() != "ready" {
            return;
        }
        app.set_update_progress(0.0);
        app.set_update_status("downloading".into());
        let _ = tx.send(Command::Install);
    });
    let weak = app.as_weak();
    app.on_update_restart(move || {
        let Some(app) = weak.upgrade() else { return };
        // An app update runs at the next start of the app; a system update
        // at the next start of the pedalboard
        let action = if app.get_update_kind() == "app" {
            "restart"
        } else {
            "reboot"
        };
        app.invoke_power(action.into());
    });
}

fn worker(
    rx: Receiver<Command>,
    app: Weak<App>,
    system: Option<System>,
    installer: Option<Installer>,
) {
    let mut pending: Option<(String, Download)> = None;
    for command in rx {
        match command {
            Command::Check => {
                set_status(&app, "checking", "");
                pending = None;
                match update::check(VERSION, system.as_ref()) {
                    Ok(check) => {
                        let version = check.release.version().to_string();
                        info!("Updates: latest release {version}, {:?}", check.plan);
                        let (status, kind, size) = match &check.plan {
                            Plan::UpToDate => ("up-to-date", "", 0),
                            Plan::Available => ("available", "", 0),
                            Plan::Preparing => ("preparing", "", 0),
                            Plan::Install(download) => {
                                pending = Some((version.clone(), download.clone()));
                                ("ready", kind_name(download.kind), download.size)
                            }
                        };
                        ui(&app, move |app| {
                            app.set_update_version(version.into());
                            app.set_update_kind(kind.into());
                            app.set_update_size(megabytes(size).into());
                            app.set_update_status(status.into());
                        });
                    }
                    Err(e) => {
                        warn!("Updates: {e:#}");
                        set_status(&app, "error", &format!("{e:#}"));
                    }
                }
            }
            Command::Install => {
                let (Some((version, download)), Some(installer)) = (pending.take(), &installer)
                else {
                    set_status(&app, "error", "Check for updates again");
                    continue;
                };
                if let Err(e) = install(&app, installer, &download) {
                    warn!("Updates: installing {version}: {e:#}");
                    set_status(&app, "error", &format!("{e:#}"));
                    // Install again after a check
                }
            }
        }
    }
}

fn install(app: &Weak<App>, installer: &Installer, download: &Download) -> anyhow::Result<()> {
    info!("Updates: downloading {}", download.file);
    let dir = pedalboard::state::data_dir().join("update");
    let mut shown = 0.0f32;
    let path = update::download(download, &dir, |received, total| {
        let progress = received as f32 / total.max(1) as f32;
        if progress - shown >= 0.01 || received == total {
            shown = progress;
            ui(app, move |app| app.set_update_progress(progress));
        }
    })?;

    set_status(app, "installing", "");
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(&download.file);
    let id = installer.request(download.kind, file)?;
    let started = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(500));
        match installer.status(&id) {
            Some(Status::Done) => break,
            Some(Status::Failed(message)) => anyhow::bail!("{message}"),
            Some(Status::Installing) | None => {}
        }
        if started.elapsed() > INSTALL_TIMEOUT {
            anyhow::bail!("The installation does not finish: restart the pedalboard");
        }
    }
    info!("Updates: {} installed", download.file);
    set_status(app, "installed", "");
    Ok(())
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::App => "app",
        Kind::System => "system",
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.saturating_add(500_000) / 1_000_000)
}

fn set_status(app: &Weak<App>, status: &'static str, message: &str) {
    let message = message.to_string();
    ui(app, move |app| {
        app.set_update_message(message.into());
        app.set_update_status(status.into());
    });
}

/// Run on the UI thread
fn ui(app: &Weak<App>, f: impl FnOnce(&App) + Send + 'static) {
    let app = app.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app.upgrade() {
            f(&app);
        }
    });
}
