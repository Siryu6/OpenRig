//! Settings → Audio & system: the settings of pedalboard.txt (audio
//! interface, sample rate, buffer, screen rotation, Wi-Fi country), changed
//! from the screen on the pedalboard OS. The root helper writes the file;
//! audio changes restart JACK, and the app with it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tracing::{error, info};

use pedalboard::system_config::{
    self, Helper, SoundCard, Status, SystemConfig, COUNTRIES, PERIODS, PERIOD_COUNTS, ROTATIONS,
    SAMPLE_RATES,
};

use crate::{App, SystemSetting};

/// Longest wait for the helper (it writes a few hundred bytes)
const TIMEOUT: Duration = Duration::from_secs(10);

struct Settings {
    helper: Helper,
    /// What runs (read at boot or after the last change)
    running: SystemConfig,
    /// What the steppers show
    pending: SystemConfig,
    cards: Vec<SoundCard>,
    /// Request being written, polled by `timer`
    request: Option<(String, Instant)>,
    timer: slint::Timer,
}

pub fn setup(app: &App) {
    let Some(helper) = Helper::from_env() else {
        return;
    };
    let running = SystemConfig::load();
    let settings = Rc::new(RefCell::new(Settings {
        helper,
        pending: running.clone(),
        running,
        cards: system_config::sound_cards(),
        request: None,
        timer: slint::Timer::default(),
    }));
    app.set_system_available(true);
    show(app, &settings.borrow());

    let (weak, s) = (app.as_weak(), settings.clone());
    app.on_system_step(move |key, delta| {
        let Some(app) = weak.upgrade() else { return };
        let mut settings = s.borrow_mut();
        let cards = settings.cards.clone();
        step(&mut settings.pending, &cards, &key, delta.signum());
        app.set_system_message(SharedString::new());
        show(&app, &settings);
    });
    let (weak, s) = (app.as_weak(), settings.clone());
    app.on_system_revert(move || {
        let Some(app) = weak.upgrade() else { return };
        let mut settings = s.borrow_mut();
        settings.pending = settings.running.clone();
        app.set_system_message(SharedString::new());
        show(&app, &settings);
    });
    let (weak, s) = (app.as_weak(), settings);
    app.on_system_apply(move || {
        let Some(app) = weak.upgrade() else { return };
        let mut settings = s.borrow_mut();
        let pairs = settings.pending.changes(&settings.running);
        if pairs.is_empty() || settings.request.is_some() {
            return;
        }
        match settings.helper.request(&pairs) {
            Ok(id) => {
                info!(changes = %pairs.join(" "), "System settings change requested");
                settings.request = Some((id, Instant::now()));
                app.set_system_busy(true);
                app.set_system_message("Saving…".into());
                let (weak, s) = (app.as_weak(), s.clone());
                settings.timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(200),
                    move || {
                        if let Some(app) = weak.upgrade() {
                            poll(&app, &s);
                        }
                    },
                );
            }
            Err(e) => {
                error!(error = %e, "System settings not saved");
                app.set_system_message(format!("{e}").into());
            }
        }
    });
}

/// Follow the request until the helper is done with it
fn poll(app: &App, settings: &Rc<RefCell<Settings>>) {
    let mut s = settings.borrow_mut();
    let Some((id, since)) = s.request.clone() else {
        s.timer.stop();
        return;
    };
    let status = match s.helper.status(&id) {
        Some(status) => status,
        None if since.elapsed() > TIMEOUT => {
            Status::Failed("The settings helper did not answer".into())
        }
        None => return,
    };
    s.request = None;
    s.timer.stop();
    let message = match status {
        Status::Failed(message) => {
            error!(error = %message, "System settings not applied");
            app.set_system_busy(false);
            app.set_system_message(message.into());
            return;
        }
        // The app quits when JACK stops, and starts again on the new server
        Status::Audio => "Saved: the audio restarts with the new settings…".to_string(),
        Status::Display => {
            // Started again by the launcher, turned
            slint::Timer::single_shot(Duration::from_millis(600), || {
                // Fails only when the event loop is already gone
                let _ = slint::quit_event_loop();
            });
            "Saved: the screen turns…".to_string()
        }
        Status::Reboot => {
            app.set_system_busy(false);
            "Saved: the Wi-Fi country applies at the next restart".to_string()
        }
        Status::Saved => {
            app.set_system_busy(false);
            "Saved".to_string()
        }
    };
    s.running = SystemConfig::load();
    s.pending = s.running.clone();
    app.set_system_message(message.into());
    show(app, &s);
}

/// Interfaces offered: Auto, the cards there are, and the one set if it is
/// unplugged
fn devices(config: &SystemConfig, cards: &[SoundCard]) -> Vec<String> {
    let mut devices = vec!["auto".to_string()];
    devices.extend(cards.iter().map(|c| c.id.clone()));
    if !devices.contains(&config.audio_device) {
        devices.push(config.audio_device.clone());
    }
    devices
}

