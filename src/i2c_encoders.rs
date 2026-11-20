//! External rotary encoders (or potentiometers) on the I2C bus, one per
//! field of the rack's knob strip: encoder 1 drives the strip's first field
//! on the page shown, and so on. This module reads the boards through a
//! [`Bus`] and turns their counts, buttons and pot positions into
//! [`Event`]s; the app maps the events to the strip and opens the real bus
//! (`/dev/i2c-1`, feature `gpio`).
//!
//! Supported boards, found by scanning their addresses (encoders are
//! numbered by address, then by channel):
//! - Adafruit I2C QT Rotary Encoder (seesaw, product 4991): one encoder,
//!   0x36–0x3D (address jumpers);
//! - Adafruit Quad Rotary Encoder (seesaw, product 5752): four encoders,
//!   0x49–0x50;
//! - M5Stack Unit 8Encoder: eight encoders, 0x41;
//! - M5Stack Unit 8Angle: eight potentiometers, a toggle switch and nine
//!   RGB LEDs, 0x43. Pots are absolute: they send [`Event::Set`] with their
//!   position, filtered so that a pot left alone sends nothing; the app
//!   lights the LEDs with [`Encoders::set_leds`]. Its address can be
//!   changed (register 0xFF), but only 0x43 is looked at: the strip uses six
//!   pots, so a second board would have nothing to drive.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::hardware::LONG_PRESS;

/// An I2C bus: plain writes and reads to a 7-bit address (an address that
/// does not answer is an error)
pub trait Bus {
    fn write(&mut self, address: u8, data: &[u8]) -> std::io::Result<()>;
    fn read(&mut self, address: u8, buffer: &mut [u8]) -> std::io::Result<()>;
    /// Wait for a seesaw board to prepare its answer
    fn pause(&mut self, time: Duration) {
        std::thread::sleep(time);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Module {
    /// Adafruit I2C QT Rotary Encoder, one encoder per board
    Seesaw,
    /// Adafruit Quad Rotary Encoder
    SeesawQuad,
    /// M5Stack Unit 8Encoder
    M5Stack8,
    /// M5Stack Unit 8Angle: potentiometers, not encoders
    M5Stack8Angle,
}

impl Module {
    pub const ALL: [Module; 4] = [
        Module::Seesaw,
        Module::SeesawQuad,
        Module::M5Stack8,
        Module::M5Stack8Angle,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Module::Seesaw => "Adafruit encoder",
            Module::SeesawQuad => "Adafruit Quad",
            Module::M5Stack8 => "M5Stack 8Encoder",
            Module::M5Stack8Angle => "M5Stack 8Angle",
        }
    }

    fn addresses(self) -> std::ops::RangeInclusive<u8> {
        match self {
            Module::Seesaw => 0x36..=0x3D,
            Module::SeesawQuad => 0x49..=0x50,
            Module::M5Stack8 => 0x41..=0x41,
            Module::M5Stack8Angle => 0x43..=0x43,
        }
    }

    /// Encoders (or pots) on one board
    pub fn encoders(self) -> usize {
        match self {
            Module::Seesaw => 1,
            Module::SeesawQuad => 4,
            Module::M5Stack8 | Module::M5Stack8Angle => 8,
        }
    }

    /// Potentiometers (absolute, with end stops) rather than encoders:
    /// counts per detent do not apply
    pub fn pots(self) -> bool {
        self == Module::M5Stack8Angle
    }

    /// RGB LEDs on one board: one per pot, then the switch's
    pub fn leds(self) -> usize {
        match self {
            Module::M5Stack8Angle => ANGLE_LEDS,
            _ => 0,
        }
    }
}

/// Settings → Hardware: the boards wired, how many counts a detent makes,
/// and whether turning right counts down
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct I2cEncoders {
    pub module: Module,
    #[serde(default = "one")]
    pub steps: u8,
    #[serde(default)]
    pub reverse: bool,
}

fn one() -> u8 {
    1
}

impl I2cEncoders {
    pub fn new(module: Module) -> Self {
        Self {
            module,
            steps: 1,
            reverse: false,
        }
    }
}

// Seesaw registers (module base, function)
const STATUS: u8 = 0x00;
const STATUS_HW_ID: u8 = 0x01;
const STATUS_VERSION: u8 = 0x02;
const GPIO: u8 = 0x01;
const GPIO_DIRCLR: u8 = 0x03;
const GPIO_BULK: u8 = 0x04;
const GPIO_BULK_SET: u8 = 0x05;
const GPIO_PULLENSET: u8 = 0x0B;
const ENCODER: u8 = 0x11;
const ENCODER_DELTA: u8 = 0x40;
/// Chips of seesaw boards (SAMD09, ATtiny8x6/8x7/16x6/16x7)
const SEESAW_IDS: [u8; 8] = [0x55, 0x46, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89];
/// Time a seesaw board needs before a read
const SEESAW_DELAY: Duration = Duration::from_micros(500);

// M5Stack 8Encoder registers
const M5_INCREMENT: u8 = 0x20;
const M5_BUTTON: u8 = 0x50;
const M5_FIRMWARE: u8 = 0xFE;

// M5Stack 8Angle registers (M5Stack's M5Unit-8Angle library and I2C
// protocol sheet): 12-bit ADC per pot (2 bytes, little endian, 0–4095),
// switch (0 or 1), LEDs (R, G, B, brightness 0–100; 0x50 is the switch's)
const ANGLE_ADC: u8 = 0x00;
const ANGLE_SWITCH: u8 = 0x20;
const ANGLE_LED: u8 = 0x30;
const ANGLE_LEDS: usize = 9;
const ADC_MAX: f32 = 4095.0;
/// Smallest move reported, in ADC counts (about 0.5 %): the noise of a pot
/// left alone stays below it
const DEADBAND: f32 = 20.0;
/// Readings this close to an end are the end, so both ends are reachable
const END_SNAP: f32 = 24.0;
/// Weight of a new reading in the smoothed position
const SMOOTHING: f32 = 0.5;
/// LEDs are written at most this often, and only those that changed
const LED_INTERVAL: Duration = Duration::from_millis(50);

impl Module {
    /// Product number in a seesaw board's version word
    fn product(self) -> u16 {
        match self {
            Module::Seesaw => 4991,
            Module::SeesawQuad => 5752,
            Module::M5Stack8 | Module::M5Stack8Angle => 0,
        }
    }

