//! Settings → Hardware: the footswitches, encoder and external (I2C)
//! encoders wired to the board.
//! Changes apply at once: the inputs are opened again on their new GPIOs.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel, Weak};
use tracing::error;

use pedalboard::hardware::{
    Encoder, Hardware, I2cEncoders, I2cModule, ENCODER_STEPS, MAX_FOOTSWITCHES, MAX_PIN,
};
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
        edit(&weak, &st, &b, |hardware| {
            step(hardware, &what, index, delta.signum())
        });
    });
    let (weak, st, b) = (app.as_weak(), state.clone(), board.clone());
    app.on_hardware_set_encoder(move |on| {
        edit(&weak, &st, &b, |hardware| {
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
        });
    });
    let (weak, st, b) = (app.as_weak(), state.clone(), board.clone());
    app.on_hardware_set_i2c(move |on| {
        edit(&weak, &st, &b, |hardware| {
            hardware.i2c_encoders = on.then(|| I2cEncoders::new(I2cModule::Seesaw));
        });
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_hardware_set_encoder_button(move |on| {
        edit(&weak, &st, &board, |hardware| {
            let free = hardware.free_pin(13);
            if let Some(encoder) = &mut hardware.encoder {
                encoder.button = if on { free } else { None };
            }
        });
    });
}

/// One step of a setting: `what` is "footswitches" (how many),
/// "footswitch" (the GPIO of switch `index`), "encoder-a", "encoder-b",
/// "encoder-button", "encoder-steps", "i2c-module", "i2c-steps" or
/// "i2c-reverse" (on when `delta` is positive)
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
            let Some(i) = usize::try_from(index).ok() else {
                return;
            };
            if let Some(&pin) = hardware.footswitches.get(i) {
                let next = next_pin(hardware, pin);
                if let Some(slot) = hardware.footswitches.get_mut(i) {
                    *slot = next;
                }
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
            let Some(encoder) = hardware.encoder.as_mut() else {
                return;
            };
            match what {
                "encoder-a" => encoder.a = next,
                "encoder-b" => encoder.b = next,
                _ => encoder.button = Some(next),
            }
        }
        "encoder-steps" => {
            if let Some(encoder) = &mut hardware.encoder {
                encoder.steps = step_in(&ENCODER_STEPS, encoder.steps, delta);
            }
        }
        "i2c-module" | "i2c-steps" | "i2c-reverse" => {
            let Some(i2c) = &mut hardware.i2c_encoders else {
                return;
            };
            match what {
                "i2c-module" => i2c.module = step_in(&I2cModule::ALL, i2c.module, delta),
                "i2c-steps" => i2c.steps = step_in(&ENCODER_STEPS, i2c.steps, delta),
                _ => i2c.reverse = delta > 0,
            }
        }
        _ => {}
    }
}

/// The item `delta` places from `current` in `items` (from the first when
/// `current` is not there), kept within the list
fn step_in<T: Copy + PartialEq>(items: &[T], current: T, delta: i32) -> T {
    let at = items.iter().position(|&i| i == current).unwrap_or(0) as i32;
    items[(at + delta).clamp(0, items.len() as i32 - 1) as usize]
}

/// Change a copy of the hardware with `f`, then save and apply it
fn edit(
    app: &Weak<App>,
    state: &Arc<AppState>,
    board: &RefCell<Board>,
    f: impl FnOnce(&mut Hardware),
) {
    let Some(app) = app.upgrade() else { return };
    let Some(mut board) = crate::borrow_page(board, "Hardware settings") else {
        return;
    };
    let mut hardware = board.hardware.clone();
    f(&mut hardware);
    change(&app, state, &mut board, hardware);
}