/// Move `key` of `config` by one choice
fn step(config: &mut SystemConfig, cards: &[SoundCard], key: &str, delta: i32) {
    fn next<T: PartialEq + Clone>(choices: &[T], current: &T, delta: i32) -> T {
        let at = choices.iter().position(|c| c == current).unwrap_or(0);
        let at = at.saturating_add_signed(delta as isize);
        choices
            .get(at.min(choices.len().saturating_sub(1)))
            .unwrap_or(current)
            .clone()
    }
    match key {
        "audio_device" => {
            let devices = devices(config, cards);
            config.audio_device = next(&devices, &config.audio_device, delta);
        }
        "sample_rate" => config.sample_rate = next(&SAMPLE_RATES, &config.sample_rate, delta),
        "period" => config.period = next(&PERIODS, &config.period, delta),
        "periods" => config.periods = next(&PERIOD_COUNTS, &config.periods, delta),
        "display_rotation" => {
            config.display_rotation = next(&ROTATIONS, &config.display_rotation, delta)
        }
        "wifi_country" => {
            let countries: Vec<String> = COUNTRIES.iter().map(|c| c.to_string()).collect();
            config.wifi_country = next(&countries, &config.wifi_country, delta);
        }
        _ => {}
    }
}

fn show(app: &App, s: &Settings) {
    let config = &s.pending;
    let row = |key: &str, label: &str, value: String, at: Option<usize>, len: usize| {
        let at = at.unwrap_or(0);
        SystemSetting {
            key: key.into(),
            label: label.into(),
            value: value.into(),
            can_decrement: at > 0,
            can_increment: at + 1 < len,
        }
    };
    let devices = devices(config, &s.cards);
    let device_name = match config.audio_device.as_str() {
        "auto" => "Auto".to_string(),
        id => s
            .cards
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| format!("{id} (not found)"), |c| c.name.clone()),
    };
    let rate = config.sample_rate as f32 / 1000.0;
    let rows = vec![
        row(
            "audio_device",
            "INTERFACE",
            device_name,
            devices.iter().position(|d| *d == config.audio_device),
            devices.len(),
        ),
        row(
            "sample_rate",
            "SAMPLE RATE",
            format!("{} kHz", (rate * 10.0).round() / 10.0),
            SAMPLE_RATES.iter().position(|r| *r == config.sample_rate),
            SAMPLE_RATES.len(),
        ),
        row(
            "period",
            "BUFFER",
            format!("{} frames", config.period),
            PERIODS.iter().position(|p| *p == config.period),
            PERIODS.len(),
        ),
        row(
            "periods",
            "PERIODS",
            config.periods.to_string(),
            PERIOD_COUNTS.iter().position(|p| *p == config.periods),
            PERIOD_COUNTS.len(),
        ),
        row(
            "display_rotation",
            "SCREEN ROTATION",
            format!("{}°", config.display_rotation),
            ROTATIONS.iter().position(|r| *r == config.display_rotation),
            ROTATIONS.len(),
        ),
        row(
            "wifi_country",
            "WI-FI COUNTRY",
            if config.wifi_country.is_empty() {
                "Default".to_string()
            } else {
                config.wifi_country.clone()
            },
            COUNTRIES.iter().position(|c| *c == config.wifi_country),
            COUNTRIES.len(),
        ),
    ];
    app.set_system_settings(ModelRc::new(VecModel::from(rows)));
    app.set_system_summary(
        format!(
            "Buffer latency {:.1} ms each way; applying restarts the audio for a moment",
            config.latency_ms()
        )
        .into(),
    );
    app.set_system_changed(s.pending != s.running);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stay_within_the_choices() {
        let cards = vec![SoundCard {
            id: "AudioBox".into(),
            name: "AudioBox USB 96".into(),
        }];
        let mut config = SystemConfig::default();
        step(&mut config, &cards, "audio_device", 1);
        assert_eq!(config.audio_device, "AudioBox");
        step(&mut config, &cards, "audio_device", 1);
        assert_eq!(config.audio_device, "AudioBox");
        step(&mut config, &cards, "period", -1);
        assert_eq!(config.period, 64);
        for _ in 0..10 {
            step(&mut config, &cards, "period", -1);
        }
        assert_eq!(config.period, PERIODS[0]);
        step(&mut config, &cards, "wifi_country", 1);
        assert_eq!(config.wifi_country, "AT");
        // A card set but unplugged stays offered
        config.audio_device = "pisound".into();
        assert_eq!(devices(&config, &cards), ["auto", "AudioBox", "pisound"]);
    }
}