    /// Seesaw pins of the push buttons, by channel
    fn button_pins(self) -> &'static [u32] {
        match self {
            Module::Seesaw => &[24],
            Module::SeesawQuad => &[12, 14, 17, 9],
            Module::M5Stack8 | Module::M5Stack8Angle => &[],
        }
    }
}

fn seesaw_read(
    bus: &mut dyn Bus,
    address: u8,
    register: [u8; 2],
    buffer: &mut [u8],
) -> std::io::Result<()> {
    bus.write(address, &register)?;
    bus.pause(SEESAW_DELAY);
    bus.read(address, buffer)
}

/// One board found on the bus
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Board {
    pub module: Module,
    pub address: u8,
}

impl Board {
    fn is_there(&self, bus: &mut dyn Bus) -> bool {
        match self.module {
            Module::M5Stack8 | Module::M5Stack8Angle => {
                let mut version = [0u8];
                bus.write(self.address, &[M5_FIRMWARE]).is_ok()
                    && bus.read(self.address, &mut version).is_ok()
            }
            module => {
                let mut id = [0u8];
                let mut version = [0u8; 4];
                seesaw_read(bus, self.address, [STATUS, STATUS_HW_ID], &mut id).is_ok()
                    && SEESAW_IDS.contains(&id[0])
                    && seesaw_read(bus, self.address, [STATUS, STATUS_VERSION], &mut version)
                        .is_ok()
                    && u16::from_be_bytes([version[0], version[1]]) == module.product()
            }
        }
    }

    /// Buttons as inputs with pull-ups (seesaw)
    fn init(&self, bus: &mut dyn Bus) -> std::io::Result<()> {
        let pins = self.module.button_pins();
        if pins.is_empty() {
            return Ok(());
        }
        let mask = pins.iter().fold(0u32, |m, p| m | 1 << p).to_be_bytes();
        for function in [GPIO_DIRCLR, GPIO_PULLENSET, GPIO_BULK_SET] {
            let mut data = vec![GPIO, function];
            data.extend_from_slice(&mask);
            bus.write(self.address, &data)?;
        }
        Ok(())
    }

