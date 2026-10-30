//! Raspberry Pi hardware controls (feature `gpio`): footswitches and a
//! rotary encoder with push button.
//!
//! Inputs are wired between the GPIO and ground (internal pull-ups). What
//! is wired where (BCM numbering) is set in Settings → Hardware
//! ([`pedalboard::hardware`]).
//!
//! The encoder sends key presses to the UI, so it drives whatever page is
//! shown (rack selection, parameter focus, browser...): Left/Right when
//! turned, Return when its button is pressed, Escape when it is held.
//! Footswitches report both edges: what they do is chosen on the Live page,
//! and holding the tap switch opens the tuner.
//!
//! External encoder boards on the I2C bus ([`pedalboard::i2c_encoders`])
//! are polled from a thread and drive the rack's knob strip
//! ([`crate::strip_ui`]).

use anyhow::{Context, Result};
use pedalboard::hardware::Hardware;
use rppal::gpio::{Event, InputPin, Trigger};
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::App;
use pedalboard::state::AppState;

const DEBOUNCE: Duration = Duration::from_millis(30);
/// Holding the encoder's button this long goes back (Escape) instead of
/// pressing (Return, on release)
const LONG_PRESS: Duration = Duration::from_millis(600);
/// Quadrature transitions: index = previous AB state << 2 | new AB state
const QUADRATURE: [i8; 16] = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];

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

/// Keeps the pins (and their interrupt threads) and the I2C polling alive:
/// dropping it frees them
pub struct Gpio {
    _pins: Vec<InputPin>,
    i2c: Option<I2cPolling>,
}

/// How often the I2C encoders are read
const I2C_POLL: Duration = Duration::from_millis(10);

/// The I2C bus of the 40-pin header (GPIO 2 and 3)
struct I2cBus(rppal::i2c::I2c);

