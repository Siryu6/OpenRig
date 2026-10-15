//! Raspberry Pi hardware controls (feature `gpio`): footswitches and a
//! rotary encoder with push button.
//!
//! Inputs are wired between the GPIO and ground (internal pull-ups).
//! Pins (BCM numbering) can be changed with environment variables:
//! - `PEDALBOARD_FOOTSWITCH_PINS` (default `17,27,22,23`)
//! - `PEDALBOARD_ENCODER_PINS` = A,B,button (default `5,6,13`, empty to disable)
//! - `PEDALBOARD_ENCODER_STEPS` = quadrature steps per detent (default 4)
//!
//! The encoder sends Left/Right/Return key presses to the UI, so it drives
//! whatever page is shown (rack selection, parameter focus, browser...).
//! Footswitches report both edges: what they do is chosen on the Live page,
//! and holding the tap switch opens the tuner.

use anyhow::{Context, Result};
use rppal::gpio::{Event, InputPin, Trigger};
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::App;
use pedalboard::state::AppState;

const DEBOUNCE: Duration = Duration::from_millis(30);
/// Quadrature transitions: index = previous AB state << 2 | new AB state
const QUADRATURE: [i8; 16] = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];

fn pins(var: &str, default: &str) -> Result<Vec<u8>> {
    let value = std::env::var(var).unwrap_or_else(|_| default.to_string());
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u8>()
                .with_context(|| format!("{}: invalid pin '{}'", var, s))
        })
        .collect()
}

fn press_key(app: &slint::Weak<App>, key: Key) {
    let app = app.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app.upgrade() {
            let text: slint::SharedString = key.into();
            app.window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        }
    });
}

/// Keeps the pins (and their interrupt threads) alive
pub struct Gpio {
    _pins: Vec<InputPin>,
}

impl Gpio {
    pub fn start(app: slint::Weak<App>, state: Arc<AppState>) -> Result<Self> {
        let gpio = rppal::gpio::Gpio::new().context("Cannot open the GPIO chip")?;
        let mut held = Vec::new();

        let footswitches = pins("PEDALBOARD_FOOTSWITCH_PINS", "17,27,22,23")?;
        if let Some(app) = app.upgrade() {
            app.set_footswitch_count(footswitches.len() as i32);
        }
        for (slot, pin) in footswitches.into_iter().enumerate() {
            let mut input = gpio.get(pin)?.into_input_pullup();
            let (app, state) = (app.clone(), state.clone());
            input.set_async_interrupt(Trigger::Both, Some(DEBOUNCE), move |event: Event| {
                // Wired to ground: pressed pulls the pin low
                let down = event.trigger == Trigger::FallingEdge;
                let (app, state) = (app.clone(), state.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = app.upgrade() {
                        if down {
                            crate::live_ui::footswitch_down(&app, &state, slot);
                        } else {
                            crate::live_ui::footswitch_up(slot);
                        }
                    }
                });
            })?;
            info!("Footswitch {} on GPIO {}", slot + 1, pin);
            held.push(input);
        }

        let encoder = pins("PEDALBOARD_ENCODER_PINS", "5,6,13")?;
        match encoder.as_slice() {
            [] => {}
            [a, b, button] => {
                let steps: i32 = std::env::var("PEDALBOARD_ENCODER_STEPS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .filter(|&s| s > 0)
                    .unwrap_or(4);
                let mut pin_a = gpio.get(*a)?.into_input_pullup();
                let mut pin_b = gpio.get(*b)?.into_input_pullup();
                // Current AB levels (bit 1 = A, bit 0 = B) and accumulated steps
                let levels = Arc::new(AtomicU8::new(
                    (u8::from(pin_a.is_high()) << 1) | u8::from(pin_b.is_high()),
                ));
                let count = Arc::new(AtomicI32::new(0));

                for (pin, bit) in [(&mut pin_a, 1u8), (&mut pin_b, 0u8)] {
                    let (levels, count, app) = (levels.clone(), count.clone(), app.clone());
                    pin.set_async_interrupt(Trigger::Both, None, move |event: Event| {
                        let high = event.trigger == Trigger::RisingEdge;
                        let set = |old: u8| {
                            if high {
                                old | (1 << bit)
                            } else {
                                old & !(1 << bit)
                            }
                        };
                        // Each pin has its own interrupt thread: update the
                        // levels and the count atomically
                        let old = levels
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                                Some(set(old))
                            })
                            .unwrap_or_else(|old| old);
                        let delta = QUADRATURE[((old << 2) | set(old)) as usize] as i32;
                        if delta == 0 {
                            return;
                        }
                        let stepped =
                            count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                let total = n + delta;
                                Some(if total.abs() >= steps { 0 } else { total })
                            });
                        let total = stepped.unwrap_or_else(|n| n) + delta;
                        if total.abs() >= steps {
                            press_key(
                                &app,
                                if total > 0 {
                                    Key::RightArrow
                                } else {
                                    Key::LeftArrow
                                },
                            );
                        }
                    })?;
                }

                let mut pin_button = gpio.get(*button)?.into_input_pullup();
                let app = app.clone();
                pin_button.set_async_interrupt(
                    Trigger::FallingEdge,
                    Some(DEBOUNCE),
                    move |_| {
                        press_key(&app, Key::Return);
                    },
                )?;
                info!("Encoder on GPIO {} / {} (button {})", a, b, button);
                held.extend([pin_a, pin_b, pin_button]);
            }
            _ => warn!("PEDALBOARD_ENCODER_PINS needs 3 pins (A,B,button); encoder disabled"),
        }

        Ok(Self { _pins: held })
    }
}