    /// Each pot's ADC reading (0–4095) and whether the switch is on
    fn read_pots(&self, bus: &mut dyn Bus) -> std::io::Result<(Vec<u16>, bool)> {
        let pots = (0..self.module.encoders() as u8)
            .map(|i| {
                let mut adc = [0u8; 2];
                bus.write(self.address, &[ANGLE_ADC + 2 * i])?;
                bus.read(self.address, &mut adc)?;
                Ok(u16::from_le_bytes(adc))
            })
            .collect::<std::io::Result<Vec<u16>>>()?;
        let mut switch = [0u8];
        bus.write(self.address, &[ANGLE_SWITCH])?;
        bus.read(self.address, &mut switch)?;
        Ok((pots, switch[0] != 0))
    }

    fn write_led(&self, bus: &mut dyn Bus, index: usize, led: Led) -> std::io::Result<()> {
        let [r, g, b] = led.rgb;
        let register = ANGLE_LED + 4 * index as u8;
        bus.write(self.address, &[register, r, g, b, led.brightness.min(100)])
    }

    /// Each encoder's counts since the last read (clockwise positive) and
    /// whether its button is down
    fn read(&self, bus: &mut dyn Bus) -> std::io::Result<Vec<(i32, bool)>> {
        match self.module {
            Module::M5Stack8Angle => Ok(Vec::new()),
            Module::M5Stack8 => {
                let mut buttons = [1u8; 8];
                bus.write(self.address, &[M5_BUTTON])?;
                bus.read(self.address, &mut buttons)?;
                (0..8u8)
                    .map(|i| {
                        let mut count = [0u8; 4];
                        // Reading an increment clears it
                        bus.write(self.address, &[M5_INCREMENT + 4 * i])?;
                        bus.read(self.address, &mut count)?;
                        // Released buttons read 1
                        Ok((i32::from_le_bytes(count), buttons[i as usize] == 0))
                    })
                    .collect()
            }
            module => {
                let mut levels = [0u8; 4];
                seesaw_read(bus, self.address, [GPIO, GPIO_BULK], &mut levels)?;
                let levels = u32::from_be_bytes(levels);
                module
                    .button_pins()
                    .iter()
                    .enumerate()
                    .map(|(i, pin)| {
                        let mut delta = [0u8; 4];
                        seesaw_read(
                            bus,
                            self.address,
                            [ENCODER, ENCODER_DELTA + i as u8],
                            &mut delta,
                        )?;
                        // Seesaw counts clockwise down; buttons pull low
                        // (saturating: a garbled read of i32::MIN must not overflow)
                        Ok((
                            i32::from_be_bytes(delta).saturating_neg(),
                            levels & (1 << pin) == 0,
                        ))
                    })
                    .collect()
            }
        }
    }
}

/// The boards of `module` that answer, by address
pub fn scan(bus: &mut dyn Bus, module: Module) -> Vec<Board> {
    module
        .addresses()
        .map(|address| Board { module, address })
        .filter(|board| board.is_there(bus))
        .collect()
}

/// What an encoder (or pot) did, `index` counting from 0 across the boards
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Detents, clockwise positive
    Turn {
        index: usize,
        detents: i32,
    },
    Press {
        index: usize,
    },
    LongPress {
        index: usize,
    },
    /// A pot moved to `value`: 0 at one end, 1 at the other
    Set {
        index: usize,
        value: f32,
    },
    /// A board's switch moved
    Switch {
        index: usize,
        on: bool,
    },
}

/// Colour and brightness (0–100) of a board's RGB LED
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Led {
    pub rgb: [u8; 3],
    pub brightness: u8,
}

impl Led {
    pub const OFF: Led = Led {
        rgb: [0; 3],
        brightness: 0,
    };
    /// A pot driving its field: the app's amber accent
    pub const LIVE: Led = Led {
        rgb: [0xF0, 0xA4, 0x3C],
        brightness: 30,
    };
    /// A pot waiting to reach its field's value (pickup): dim white
    pub const WAITING: Led = Led {
        rgb: [0xFF, 0xFF, 0xFF],
        brightness: 4,
    };
}

/// A pot's smoothed position and the last one reported, in ADC counts
#[derive(Debug, Default, Clone)]
struct Pot {
    smoothed: Option<f32>,
    reported: f32,
}