impl pedalboard::i2c_encoders::Bus for I2cBus {
    fn write(&mut self, address: u8, data: &[u8]) -> std::io::Result<()> {
        self.0
            .set_slave_address(address.into())
            .map_err(std::io::Error::other)?;
        let written = self.0.write(data).map_err(std::io::Error::other)?;
        if written != data.len() {
            return Err(std::io::Error::other(format!(
                "0x{address:02X}: short write"
            )));
        }
        Ok(())
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> std::io::Result<()> {
        self.0
            .set_slave_address(address.into())
            .map_err(std::io::Error::other)?;
        let read = self.0.read(buffer).map_err(std::io::Error::other)?;
        if read != buffer.len() {
            return Err(std::io::Error::other(format!(
                "0x{address:02X}: short read"
            )));
        }
        Ok(())
    }
}

/// The thread reading the encoder boards; stopped when dropped
struct I2cPolling {
    found: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl I2cPolling {
    fn start(app: &slint::Weak<App>, settings: pedalboard::hardware::I2cEncoders) -> Result<Self> {
        use pedalboard::i2c_encoders::{Encoders, Event};
        let i2c =
            rppal::i2c::I2c::new().context("Cannot open the I2C bus (enable it in config.txt)")?;
        let mut bus = I2cBus(i2c);
        let mut encoders = Encoders::open(&mut bus, settings)?;
        let found = format!(
            "{} encoder{} found ({})",
            encoders.count(),
            if encoders.count() == 1 { "" } else { "s" },
            encoders.addresses()
        );
        info!("I2C: {found}");
        let stop = Arc::new(AtomicBool::new(false));
        let (app, halt) = (app.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("i2c-encoders".into())
            .spawn(move || {
                let mut failing = false;
                while !halt.load(Ordering::Relaxed) {
                    match encoders.poll(&mut bus, Instant::now()) {
                        Ok(events) if !events.is_empty() => {
                            let app = app.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                let Some(app) = app.upgrade() else { return };
                                for event in events {
                                    let (index, action, detents) = match event {
                                        Event::Turn { index, detents } => (index, 0, detents),
                                        Event::Press { index } => (index, 1, 0),
                                        Event::LongPress { index } => (index, 2, 0),
                                    };
                                    app.invoke_strip_encoder(index as i32, action, detents);
                                }
                            });
                            failing = false;
                        }
                        Ok(_) => failing = false,
                        // A loose cable: said once, then tried again slower
                        Err(e) => {
                            if !failing {
                                warn!("I2C encoders: {e}");
                            }
                            failing = true;
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                    std::thread::sleep(I2C_POLL);
                }
            })
            .context("I2C polling thread")?;
        Ok(Self {
            found,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for I2cPolling {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Gpio {
    /// Opens every input it can: one that fails (a GPIO taken by a driver)
    /// is left out and reported in `errors`, the others work
    pub fn start(
        app: slint::Weak<App>,
        state: Arc<AppState>,
        hardware: &Hardware,
    ) -> Result<(Self, Vec<anyhow::Error>)> {
        let gpio = rppal::gpio::Gpio::new().context("Cannot open the GPIO chip")?;
        let input = |pin: u8, user: &str| -> Result<InputPin> {
            Ok(gpio
                .get(pin)
                .with_context(|| format!("{user}: GPIO {pin} is not available"))?
                .into_input_pullup())
        };
        let mut held = Vec::new();
        let mut errors = Vec::new();

        for (slot, &pin) in hardware.footswitches.iter().enumerate() {
            let user = format!("FS {}", slot + 1);
            let mut input = match input(pin, &user) {
                Ok(input) => input,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let (app, state) = (app.clone(), state.clone());
            let interrupt =
                input.set_async_interrupt(Trigger::Both, Some(DEBOUNCE), move |event: Event| {
                    // Wired to ground: pressed pulls the pin low
                    let down = event.trigger == Trigger::FallingEdge;
                    let (app, state) = (app.clone(), state.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = app.upgrade() {
                            if down {
                                crate::live_ui::footswitch_down(&app, &state, slot);
                            } else {
                                crate::live_ui::footswitch_up(&app, slot);
                            }
                        }
                    });
                });
            if let Err(e) = interrupt {
                errors.push(anyhow::Error::from(e).context(format!("{user}: GPIO {pin}")));
                continue;
            }
            info!("Footswitch {} on GPIO {}", slot + 1, pin);
            held.push(input);
        }

        if let Some(encoder) = hardware.encoder {
            if let Err(e) = Self::encoder(&input, &app, encoder, &mut held) {
                errors.push(e);
            }
        }
        let i2c =
            hardware
                .i2c_encoders
                .and_then(|settings| match I2cPolling::start(&app, settings) {
                    Ok(polling) => Some(polling),
                    Err(e) => {
                        errors.push(e.context("External encoders"));
                        None
                    }
                });
        Ok((Self { _pins: held, i2c }, errors))
    }

    /// The external encoders found: "5 encoders found (0x36, 0x49)"
    pub fn i2c_found(&self) -> Option<&str> {
        self.i2c.as_ref().map(|i2c| i2c.found.as_str())
    }

    /// The encoder needs both its pins; its button is apart
    fn encoder(
        input: &impl Fn(u8, &str) -> Result<InputPin>,
        app: &slint::Weak<App>,
        encoder: pedalboard::hardware::Encoder,
        held: &mut Vec<InputPin>,
    ) -> Result<()> {
        {
            let steps = i32::from(encoder.steps.max(1));
            let mut pin_a = input(encoder.a, "Encoder A")?;
            let mut pin_b = input(encoder.b, "Encoder B")?;
            // Current AB levels (bit 1 = A, bit 0 = B) and accumulated steps
            let levels = Arc::new(AtomicU8::new(
                (u8::from(pin_a.is_high()) << 1) | u8::from(pin_b.is_high()),
            ));
            let count = Arc::new(AtomicI32::new(0));

            for (pin, bit) in [(&mut pin_a, 1u8), (&mut pin_b, 0u8)] {
                let (levels, count, app) = (levels.clone(), count.clone(), app.clone());
                pin.set_async_interrupt(Trigger::Both, None, move |event: Event| {
                    let high = event.trigger == Trigger::RisingEdge;
                    // Each pin has its own interrupt thread: update the
                    // levels and the count atomically
                    let old = if high {
                        levels.fetch_or(1 << bit, Ordering::Relaxed)
                    } else {
                        levels.fetch_and(!(1 << bit), Ordering::Relaxed)
                    };
                    let new = if high {
                        old | (1 << bit)
                    } else {
                        old & !(1 << bit)
                    };
                    let delta = QUADRATURE[((old << 2) | new) as usize] as i32;
                    if delta == 0 {
                        return;
                    }
                    // `try_update` needs a newer Rust than the image's 1.88
                    #[allow(deprecated)]
                    let stepped = count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
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
                })
                .context("Encoder")?;
            }

            info!("Encoder on GPIO {} / {}", encoder.a, encoder.b);
            held.extend([pin_a, pin_b]);
            // Without a push button, the touch screen confirms
            if let Some(button) = encoder.button {
                let mut pin_button = input(button, "Encoder button")?;
                let app = app.clone();
                // Presses so far; the held one, until a long press consumes it
                let presses = Arc::new(AtomicU32::new(0));
                let held_press = Arc::new(AtomicU32::new(0));
                pin_button
                    .set_async_interrupt(Trigger::Both, Some(DEBOUNCE), move |event: Event| {
                        if event.trigger == Trigger::FallingEdge {
                            let press = presses.fetch_add(1, Ordering::Relaxed) + 1;
                            held_press.store(press, Ordering::Relaxed);
                            let (app, held_press) = (app.clone(), held_press.clone());
                            let _ = slint::invoke_from_event_loop(move || {
                                slint::Timer::single_shot(LONG_PRESS, move || {
                                    if held_press
                                        .compare_exchange(
                                            press,
                                            0,
                                            Ordering::Relaxed,
                                            Ordering::Relaxed,
                                        )
                                        .is_ok()
                                    {
                                        press_key(&app, Key::Escape);
                                    }
                                });
                            });
                        } else if held_press.swap(0, Ordering::Relaxed) != 0 {
                            press_key(&app, Key::Return);
                        }
                    })
                    .context("Encoder button")?;
                info!("Encoder button on GPIO {}", button);
                held.push(pin_button);
            }
        }
        Ok(())
    }
}
