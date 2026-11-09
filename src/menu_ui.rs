//! The ⋮ menu of the top bar. Its items depend on what the build and the
//! system offer, so they are listed here, one rule per item, rather than as
//! one Slint array per combination.

use crate::{model, App, MenuItem};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the menu depends on
#[derive(Clone, Copy, Debug, Default)]
struct Menu {
    /// A TONE3000 account is connected
    tone3000: bool,
    /// Built with the update feature
    update: bool,
    /// The OS switches the board off for the app (else the app just quits)
    power: bool,
    /// An update waits to be installed, or for a restart
    update_pending: bool,
}

/// Most used first, leaving the app last
fn items(menu: Menu) -> Vec<MenuItem> {
    let item = |id: &str, label: &str, icon: &str| MenuItem {
        id: id.into(),
        label: label.into(),
        icon: icon.into(),
        enabled: true,
        badge: false,
    };
    let mut items = Vec::with_capacity(5);
    if menu.tone3000 {
        items.push(item("tone3000", "TONE3000", "plug"));
    }
    items.push(item("routing", "Routing", "routing"));
    // Without updates (desktop builds), the same dialog shows the version
    // and the license
    items.push(if menu.update {
        MenuItem {
            badge: menu.update_pending,
            ..item("updates", "Updates", "refresh")
        }
    } else {
        item("about", "About OpenRig", "info")
    });
    items.push(item("settings", "Settings", "settings"));
    items.push(if menu.power {
        item("power", "Power off", "power")
    } else {
        item("quit", "Quit OpenRig", "power")
    });
    items
}

pub fn setup(app: &App) {
    // The About / Updates dialog shows it; the update feature adds the
    // system's version
    app.set_update_current(format!("OpenRig {VERSION}").into());
    app.on_main_menu(|tone3000, update, power, update_pending| {
        model(items(Menu {
            tone3000,
            update,
            power,
            update_pending,
        }))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(menu: Menu) -> Vec<String> {
        items(menu).iter().map(|i| i.id.to_string()).collect()
    }

    #[test]
    fn desktop_build_has_about_and_quit() {
        assert_eq!(
            ids(Menu::default()),
            ["routing", "about", "settings", "quit"]
        );
    }

    #[test]
    fn pedalboard_os_has_everything_in_order() {
        let menu = Menu {
            tone3000: true,
            update: true,
            power: true,
            update_pending: false,
        };
        assert_eq!(
            ids(menu),
            ["tone3000", "routing", "updates", "settings", "power"]
        );
    }

    #[test]
    fn only_updates_carry_the_badge() {
        let menu = Menu {
            update: true,
            update_pending: true,
            ..Menu::default()
        };
        let badged: Vec<String> = items(menu)
            .iter()
            .filter(|i| i.badge)
            .map(|i| i.id.to_string())
            .collect();
        assert_eq!(badged, ["updates"]);
        // No dot without the update feature
        let menu = Menu {
            update_pending: true,
            ..Menu::default()
        };
        assert!(items(menu).iter().all(|i| !i.badge));
    }
}