impl Pot {
    /// The new position (0–1) when the pot moved past the noise; nothing
    /// on the first reading
    fn update(&mut self, raw: u16) -> Option<f32> {
        let raw = f32::from(raw).min(ADC_MAX);
        let snap = |v: f32| {
            if v <= END_SNAP {
                0.0
            } else if v >= ADC_MAX - END_SNAP {
                ADC_MAX
            } else {
                v
            }
        };
        let Some(previous) = self.smoothed else {
            self.smoothed = Some(raw);
            self.reported = snap(raw);
            return None;
        };
        let smoothed = previous + (raw - previous) * SMOOTHING;
        self.smoothed = Some(smoothed);
        let position = snap(smoothed);
        let at_end = position == 0.0 || position == ADC_MAX;
        let moved =
            (position - self.reported).abs() >= DEADBAND || (at_end && position != self.reported);
        if !moved {
            return None;
        }
        self.reported = position;
        Some(position / ADC_MAX)
    }
}

/// Counts and button of one encoder, between reads
#[derive(Debug, Default, Clone)]
struct Tracker {
    counts: i32,
    down_since: Option<Instant>,
    long_sent: bool,
}

impl Tracker {
    fn update(
        &mut self,
        index: usize,
        counts: i32,
        down: bool,
        steps: i32,
        now: Instant,
        out: &mut Vec<Event>,
    ) {
        self.counts = self.counts.saturating_add(counts);
        let detents = self.counts / steps;
        if detents != 0 {
            self.counts -= detents * steps;
            out.push(Event::Turn { index, detents });
        }
        match (down, self.down_since) {
            (true, None) => {
                self.down_since = Some(now);
                self.long_sent = false;
            }
            (true, Some(since))
                if !self.long_sent && now.saturating_duration_since(since) >= LONG_PRESS =>
            {
                self.long_sent = true;
                out.push(Event::LongPress { index });
            }
            (false, Some(_)) => {
                self.down_since = None;
                if !self.long_sent {
                    out.push(Event::Press { index });
                }
            }
            _ => {}
        }
    }
}

/// The boards found and their encoders' state
pub struct Encoders {
    settings: I2cEncoders,
    boards: Vec<Board>,
    trackers: Vec<Tracker>,
    pots: Vec<Pot>,
    /// Each board's switch, once read
    switches: Vec<Option<bool>>,
    /// LEDs wanted and written (None: unknown, written at the next chance)
    leds: Vec<Led>,
    shown: Vec<Option<Led>>,
    leds_written: Option<Instant>,
}

impl Encoders {
    /// Finds and sets up the boards; none found is an error
    pub fn open(bus: &mut dyn Bus, settings: I2cEncoders) -> anyhow::Result<Self> {
        let module = settings.module;
        let boards = scan(bus, module);
        if boards.is_empty() {
            // The two M5Stack units look alike: say which one answers
            let other = match module {
                Module::M5Stack8 => Some(Module::M5Stack8Angle),
                Module::M5Stack8Angle => Some(Module::M5Stack8),
                _ => None,
            };
            if let Some(other) = other.filter(|&other| !scan(bus, other).is_empty()) {
                anyhow::bail!(
                    "No {} answers on the I2C bus, but an {} does: choose it as the board",
                    module.name(),
                    other.name()
                );
            }
            anyhow::bail!("No {} answers on the I2C bus", module.name());
        }
        for board in &boards {
            board.init(bus)?;
        }
        let count = boards.len() * module.encoders();
        let leds = boards.len() * module.leds();
        Ok(Self {
            settings,
            trackers: vec![Tracker::default(); count],
            pots: vec![Pot::default(); if module.pots() { count } else { 0 }],
            switches: vec![None; boards.len()],
            leds: vec![Led::OFF; leds],
            // Unknown: the first poll turns them all off
            shown: vec![None; leds],
            leds_written: None,
            boards,
        })
    }

    pub fn count(&self) -> usize {
        self.trackers.len()
    }

    /// "8 encoders found (0x41)", "8 pots found (0x43)"
    pub fn found(&self) -> String {
        let count = self.count();
        let noun = if self.settings.module.pots() {
            "pot"
        } else {
            "encoder"
        };
        let plural = if count == 1 { "" } else { "s" };
        format!("{count} {noun}{plural} found ({})", self.addresses())
    }

    /// Whether the boards have LEDs for [`Self::set_leds`]
    pub fn has_leds(&self) -> bool {
        !self.leds.is_empty()
    }

    /// The LEDs wanted, by board: one per pot, then the switch's. Written
    /// by the next polls, only those that changed
    pub fn set_leds(&mut self, leds: &[Led]) {
        for (wanted, led) in self.leds.iter_mut().zip(leds) {
            *wanted = *led;
        }
    }

