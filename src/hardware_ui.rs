//! Settings → Hardware: the footswitches and encoder wired to the board.
//! Changes apply at once: the inputs are opened again on their new GPIOs.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};
use tracing::warn;

use pedalboard::hardware::{Encoder, Hardware, ENCODER_STEPS, MAX_FOOTSWITCHES, MAX_PIN};
use pedalboard::state::AppState;

use crate::{App, Features};

struct Board {
    hardware: Hardware,
    #[cfg(feature = "gpio")]
    gpio: Option<crate::gpio::Gpio>,
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    app.global::<Features>().set_gpio(cfg!(feature = "gpio"));
    let board = Rc::new(RefCell::new(Board {
        hardware: Hardware::load(&pedalboard::state::data_dir()),
        #[cfg(feature = "gpio")]
        gpio: None,
    }));
    apply(app, state, &mut board.borrow_mut());

    let (weak, st, b) = (app.as_weak(), state.clone(), board.clone());
    app.on_hardware_step(move |what, index, delta| {
        let Some(app) = weak.upgrade() else { return };
        let mut board = b.borrow_mut();
        let mut hardware = board.hardware.clone();
        step(&mut hardware, &what, index, delta.signum());
        change(&app, &st, &mut board, hardware);
    });
    let (weak, st, b) = (app.as_weak(), state.clone(), board.clone());
    app.on_hardware_set_encoder(move |on| {
        let Some(app) = weak.upgrade() else { return };
        let mut board = b.borrow_mut();
        let mut hardware = board.hardware.clone();
        hardware.encoder = on.then(|| {
            // The reference wiring when free, else free pins
            let mut encoder = Encoder::default();
            let mut free = hardware.clone();
            for pin in [&mut encoder.a, &mut encoder.b] {
                *pin = free.free_pin(*pin).unwrap_or(*pin);
                free.footswitches.push(*pin);
            }
            encoder.button = free.free_pin(13);
            encoder
        });
        change(&app, &st, &mut board, hardware);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_hardware_set_encoder_button(move |on| {
        let Some(app) = weak.upgrade() else { return };
        let mut board = board.borrow_mut();
        let mut hardware = board.hardware.clone();
        let free = hardware.free_pin(13);
        if let Some(encoder) = &mut hardware.encoder {
            encoder.button = if on { free } else { None };
        }
        change(&app, &st, &mut board, hardware);
    });
}

/// One step of a setting: `what` is "footswitches" (how many),
/// "footswitch" (the GPIO of switch `index`), "encoder-a", "encoder-b",
/// "encoder-button" or "encoder-steps"
fn step(hardware: &mut Hardware, what: &str, index: i32, delta: i32) {
    let next_pin = |hardware: &Hardware, pin: u8| -> u8 {
        let used: Vec<u8> = hardware.pins().into_iter().map(|(p, _)| p).collect();
        let pins = MAX_PIN as i32 + 1;
        (1..pins)
            .map(|i| (pin as i32 + delta * i).rem_euclid(pins) as u8)
            .find(|p| !used.contains(p))
            .unwrap_or(pin)
    };
    match what {
        "footswitches" if delta > 0 && hardware.footswitches.len() < MAX_FOOTSWITCHES => {
            let after = hardware
                .footswitches
                .last()
                .map_or(17, |&p| p.saturating_add(1));
            if let Some(pin) = hardware.free_pin(after.min(MAX_PIN)) {
                hardware.footswitches.push(pin);
            }
        }
        "footswitches" if delta < 0 => {
            hardware.footswitches.pop();
        }
        "footswitch" => {
            if let Some(&pin) = usize::try_from(index)
                .ok()
                .and_then(|i| hardware.footswitches.get(i))
            {
                let next = next_pin(hardware, pin);
                hardware.footswitches[index as usize] = next;
            }
        }
        "encoder-a" | "encoder-b" | "encoder-button" => {
            let Some(encoder) = hardware.encoder else {
                return;
            };
            let pin = match what {
                "encoder-a" => encoder.a,
                "encoder-b" => encoder.b,
                _ => match encoder.button {
                    Some(pin) => pin,
                    None => return,
                },
            };
            let next = next_pin(hardware, pin);
            let encoder = hardware.encoder.as_mut().unwrap();
            match what {
                "encoder-a" => encoder.a = next,
                "encoder-b" => encoder.b = next,
                _ => encoder.button = Some(next),
            }
        }
        "encoder-steps" => {
            if let Some(encoder) = &mut hardware.encoder {
                let at = ENCODER_STEPS
                    .iter()
                    .position(|&s| s == encoder.steps)
                    .unwrap_or(0) as i32;
                let at = (at + delta).clamp(0, ENCODER_STEPS.len() as i32 - 1);
                encoder.steps = ENCODER_STEPS[at as usize];
            }
        }
        _ => {}
    }
}

fn change(app: &App, state: &Arc<AppState>, board: &mut Board, hardware: Hardware) {
    if hardware == board.hardware {
        return;
    }
    if let Err(e) = hardware.save(&pedalboard::state::data_dir()) {
        warn!("Hardware: {e:#}");
        app.set_hardware_message(format!("{e:#}").into());
        return;
    }
    board.hardware = hardware;
    apply(app, state, board);
}

/// Show the hardware and open its inputs
fn apply(app: &App, state: &Arc<AppState>, board: &mut Board) {
    let hardware = &board.hardware;
    let pins: Vec<i32> = hardware.footswitches.iter().map(|&p| p as i32).collect();
    app.set_hardware_footswitch_pins(ModelRc::new(VecModel::from(pins)));
    app.set_footswitch_count(hardware.footswitches.len() as i32);
    let encoder = hardware.encoder;
    app.set_hardware_encoder(encoder.is_some());
    let encoder = encoder.unwrap_or_default();
    app.set_hardware_encoder_a(encoder.a as i32);
    app.set_hardware_encoder_b(encoder.b as i32);
    app.set_hardware_encoder_button(encoder.button.map_or(-1, i32::from));
    app.set_hardware_encoder_steps(encoder.steps as i32);
    // A warning only: the bus may be unused on this board
    app.set_hardware_message(hardware.shared_pins().join("\n").into());

    #[cfg(feature = "gpio")]
    {
        // Frees the pins before opening them again
        board.gpio = None;
        match crate::gpio::Gpio::start(app.as_weak(), state.clone(), &board.hardware) {
            Ok((gpio, errors)) => {
                board.gpio = Some(gpio);
                let errors: Vec<String> = errors.iter().map(|e| format!("{e:#}")).collect();
                if !errors.is_empty() {
                    warn!("GPIO: {}", errors.join("; "));
                    app.set_hardware_message(format!("Not working: {}", errors.join("; ")).into());
                }
            }
            Err(e) => {
                warn!("GPIO disabled: {e:#}");
                app.set_hardware_message(format!("{e:#}").into());
            }
        }
    }
    // A switch held while it was moved or removed never reports its release
    crate::live_ui::release_footswitches(app);
    crate::live_ui::refresh(app, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_skip_used_pins() {
        let mut h = Hardware::default(); // FS 17 27 22 23, encoder 5 6 13
        step(&mut h, "footswitch", 2, 1);
        assert_eq!(h.footswitches[2], 24);
        step(&mut h, "footswitch", 2, -1);
        step(&mut h, "footswitch", 2, -1);
        assert_eq!(h.footswitches[2], 21);
        step(&mut h, "encoder-a", 0, -1);
        assert_eq!(h.encoder.unwrap().a, 4);
        // Wraps around the header
        step(&mut h, "footswitch", 1, 1);
        assert_eq!(h.footswitches[1], 0);
        assert!(h.validate().is_ok());
    }

    #[test]
    fn footswitch_count() {
        let mut h = Hardware::default();
        step(&mut h, "footswitches", 0, 1);
        assert_eq!(h.footswitches, vec![17, 27, 22, 23, 24]);
        for _ in 0..10 {
            step(&mut h, "footswitches", 0, 1);
        }
        assert_eq!(h.footswitches.len(), MAX_FOOTSWITCHES);
        assert!(h.validate().is_ok());
        for _ in 0..10 {
            step(&mut h, "footswitches", 0, -1);
        }
        assert!(h.footswitches.is_empty());
        step(&mut h, "footswitches", 0, 1);
        assert_eq!(h.footswitches, vec![17]);
    }

    #[test]
    fn encoder_steps() {
        let mut h = Hardware::default();
        step(&mut h, "encoder-steps", 0, -1);
        step(&mut h, "encoder-steps", 0, -1);
        step(&mut h, "encoder-steps", 0, -1);
        assert_eq!(h.encoder.unwrap().steps, 1);
        step(&mut h, "encoder-steps", 0, 1);
        assert_eq!(h.encoder.unwrap().steps, 2);
    }
}
