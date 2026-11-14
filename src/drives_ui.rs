//! USB drives in the app: the rigs panel and Settings → Backup list them
//! again when one is plugged in or pulled out, and their Eject button
//! ejects them (`pedalboard::drives`, through the root helper of the
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

/// Eject every drive listed (a pedal has one, usually)
fn eject(app: &App) {
    if app.global::<Rigs>().get_ejecting() {
        return;
    }
    set_ejecting(app, true);
    show_status(app, "Ejecting the USB drive…".into());
    let weak = app.as_weak();
    let started = spawn("drives-eject", move || {
        let found = pedalboard::rigs::removable_drives();
        let mut failed = None;
        for drive in &found {
            match drives::eject(drive) {
                Ok(()) => info!(drive = %drive.display(), "USB drive ejected"),
                Err(e) => {
                    warn!(drive = %drive.display(), error = format_args!("{e:#}"), "USB drive not ejected");
                    failed.get_or_insert(format!("{e:#}"));
                }
            }
        }
        ui(&weak, move |app| {
            set_ejecting(app, false);
            let text = match (failed, found.len()) {
                (Some(e), _) => format!("Not ejected: {e}"),
                (None, 0) => "No USB drive to eject".to_string(),
                (None, 1) => "You can unplug the USB drive".to_string(),
                (None, _) => "You can unplug the USB drives".to_string(),
            };
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
    app.global::<Rigs>().on_eject(move || {
        if let Some(app) = weak.upgrade() {
            eject(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<Backup>().on_eject(move || {
        if let Some(app) = weak.upgrade() {
            eject(&app);
        }
    });
    let weak = app.as_weak();
    if let Err(e) = drives::watch(move |_| ui(&weak, changed)) {
        warn!(error = %e, "USB drives not watched: refresh the pages by hand");
    }
}