    /// Turns every LED off now (the boards are let go)
    pub fn leds_off(&mut self, bus: &mut dyn Bus) -> std::io::Result<()> {
        self.leds.fill(Led::OFF);
        self.shown.fill(None);
        self.leds_written = None;
        self.write_leds(bus, Instant::now())
    }

    /// Writes the LEDs that changed, at most every [`LED_INTERVAL`]
    fn write_leds(&mut self, bus: &mut dyn Bus, now: Instant) -> std::io::Result<()> {
        if self
            .leds_written
            .is_some_and(|at| now.saturating_duration_since(at) < LED_INTERVAL)
            || self
                .leds
                .iter()
                .zip(&self.shown)
                .all(|(wanted, shown)| Some(*wanted) == *shown)
        {
            return Ok(());
        }
        let per_board = self.settings.module.leds();
        for (i, (wanted, shown)) in self.leds.iter().zip(self.shown.iter_mut()).enumerate() {
            if Some(*wanted) == *shown {
                continue;
            }
            if let Some(board) = self.boards.get(i / per_board.max(1)) {
                board.write_led(bus, i % per_board.max(1), *wanted)?;
            }
            *shown = Some(*wanted);
        }
        self.leds_written = Some(now);
        Ok(())
    }

    /// "0x36, 0x37"
    pub fn addresses(&self) -> String {
        let addresses: Vec<String> = self
            .boards
            .iter()
            .map(|b| format!("0x{:02X}", b.address))
            .collect();
        addresses.join(", ")
    }

    /// Reads every board once
    pub fn poll(&mut self, bus: &mut dyn Bus, now: Instant) -> std::io::Result<Vec<Event>> {
        let steps = i32::from(self.settings.steps.max(1));
        let reverse = self.settings.reverse;
        let mut events = Vec::new();
        if self.settings.module.pots() {
            self.poll_pots(bus, &mut events)?;
            self.write_leds(bus, now)?;
            return Ok(events);
        }
        let mut index = 0;
        for board in &self.boards {
            for (counts, down) in board.read(bus)? {
                let counts = if reverse {
                    counts.saturating_neg()
                } else {
                    counts
                };
                // One tracker per encoder of each board (see `open`)
                if let Some(tracker) = self.trackers.get_mut(index) {
                    tracker.update(index, counts, down, steps, now, &mut events);
                }
                index += 1;
            }
        }
        Ok(events)
    }

    /// Pots that moved past the noise, and switches that moved
    fn poll_pots(&mut self, bus: &mut dyn Bus, events: &mut Vec<Event>) -> std::io::Result<()> {
        let reverse = self.settings.reverse;
        let mut index = 0;
        for (b, board) in self.boards.iter().enumerate() {
            let (positions, on) = board.read_pots(bus)?;
            for raw in positions {
                if let Some(value) = self.pots.get_mut(index).and_then(|pot| pot.update(raw)) {
                    let value = if reverse { 1.0 - value } else { value };
                    events.push(Event::Set { index, value });
                }
                index += 1;
            }
            if let Some(switch) = self.switches.get_mut(b) {
                if switch.is_some_and(|was| was != on) {
                    events.push(Event::Switch { index: b, on });
                }
                *switch = Some(on);
            }
        }
        Ok(())
    }
}

