//! Wi-Fi in Settings. `nmcli` calls take seconds (scans, joining a network),
//! so one worker thread runs them in order and publishes the result to the UI.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use slint::{ComponentHandle, Weak};
use tracing::{info, warn};

use pedalboard::wifi::{Network, Wifi, WifiError};

use crate::{model, App, WifiNetwork};

enum Command {
    Refresh {
        rescan: bool,
    },
    SetEnabled(bool),
    /// The password is never logged
    Connect {
        ssid: String,
        password: Option<String>,
    },
    Disconnect,
    Forget(String),
}

pub fn setup(app: &App) {
    let (tx, rx) = mpsc::channel();
    let weak = app.as_weak();
    if let Err(e) = thread::Builder::new()
        .name("wifi".into())
        .spawn(move || worker(rx, weak))
    {
        warn!("Wi-Fi: cannot start the worker: {e}");
        return;
    }

    let send = move |command| {
        let _ = tx.send(command);
    };
    let s = send.clone();
    app.on_wifi_refresh(move |rescan| s(Command::Refresh { rescan }));
    let s = send.clone();
    app.on_wifi_set_enabled(move |on| s(Command::SetEnabled(on)));
    let s = send.clone();
    app.on_wifi_connect(move |ssid, password| {
        s(Command::Connect {
            ssid: ssid.into(),
            password: (!password.is_empty()).then(|| password.into()),
        })
    });
    let s = send.clone();
    app.on_wifi_disconnect(move || s(Command::Disconnect));
    app.on_wifi_forget(move |ssid| send(Command::Forget(ssid.into())));
}

fn worker(rx: Receiver<Command>, app: Weak<App>) {
    let mut wifi: Option<Wifi> = None;
    for command in rx {
        // Detected on use: an adapter may be plugged in later
        if wifi.is_none() {
            match Wifi::detect() {
                Ok(w) => wifi = Some(w),
                Err(e) => {
                    let status = if e == WifiError::NoDevice {
                        "no-device"
                    } else {
                        "unsupported"
                    };
                    ui(&app, move |app| {
                        app.set_wifi_status(status.into());
                        app.set_wifi_networks(model(Vec::new()));
                        app.set_wifi_scanning(false);
                    });
                    continue;
                }
            }
        }
        let Some(wifi) = &wifi else { continue };

        let mut rescan = false;
        let result = match command {
            Command::Refresh { rescan: r } => {
                rescan = r;
                Ok(())
            }
            Command::SetEnabled(on) => {
                rescan = on;
                wifi.set_enabled(on)
            }
            Command::Connect { ssid, password } => {
                info!("Wi-Fi: joining {ssid}");
                let busy = ssid.clone();
                ui(&app, move |app| {
                    app.set_wifi_busy_ssid(busy.into());
                    app.set_wifi_message("".into());
                });
                let result = wifi.connect(&ssid, password.as_deref());
                ui(&app, |app| app.set_wifi_busy_ssid("".into()));
                result.map_err(|e| match e {
                    WifiError::WrongPassword => {
                        WifiError::Failed(format!("Wrong password for {ssid}."))
                    }
                    e => e,
                })
            }
            Command::Disconnect => wifi.disconnect(),
            Command::Forget(ssid) => wifi.forget(&ssid),
        };
        let message = match result {
            Ok(()) => String::new(),
            Err(e) => {
                warn!("Wi-Fi: {e}");
                e.to_string()
            }
        };
        refresh(wifi, &app, rescan, message);
    }
}

/// Publish the radio state and the networks; a rescan shows the cached list first
fn refresh(wifi: &Wifi, app: &Weak<App>, rescan: bool, message: String) {
    let enabled = match wifi.status() {
        Ok(status) => status.enabled,
        Err(e) => {
            let message = e.to_string();
            ui(app, move |app| app.set_wifi_message(message.into()));
            return;
        }
    };
    if !enabled {
        ui(app, move |app| {
            app.set_wifi_status("off".into());
            app.set_wifi_networks(model(Vec::new()));
            app.set_wifi_scanning(false);
            app.set_wifi_message(message.into());
        });
        return;
    }

    let cached = wifi.networks(false).unwrap_or_default();
    ui(app, move |app| {
        app.set_wifi_status("on".into());
        app.set_wifi_networks(model(cached.iter().map(view).collect()));
        app.set_wifi_scanning(rescan);
        app.set_wifi_message(message.into());
    });
    if rescan {
        let scanned = wifi.networks(true);
        ui(app, move |app| {
            app.set_wifi_scanning(false);
            match scanned {
                Ok(networks) => app.set_wifi_networks(model(networks.iter().map(view).collect())),
                Err(e) => app.set_wifi_message(e.to_string().into()),
            }
        });
    }
}

fn view(network: &Network) -> WifiNetwork {
    WifiNetwork {
        ssid: network.ssid.as_str().into(),
        signal: network.signal.into(),
        secured: network.secured,
        connected: network.connected,
        saved: network.saved,
    }
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
