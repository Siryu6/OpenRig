//! USB drives in the app: one list of the drives for every page, kept up
//! to date by `pedalboard::drives::watch` and by the pages that read the
//! drives themselves. The first drive is the one the pages show, write to
//! and eject (the `Drives` global); the rigs panel and Settings → Backup
//! list again when the drives change. Ejecting (through the root helper of
//! the OpenRig OS) runs on a worker thread.

use std::cell::RefCell;

use slint::ComponentHandle;
use tracing::{info, warn};

use pedalboard::drives::{self, Drive};

use crate::{backup_ui, rigs_ui, show_status, spawn, ui, App, Drives};

thread_local! {
    /// The drives as the pages last saw them
    static DRIVES: RefCell<Vec<Drive>> = const { RefCell::new(Vec::new()) };
}

/// The drives are `now` (read by the watch, an eject or a page): when they
/// changed, the `Drives` global shows the first one and the pages read
/// them again (and drop what they said of a drive gone)
pub(crate) fn update(app: &App, now: Vec<Drive>) {
    let changed = DRIVES.with(|d| {
        let mut drives = d.borrow_mut();
        let changed = *drives != now;
        if changed {
            drives.clone_from(&now);
        }
        changed
    });
    if !changed {
        return;
    }
    let g = app.global::<Drives>();
    let (path, name) = now
        .first()
        .map(|d| (d.path().to_string_lossy().into_owned(), d.name()))
        .unwrap_or_default();
    g.set_drive(path.into());
    g.set_name(name.into());
    rigs_ui::drives_changed(app, &now);
    backup_ui::drives_changed(app, &now);
}

/// Eject the drive the pages show (and write to)
fn eject(app: &App) {
    let g = app.global::<Drives>();
    if g.get_ejecting() {
        return;
    }
    let shown = g.get_drive();
    g.set_ejecting(true);
    show_status(app, "Ejecting the USB drive…".into());
    let weak = app.as_weak();
    let started = spawn("drives-eject", move || {
        let text = match drives::find(&shown) {
            None => "The USB drive is already gone".to_string(),
            Some(drive) => match drive.eject() {
                Ok(()) => {
                    info!(drive = %drive.path().display(), "USB drive ejected");
                    format!("You can unplug {}", drive.name())
                }
                Err(e) => {
                    warn!(drive = %drive.path().display(), error = format_args!("{e:#}"), "USB drive not ejected");
                    format!("{} not ejected: {e:#}", drive.name())
                }
            },
        };
        let now = drives::list();
        ui(&weak, move |app| {
            app.global::<Drives>().set_ejecting(false);
            show_status(app, text.into());
            update(app, now);
        });
    });
    if !started {
        g.set_ejecting(false);
    }
}

pub fn setup(app: &App) {
    let g = app.global::<Drives>();
    g.set_can_eject(drives::can_eject());
    let weak = app.as_weak();
    g.on_eject(move || {
        if let Some(app) = weak.upgrade() {
            eject(&app);
        }
    });
    let weak = app.as_weak();
    if let Err(e) = drives::watch(move |now| ui(&weak, move |app| update(app, now))) {
        warn!(error = %e, "USB drives not watched: refresh the pages by hand");
    }
}