/// Simulated boards, for the tests
#[cfg(test)]
pub mod sim {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Error, ErrorKind};

    #[derive(Debug, Clone)]
    pub struct Device {
        pub module: Module,
        /// Counts not read yet, by channel (clockwise positive)
        pub counts: Vec<i32>,
        pub down: Vec<bool>,
        /// Seesaw: pins set as inputs with pull-ups
        pub pull_ups: u32,
        /// 8Angle: ADC of each pot, the switch, the LEDs as written
        /// (R, G, B, brightness) and how many LED writes there were
        pub adc: Vec<u16>,
        pub switch: bool,
        pub leds: Vec<[u8; 4]>,
        pub led_writes: usize,
        register: Vec<u8>,
    }

    impl Device {
        pub fn new(module: Module) -> Self {
            let n = module.encoders();
            Self {
                module,
                counts: vec![0; n],
                down: vec![false; n],
                pull_ups: 0,
                adc: vec![2048; n],
                switch: false,
                leds: vec![[0xAA; 4]; module.leds()],
                led_writes: 0,
                register: Vec::new(),
            }
        }

        fn answer(&mut self, buffer: &mut [u8]) {
            let data: Vec<u8> = match (self.module, self.register.as_slice()) {
                (Module::M5Stack8 | Module::M5Stack8Angle, [M5_FIRMWARE]) => vec![2],
                (Module::M5Stack8Angle, [ANGLE_SWITCH]) => vec![u8::from(self.switch)],
                (Module::M5Stack8Angle, &[r]) if r < ANGLE_ADC + 16 && r % 2 == 0 => {
                    self.adc[usize::from(r / 2)].to_le_bytes().to_vec()
                }
                (Module::M5Stack8Angle, _) => vec![0; buffer.len()],
                (Module::M5Stack8, [M5_BUTTON]) => {
                    self.down.iter().map(|&d| u8::from(!d)).collect()
                }
                (Module::M5Stack8, &[r]) if (M5_INCREMENT..M5_INCREMENT + 32).contains(&r) => {
                    let i = usize::from((r - M5_INCREMENT) / 4);
                    std::mem::take(&mut self.counts[i]).to_le_bytes().to_vec()
                }
                (_, [STATUS, STATUS_HW_ID]) => vec![0x87],
                (module, [STATUS, STATUS_VERSION]) => {
                    let mut v = module.product().to_be_bytes().to_vec();
                    v.extend([0, 0]);
                    v
                }
                (module, [GPIO, GPIO_BULK]) => {
                    // Released buttons read high only with their pull-ups
                    let mut levels = 0u32;
                    for (i, pin) in module.button_pins().iter().enumerate() {
                        if !self.down[i] && self.pull_ups & (1 << pin) != 0 {
                            levels |= 1 << pin;
                        }
                    }
                    levels.to_be_bytes().to_vec()
                }
                (_, &[ENCODER, f]) if f >= ENCODER_DELTA => {
                    let i = usize::from(f - ENCODER_DELTA);
                    (-std::mem::take(&mut self.counts[i]))
                        .to_be_bytes()
                        .to_vec()
                }
                _ => vec![0; buffer.len()],
            };
            for (b, d) in buffer
                .iter_mut()
                .zip(data.iter().chain(std::iter::repeat(&0)))
            {
                *b = *d;
            }
        }
    }

    /// A bus with simulated boards on it
    #[derive(Debug, Default)]
    pub struct SimBus {
        pub devices: BTreeMap<u8, Device>,
    }

    impl SimBus {
        pub fn with(boards: &[(Module, u8)]) -> Self {
            Self {
                devices: boards.iter().map(|&(m, a)| (a, Device::new(m))).collect(),
            }
        }

        pub fn device(&mut self, address: u8) -> &mut Device {
            self.devices.get_mut(&address).expect("no such board")
        }
    }

    impl Bus for SimBus {
        fn write(&mut self, address: u8, data: &[u8]) -> std::io::Result<()> {
            let device = self
                .devices
                .get_mut(&address)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "no answer"))?;
            if device.module == Module::M5Stack8Angle {
                if let [r, rgb @ ..] = data {
                    if *r >= ANGLE_LED && rgb.len() == 4 {
                        let i = usize::from((r - ANGLE_LED) / 4);
                        device.leds[i] = [rgb[0], rgb[1], rgb[2], rgb[3]];
                        device.led_writes += 1;
                    }
                }
            }
            if device.module != Module::M5Stack8
                && device.module != Module::M5Stack8Angle
                && data.len() == 6
                && data[0] == GPIO
                && data[1] == GPIO_PULLENSET
            {
                device.pull_ups |= u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
            }
            device.register = data.to_vec();
            Ok(())
        }

        fn read(&mut self, address: u8, buffer: &mut [u8]) -> std::io::Result<()> {
            self.devices
                .get_mut(&address)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "no answer"))?
                .answer(buffer);
            Ok(())
        }

        fn pause(&mut self, _time: Duration) {}
    }
}

#[cfg(test)]
mod tests {
    use super::sim::SimBus;
    use super::*;