fn change(app: &App, state: &Arc<AppState>, board: &mut Board, hardware: Hardware) {
    if hardware == board.hardware {
        return;
    }
    if let Err(e) = hardware.save(&pedalboard::state::data_dir()) {
        error!(error = format_args!("{e:#}"), "Hardware settings not saved");
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
    let i2c = hardware.i2c_encoders;
    app.set_hardware_i2c(i2c.is_some());
    let i2c = i2c.unwrap_or(I2cEncoders::new(I2cModule::Seesaw));
    app.set_hardware_i2c_module(i2c.module.name().into());
    app.set_hardware_i2c_steps(i2c.steps.into());
    app.set_hardware_i2c_reverse(i2c.reverse);
    app.set_hardware_i2c_found("".into());
    // A warning only: the bus may be unused on this board
    app.set_hardware_message(hardware.shared_pins().join("\n").into());

    #[cfg(feature = "gpio")]
    {
        // Frees the pins before opening them again
        board.gpio = None;
        match crate::gpio::Gpio::start(app.as_weak(), state.clone(), &board.hardware) {
            Ok((gpio, errors)) => {
                if let Some(found) = gpio.i2c_found() {
                    app.set_hardware_i2c_found(found.into());
                }
                board.gpio = Some(gpio);
                let errors: Vec<String> = errors.iter().map(|e| format!("{e:#}")).collect();
                if !errors.is_empty() {
                    tracing::warn!(errors = %errors.join("; "), "Some hardware inputs not opened");
                    app.set_hardware_message(format!("Not working: {}", errors.join("; ")).into());
                }
            }
            Err(e) => {
                error!(error = format_args!("{e:#}"), "GPIO disabled");
                app.set_hardware_message(format!("{e:#}").into());
            }
        }
    }
    // A switch held while it was moved or removed never reports its release
    crate::live_ui::release_footswitches(app, state);
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
        // A value off the list starts from the first
        assert_eq!(step_in(&ENCODER_STEPS, 3, 1), 2);
    }

    #[test]
    fn i2c_encoders() {
        let mut h = Hardware::default();
        // Off: nothing to step
        step(&mut h, "i2c-module", 0, 1);
        assert_eq!(h.i2c_encoders, None);
        h.i2c_encoders = Some(I2cEncoders::new(I2cModule::Seesaw));
        for _ in 0..5 {
            step(&mut h, "i2c-module", 0, 1);
            step(&mut h, "i2c-steps", 0, 1);
        }
        step(&mut h, "i2c-reverse", 0, 1);
        let i2c = h.i2c_encoders.unwrap();
        assert_eq!(
            (i2c.module, i2c.steps, i2c.reverse),
            (I2cModule::M5Stack8, 4, true)
        );
        step(&mut h, "i2c-steps", 0, -1);
        step(&mut h, "i2c-reverse", 0, -1);
        let i2c = h.i2c_encoders.unwrap();
        assert_eq!((i2c.steps, i2c.reverse), (2, false));
        assert!(h.validate().is_ok());
    }

    /// The rotary encoder never reaches its own settings on the Hardware
    /// page: changing its pins or switching it off would cut it off
    #[cfg(feature = "headless")]
    #[test]
    fn the_encoder_leaves_its_own_settings_alone() {
        use crate::View;
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Key, Platform, WindowAdapter, WindowEvent};

        struct Test(Rc<MinimalSoftwareWindow>);
        impl Platform for Test {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(slint::PhysicalSize::new(800, 480));
        slint::platform::set_platform(Box::new(Test(window.clone()))).unwrap();
        let app = App::new().unwrap();
        app.global::<Features>().set_gpio(true);
        app.set_footswitch_count(2);
        app.set_hardware_footswitch_pins(crate::model(vec![17, 27]));
        app.set_hardware_encoder(true);
        app.set_hardware_i2c(true);
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let c = calls.clone();
        app.on_hardware_step(move |what, _, _| c.borrow_mut().push(what.into()));
        let c = calls.clone();
        app.on_hardware_set_encoder(move |_| c.borrow_mut().push("set-encoder".into()));
        let c = calls.clone();
        app.on_hardware_set_encoder_button(move |_| {
            c.borrow_mut().push("set-encoder-button".into())
        });
        let c = calls.clone();
        app.on_tap_switch_changed(move |_| c.borrow_mut().push("tap".into()));
        let c = calls.clone();
        app.on_hardware_set_i2c(move |_| c.borrow_mut().push("i2c".into()));
        app.show().unwrap();
        // In use: no first turn or press spent showing the focus
        app.global::<crate::Encoder>().set_active(true);

        let key = |k: Key| {
            let text: slint::SharedString = k.into();
            window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(WindowEvent::KeyReleased { text });
        };
        // Runs the changed callbacks (the focus back to the start on a
        // new page)
        let settle = || {
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            window.draw_if_needed(|r| {
                let mut px = vec![slint::Rgb8Pixel::default(); 800 * 480];
                r.render(&mut px, 800);
            });
        };
        // Every stop, past the last: a press, a turn (adjusting it when it
        // is a stepper), a press again
        for stop in 0..12 {
            app.set_current_view(View::Rack);
            settle();
            app.set_current_view(View::Hardware);
            settle();
            for _ in 0..stop {
                key(Key::RightArrow);
            }
            key(Key::Return);
            key(Key::RightArrow);
            key(Key::Return);
        }
        let calls = calls.borrow();
        assert!(calls.iter().all(|c| !c.contains("encoder")), "{calls:?}");
        // The rest of the page is reached
        for what in [
            "footswitches",
            "footswitch",
            "tap",
            "i2c",
            "i2c-module",
            "i2c-steps",
            "i2c-reverse",
        ] {
            assert!(
                calls.iter().any(|c| c == what),
                "{what} not reached: {calls:?}"
            );
        }
    }
}
