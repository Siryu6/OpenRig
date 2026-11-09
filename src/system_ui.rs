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
    app.on_system_pick(move |key, index| {
        let Some(app) = weak.upgrade() else { return };
        let mut settings = s.borrow_mut();
        let cards = settings.cards.clone();
        pick(&mut settings.pending, &cards, &key, index);
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

/// Set `key` of `config` to the choice at `index` of its list or segments
fn pick(config: &mut SystemConfig, cards: &[SoundCard], key: &str, index: i32) {
    let Ok(index) = usize::try_from(index) else {
        return;
    };
    match key {
        "audio_device" => {
            if let Some(device) = devices(config, cards).get(index) {
                config.audio_device = device.clone();
            }
        }
        "display_rotation" => {
            if let Some(rotation) = ROTATIONS.get(index) {
                config.display_rotation = *rotation;
            }
        }
        "wifi_country" => {
            if let Some(country) = COUNTRIES.get(index) {
                config.wifi_country = country.to_string();
            }
        }
        _ => {}
    }
}

/// Name of an interface: Auto, the card's, or its id when unplugged
fn device_name(id: &str, cards: &[SoundCard]) -> String {
    match id {
        "auto" => "Auto".to_string(),
        id => cards
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| format!("{id} (not found)"), |c| c.name.clone()),
    }
}

/// A Wi-Fi country as shown: its code and name, "Default" for none
fn country_label(code: &str) -> String {
    let name = match code {
        "" => return "Default".to_string(),
        "AT" => "Austria",
        "AU" => "Australia",
        "BE" => "Belgium",
        "BR" => "Brazil",
        "CA" => "Canada",
        "CH" => "Switzerland",
        "CN" => "China",
        "CZ" => "Czechia",
        "DE" => "Germany",
        "DK" => "Denmark",
        "ES" => "Spain",
        "FI" => "Finland",
        "FR" => "France",
        "GB" => "United Kingdom",
        "GR" => "Greece",
        "HU" => "Hungary",
        "IE" => "Ireland",
        "IL" => "Israel",
        "IN" => "India",
        "IT" => "Italy",
        "JP" => "Japan",
        "KR" => "South Korea",
        "LU" => "Luxembourg",
        "MX" => "Mexico",
        "NL" => "Netherlands",
        "NO" => "Norway",
        "NZ" => "New Zealand",
        "PL" => "Poland",
        "PT" => "Portugal",
        "SE" => "Sweden",
        "US" => "United States",
        "ZA" => "South Africa",
        code => return code.to_string(),
    };
    format!("{code} · {name}")
}

fn show(app: &App, s: &Settings) {
    let config = &s.pending;
    let row =
        |key: &str, label: &str, value: String, at: Option<usize>, len: usize| SystemSetting {
            key: key.into(),
            label: label.into(),
            value: value.into(),
            can_decrement: at.unwrap_or(0) > 0,
            can_increment: at.unwrap_or(0) + 1 < len,
            kind: "stepper".into(),
            choices: ModelRc::default(),
            selected: at.map_or(-1, |at| at as i32),
        };
    // Unordered choices: a list to pick from, or segments when they are few
    let choose = |kind: &str, row: SystemSetting, choices: Vec<String>| SystemSetting {
        kind: kind.into(),
        choices: ModelRc::new(VecModel::from(
            choices
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )),
        ..row
    };
    let devices = devices(config, &s.cards);
    let rate = config.sample_rate as f32 / 1000.0;
    let rows = vec![
        choose(
            "list",
            row(
                "audio_device",
                "INTERFACE",
                device_name(&config.audio_device, &s.cards),
                devices.iter().position(|d| *d == config.audio_device),
                devices.len(),
            ),
            devices.iter().map(|d| device_name(d, &s.cards)).collect(),
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
        choose(
            "segmented",
            row(
                "display_rotation",
                "SCREEN ROTATION",
                format!("{}°", config.display_rotation),
                ROTATIONS.iter().position(|r| *r == config.display_rotation),
                ROTATIONS.len(),
            ),
            ROTATIONS.iter().map(|r| format!("{r}°")).collect(),
        ),
        choose(
            "list",
            row(
                "wifi_country",
                "WI-FI COUNTRY",
                country_label(&config.wifi_country),
                COUNTRIES.iter().position(|c| *c == config.wifi_country),
                COUNTRIES.len(),
            ),
            COUNTRIES.iter().map(|c| country_label(c)).collect(),
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

    #[test]
    fn picks_set_the_choice_at_the_index() {
        let cards = vec![SoundCard {
            id: "AudioBox".into(),
            name: "AudioBox USB 96".into(),
        }];
        let mut config = SystemConfig::default();
        pick(&mut config, &cards, "audio_device", 1);
        assert_eq!(config.audio_device, "AudioBox");
        pick(&mut config, &cards, "audio_device", 0);
        assert_eq!(config.audio_device, "auto");
        pick(&mut config, &cards, "display_rotation", 2);
        assert_eq!(config.display_rotation, 180);
        let fr = COUNTRIES.iter().position(|c| *c == "FR").unwrap();
        pick(&mut config, &cards, "wifi_country", fr as i32);
        assert_eq!(config.wifi_country, "FR");
        // Out of the list: nothing changes
        let before = config.clone();
        pick(&mut config, &cards, "audio_device", 5);
        pick(&mut config, &cards, "wifi_country", -1);
        pick(&mut config, &cards, "display_rotation", 4);
        pick(&mut config, &cards, "period", 0);
        assert_eq!(config, before);
    }

    #[test]
    fn labels_of_the_choices() {
        let cards = vec![SoundCard {
            id: "AudioBox".into(),
            name: "AudioBox USB 96".into(),
        }];
        assert_eq!(device_name("auto", &cards), "Auto");
        assert_eq!(device_name("AudioBox", &cards), "AudioBox USB 96");
        assert_eq!(device_name("pisound", &cards), "pisound (not found)");
        assert_eq!(country_label(""), "Default");
        assert_eq!(country_label("FR"), "FR · France");
        assert_eq!(country_label("XX"), "XX");
        // Every country offered has a name
        assert!(COUNTRIES[1..]
            .iter()
            .all(|c| country_label(c).contains(" · ")));
    }
}