    #[test]
    fn scanning_finds_each_kind_by_address() {
        let mut bus = SimBus::with(&[
            (Module::Seesaw, 0x37),
            (Module::Seesaw, 0x36),
            (Module::SeesawQuad, 0x49),
            (Module::M5Stack8, 0x41),
            (Module::M5Stack8Angle, 0x43),
        ]);
        let found =
            |bus: &mut SimBus, m| scan(bus, m).iter().map(|b| b.address).collect::<Vec<_>>();
        assert_eq!(found(&mut bus, Module::Seesaw), [0x36, 0x37]);
        // A quad board is not taken for single encoders, and the reverse
        assert_eq!(found(&mut bus, Module::SeesawQuad), [0x49]);
        assert_eq!(found(&mut bus, Module::M5Stack8), [0x41]);
        assert_eq!(found(&mut bus, Module::M5Stack8Angle), [0x43]);
        assert!(Encoders::open(&mut SimBus::default(), I2cEncoders::new(Module::Seesaw)).is_err());
    }

    #[test]
    fn turns_are_numbered_across_boards() {
        let mut bus = SimBus::with(&[(Module::Seesaw, 0x36), (Module::Seesaw, 0x38)]);
        let mut encoders = Encoders::open(&mut bus, I2cEncoders::new(Module::Seesaw)).unwrap();
        assert_eq!(encoders.count(), 2);
        assert_eq!(encoders.addresses(), "0x36, 0x38");
        bus.device(0x38).counts[0] = 3;
        bus.device(0x36).counts[0] = -1;
        let now = Instant::now();
        assert_eq!(
            encoders.poll(&mut bus, now).unwrap(),
            [
                Event::Turn {
                    index: 0,
                    detents: -1
                },
                Event::Turn {
                    index: 1,
                    detents: 3
                }
            ]
        );
        // Counts are read once
        assert!(encoders.poll(&mut bus, now).unwrap().is_empty());
    }

    #[test]
    fn counts_per_detent_and_reverse() {
        let mut bus = SimBus::with(&[(Module::M5Stack8, 0x41)]);
        let settings = I2cEncoders {
            module: Module::M5Stack8,
            steps: 2,
            reverse: true,
        };
        let mut encoders = Encoders::open(&mut bus, settings).unwrap();
        assert_eq!(encoders.count(), 8);
        let now = Instant::now();
        bus.device(0x41).counts[5] = 1;
        assert!(encoders.poll(&mut bus, now).unwrap().is_empty());
        bus.device(0x41).counts[5] = 3;
        assert_eq!(
            encoders.poll(&mut bus, now).unwrap(),
            [Event::Turn {
                index: 5,
                detents: -2
            }]
        );
    }

    #[test]
    fn short_and_long_presses() {
        let mut bus = SimBus::with(&[(Module::SeesawQuad, 0x4A)]);
        let mut encoders = Encoders::open(&mut bus, I2cEncoders::new(Module::SeesawQuad)).unwrap();
        // The buttons got their pull-ups: released reads released
        let t = Instant::now();
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        bus.device(0x4A).down[2] = true;
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        bus.device(0x4A).down[2] = false;
        assert_eq!(
            encoders
                .poll(&mut bus, t + Duration::from_millis(100))
                .unwrap(),
            [Event::Press { index: 2 }]
        );

        bus.device(0x4A).down[3] = true;
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        let held = t + LONG_PRESS;
        assert_eq!(
            encoders.poll(&mut bus, held).unwrap(),
            [Event::LongPress { index: 3 }]
        );
        assert!(encoders
            .poll(&mut bus, held + LONG_PRESS)
            .unwrap()
            .is_empty());
        // No press on release after a long press
        bus.device(0x4A).down[3] = false;
        assert!(encoders
            .poll(&mut bus, held + LONG_PRESS)
            .unwrap()
            .is_empty());
    }

    fn angle() -> (SimBus, Encoders) {
        let mut bus = SimBus::with(&[(Module::M5Stack8Angle, 0x43)]);
        let encoders = Encoders::open(&mut bus, I2cEncoders::new(Module::M5Stack8Angle)).unwrap();
        (bus, encoders)
    }

