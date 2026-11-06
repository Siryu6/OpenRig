//! Raspberry Pi hardware controls (feature `gpio`): footswitches and a
//! rotary encoder with push button.
//!
//! Inputs are wired between the GPIO and ground (internal pull-ups). What
//! is wired where (BCM numbering) is set in Settings → Hardware
//! ([`pedalboard::hardware`]). The lines are taken through the kernel's GPIO
//! character device: a GPIO a driver holds (an audio HAT's I2S data lines)
//! is left alone, never switched to a plain input.
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
use gpiocdev::line::{Bias, EdgeDetection, EdgeKind, Value};
use gpiocdev::{Chip, Request};
use pedalboard::hardware::{Hardware, Line};
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use std::path::Path;
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
    crate::ui(app, move |app| {
        let text: slint::SharedString = key.into();
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    });
}

/// Keeps the inputs (a thread per GPIO line) and the I2C polling alive:
/// dropping it stops them and frees the lines
pub struct Gpio {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    i2c: Option<I2cPolling>,
}

/// How often an input thread checks whether it must stop
const STOP_POLL: Duration = Duration::from_millis(100);

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
        info!(found = %found, "External encoders opened");
        let stop = Arc::new(AtomicBool::new(false));
        let (app, halt) = (app.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("i2c-encoders".into())
            .spawn(move || {
                let mut failing = false;
                while !halt.load(Ordering::Relaxed) {
                    match encoders.poll(&mut bus, Instant::now()) {
                        Ok(events) if !events.is_empty() => {
                            crate::ui(&app, move |app| {
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
                                warn!(error = %e, "External encoders not answering");
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
            if thread.join().is_err() {
                warn!("External encoder thread had panicked");
            }
        }
    }
}

impl Gpio {
    /// Opens every input it can: one on a GPIO a driver holds (an audio
    /// HAT's I2S lines) is left out, one that fails is reported; both are
    /// in `errors`, the others work
    pub fn start(
        app: slint::Weak<App>,
        state: Arc<AppState>,
        hardware: &Hardware,
    ) -> Result<(Self, Vec<anyhow::Error>)> {
        let chip = header_chip()?;
        let mut gpio = Self {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            i2c: None,
        };
        // The kernel's view of each line: a pin set to I2S, SPI... reads
        // as used, and is never touched
        let usable = hardware.usable(|pin| {
            chip.line_info(pin.into()).ok().map(|info| {
                if info.used {
                    Line::Used(info.consumer)
                } else {
                    Line::Free
                }
            })
        });
        let mut errors: Vec<anyhow::Error> = usable
            .skipped
            .into_iter()
            .map(|skipped| {
                warn!(input = %skipped, "GPIO in use by a driver, input left out");
                anyhow::anyhow!(skipped)
            })
            .collect();
        let chip = chip.path().to_path_buf();

        for (slot, pin) in usable.footswitches {
            let user = format!("FS {}", slot + 1);
            let request = match request(&chip, pin, &user, Some(DEBOUNCE)) {
                Ok(request) => request,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let (app, state) = (app.clone(), state.clone());
            let watched = gpio.watch(request, &user, move |high| {
                // Wired to ground: pressed pulls the pin low
                let state = state.clone();
                crate::ui(&app, move |app| {
                    if high {
                        crate::live_ui::footswitch_up(app, slot);
                    } else {
                        crate::live_ui::footswitch_down(app, &state, slot);
                    }
                });
            });
            if let Err(e) = watched {
                errors.push(e.context(format!("{user}: GPIO {pin}")));
                continue;
            }
            info!(footswitch = slot + 1, gpio = pin, "Footswitch opened");
        }

        if let Some(encoder) = usable.encoder {
            if let Err(e) = gpio.encoder(&chip, &app, encoder) {
                errors.push(e);
            }
        }
        gpio.i2c =
            hardware
                .i2c_encoders
                .and_then(|settings| match I2cPolling::start(&app, settings) {
                    Ok(polling) => Some(polling),
                    Err(e) => {
                        errors.push(e.context("External encoders"));
                        None
                    }
                });
        Ok((gpio, errors))
    }

    /// Calls `on_edge` with the new level (high: true) on each edge of the
    /// line, from a thread of its own until `self` is dropped
    fn watch(
        &mut self,
        request: Request,
        user: &str,
        mut on_edge: impl FnMut(bool) + Send + 'static,
    ) -> Result<()> {
        let stop = self.stop.clone();
        let name = user.to_owned();
        let thread = std::thread::Builder::new()
            .name(format!("gpio {user}"))
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let edge = request
                        .wait_edge_event(STOP_POLL)
                        .and_then(|ready| ready.then(|| request.read_edge_event()).transpose());
                    match edge {
                        Ok(Some(edge)) => on_edge(edge.kind == EdgeKind::Rising),
                        Ok(None) => {}
                        Err(e) => {
                            warn!(input = %name, error = %e, "GPIO input stopped");
                            return;
                        }
                    }
                }
            })
            .context("GPIO thread")?;
        self.threads.push(thread);
        Ok(())
    }

    /// The external encoders found: "5 encoders found (0x36, 0x49)"
    pub fn i2c_found(&self) -> Option<&str> {
        self.i2c.as_ref().map(|i2c| i2c.found.as_str())
    }

    /// The encoder needs both its pins; its button is apart
    fn encoder(
        &mut self,
        chip: &Path,
        app: &slint::Weak<App>,
        encoder: pedalboard::hardware::Encoder,
    ) -> Result<()> {
        let steps = i32::from(encoder.steps.max(1));
        let pin_a = request(chip, encoder.a, "Encoder A", None)?;
        let pin_b = request(chip, encoder.b, "Encoder B", None)?;
        let high = |request: &Request| -> Result<u8> {
            Ok(u8::from(request.lone_value()? == Value::Active))
        };
        // Current AB levels (bit 1 = A, bit 0 = B) and accumulated steps
        let levels = Arc::new(AtomicU8::new(
            (high(&pin_a).context("Encoder A")? << 1) | high(&pin_b).context("Encoder B")?,
        ));
        let count = Arc::new(AtomicI32::new(0));

        for (pin, bit, user) in [(pin_a, 1u8, "Encoder A"), (pin_b, 0u8, "Encoder B")] {
            let (levels, count, app) = (levels.clone(), count.clone(), app.clone());
            self.watch(pin, user, move |high| {
                // Each pin has its own thread: update the levels and the
                // count atomically
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
                let stepped = count.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
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
        info!(gpio_a = encoder.a, gpio_b = encoder.b, "Encoder opened");

        // Without a push button, the touch screen confirms
        if let Some(button) = encoder.button {
            let pin_button = request(chip, button, "Encoder button", Some(DEBOUNCE))?;
            let app = app.clone();
            // Presses so far; the held one, until a long press consumes it
            let presses = Arc::new(AtomicU32::new(0));
            let held_press = Arc::new(AtomicU32::new(0));
            self.watch(pin_button, "Encoder button", move |high| {
                if !high {
                    let press = presses.fetch_add(1, Ordering::Relaxed) + 1;
                    held_press.store(press, Ordering::Relaxed);
                    let (app, held_press) = (app.clone(), held_press.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        slint::Timer::single_shot(LONG_PRESS, move || {
                            if held_press
                                .compare_exchange(press, 0, Ordering::Relaxed, Ordering::Relaxed)
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
            info!(gpio = button, "Encoder button opened");
        }
        Ok(())
    }
}

impl Drop for Gpio {
    /// Stops the threads, which frees the lines
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                warn!("GPIO thread had panicked");
            }
        }
    }
}

/// Labels of the chip of the 40-pin header: Pi 5 (RP1), Pi 4, older Pis
const HEADER_CHIPS: [&str; 3] = ["pinctrl-rp1", "pinctrl-bcm2711", "pinctrl-bcm2835"];

/// The GPIO chip of the Raspberry Pi header
fn header_chip() -> Result<Chip> {
    let chips = gpiocdev::chip::chips().context("Cannot list the GPIO chips")?;
    chips
        .iter()
        .filter_map(|path| Chip::from_path(path).ok())
        .find(|chip| {
            chip.info()
                .is_ok_and(|info| HEADER_CHIPS.contains(&info.label.as_str()))
        })
        .context("No Raspberry Pi GPIO chip found")
}

/// Takes a GPIO as an input with its pull-up, both edges reported. Through
/// the kernel: it refuses a line a driver holds, and sets the pull-up
/// itself (nothing is written to the GPIO registers behind its back)
fn request(chip: &Path, pin: u8, user: &str, debounce: Option<Duration>) -> Result<Request> {
    let mut builder = Request::builder();
    builder
        .on_chip(chip)
        .with_consumer(format!("OpenRig {user}"))
        .with_line(pin.into())
        .as_input()
        .with_bias(Bias::PullUp)
        .with_edge_detection(EdgeDetection::BothEdges);
    if let Some(debounce) = debounce {
        builder.with_debounce_period(debounce);
    }
    builder
        .request()
        .with_context(|| format!("{user}: GPIO {pin} is not available"))
}
