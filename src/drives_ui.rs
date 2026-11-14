//! USB drives in the app: the rigs panel and Settings → Backup list them
//! again when one is plugged in or pulled out, and their Eject button
//! ejects the one they show (`pedalboard::drives`, through the root helper of the
//! OpenRig OS). Ejecting runs on a worker thread.

use slint::ComponentHandle;
use tracing::{info, warn};

use pedalboard::drives;

use crate::{backup_ui, rigs_ui, show_status, spawn, ui, App, Backup, Rigs};

fn set_ejecting(app: &App, on: bool) {
    app.global::<Rigs>().set_ejecting(on);
    app.global::<Backup>().set_ejecting(on);
}

/// The drives changed: the pages that show them read them again
fn changed(app: &App) {
    rigs_ui::drives_changed(app);
    backup_ui::drives_changed(app);
}

/// Eject `shown`, the drive the page shows (and writes to)
fn eject(app: &App, shown: slint::SharedString) {
    if app.global::<Rigs>().get_ejecting() {
        return;
    }
    set_ejecting(app, true);
    show_status(app, "Ejecting the USB drive…".into());
    let weak = app.as_weak();
    let started = spawn("drives-eject", move || {
        let found = drives::chosen(&pedalboard::rigs::removable_drives(), &shown);
        let text = match found {
            None => "The USB drive is already gone".to_string(),
            Some(drive) => match drives::eject(&drive) {
                Ok(()) => {
                    info!(drive = %drive.display(), "USB drive ejected");
                    format!("You can unplug {}", drives::name(&drive))
                }
                Err(e) => {
                    warn!(drive = %drive.display(), error = format_args!("{e:#}"), "USB drive not ejected");
                    format!("{} not ejected: {e:#}", drives::name(&drive))
                }
            },
        };
        ui(&weak, move |app| {
            set_ejecting(app, false);
            show_status(app, text.into());
            changed(app);
        });
    });
    if !started {
        set_ejecting(app, false);
    }
}

pub fn setup(app: &App) {
    let can_eject = drives::can_eject();
    app.global::<Rigs>().set_can_eject(can_eject);
    app.global::<Backup>().set_can_eject(can_eject);
    let weak = app.as_weak();
    app.global::<Rigs>().on_eject(move |drive| {
        if let Some(app) = weak.upgrade() {
            eject(&app, drive);
        }
    });
    let weak = app.as_weak();
    app.global::<Backup>().on_eject(move |drive| {
        if let Some(app) = weak.upgrade() {
            eject(&app, drive);
        }
    });
    let weak = app.as_weak();
    if let Err(e) = drives::watch(move |_| ui(&weak, changed)) {
        warn!(error = %e, "USB drives not watched: refresh the pages by hand");
    }
}