    #[test]
    fn the_8angle_is_told_from_the_8encoder() {
        let (_, encoders) = angle();
        assert_eq!(encoders.found(), "8 pots found (0x43)");
        // The wrong M5Stack unit chosen: the error names the right one
        let mut bus = SimBus::with(&[(Module::M5Stack8Angle, 0x43)]);
        let error = Encoders::open(&mut bus, I2cEncoders::new(Module::M5Stack8))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("M5Stack 8Angle does"), "{error}");
        // Saved by name; the older names stay
        let json = |m: Module| serde_json::to_string(&m).unwrap();
        assert_eq!(json(Module::M5Stack8Angle), "\"m5-stack8-angle\"");
        assert_eq!(json(Module::M5Stack8), "\"m5-stack8\"");
        assert!(Module::M5Stack8Angle.pots() && !Module::M5Stack8.pots());
    }

    #[test]
    fn a_still_pot_sends_nothing_and_a_moved_one_its_position() {
        let (mut bus, mut encoders) = angle();
        let t = Instant::now();
        // The first reading is where the pots are, not a move
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        // Noise within the deadband
        for adc in [2050, 2040, 2058, 2044] {
            bus.device(0x43).adc[0] = adc;
            assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        }
        bus.device(0x43).adc[2] = 3000;
        let events = encoders.poll(&mut bus, t).unwrap();
        let [Event::Set { index: 2, value }] = events[..] else {
            panic!("{events:?}");
        };
        assert!(value > 0.5 && value < 0.74, "{value}");
        // Smoothed: it settles on the new place, then is quiet
        let mut last = value;
        for _ in 0..20 {
            for event in encoders.poll(&mut bus, t).unwrap() {
                if let Event::Set { index: 2, value } = event {
                    last = value;
                }
            }
        }
        assert!((last - 3000.0 / 4095.0).abs() < 0.01, "{last}");
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        // The ends are reached exactly
        bus.device(0x43).adc[4] = 4090;
        let mut end = 0.0;
        for _ in 0..20 {
            for event in encoders.poll(&mut bus, t).unwrap() {
                if let Event::Set { index: 4, value } = event {
                    end = value;
                }
            }
        }
        assert_eq!(end, 1.0);
    }

    #[test]
    fn reverse_flips_the_pots_and_the_switch_reports_its_moves() {
        let mut bus = SimBus::with(&[(Module::M5Stack8Angle, 0x43)]);
        let settings = I2cEncoders {
            reverse: true,
            ..I2cEncoders::new(Module::M5Stack8Angle)
        };
        let mut encoders = Encoders::open(&mut bus, settings).unwrap();
        let t = Instant::now();
        bus.device(0x43).switch = true;
        // Where the switch is at first is not a move
        assert!(encoders.poll(&mut bus, t).unwrap().is_empty());
        bus.device(0x43).adc[0] = 0;
        bus.device(0x43).switch = false;
        let events = encoders.poll(&mut bus, t).unwrap();
        let [Event::Set { index: 0, value }, Event::Switch {
            index: 0,
            on: false,
        }] = events[..]
        else {
            panic!("{events:?}");
        };
        assert!(value > 0.7, "{value}");
    }

    #[test]
    fn leds_are_written_when_they_change_and_not_too_often() {
        let (mut bus, mut encoders) = angle();
        assert!(encoders.has_leds());
        let t = Instant::now();
        // The first poll turns them all off
        encoders.poll(&mut bus, t).unwrap();
        assert_eq!(bus.device(0x43).led_writes, 9);
        assert_eq!(bus.device(0x43).leds, vec![[0; 4]; 9]);
        // Nothing changed: nothing written
        let later = t + LED_INTERVAL;
        encoders.poll(&mut bus, later).unwrap();
        assert_eq!(bus.device(0x43).led_writes, 9);
        let mut leds = [Led::OFF; 9];
        leds[1] = Led::LIVE;
        leds[8] = Led::WAITING;
        encoders.set_leds(&leds);
        // Too soon after the last write: kept for later
        encoders.poll(&mut bus, t).unwrap();
        assert_eq!(bus.device(0x43).led_writes, 9);
        encoders.poll(&mut bus, later).unwrap();
        assert_eq!(bus.device(0x43).led_writes, 11);
        assert_eq!(bus.device(0x43).leds[1], [0xF0, 0xA4, 0x3C, 30]);
        assert_eq!(bus.device(0x43).leds[8], [0xFF, 0xFF, 0xFF, 4]);
        encoders.leds_off(&mut bus).unwrap();
        assert_eq!(bus.device(0x43).leds, vec![[0; 4]; 9]);
        // Encoder boards have none
        let mut bus = SimBus::with(&[(Module::M5Stack8, 0x41)]);
        let encoders = Encoders::open(&mut bus, I2cEncoders::new(Module::M5Stack8)).unwrap();
        assert!(!encoders.has_leds());
    }
}
