//! External rotary encoders on the I2C bus, one per field of the rack's
//! knob strip: encoder 1 drives the strip's first field on the page shown,
//! and so on. This module reads the boards through a [`Bus`] and turns
//! their counts and buttons into [`Event`]s; the app maps the events to the
//! strip and opens the real bus (`/dev/i2c-1`, feature `gpio`).
//!
//! Supported boards, found by scanning their addresses (encoders are
//! numbered by address, then by channel):
//! - Adafruit I2C QT Rotary Encoder (seesaw, product 4991): one encoder,
//!   0x36–0x3D (address jumpers);
//! - Adafruit Quad Rotary Encoder (seesaw, product 5752): four encoders,
//!   0x49–0x50;
//! - M5Stack Unit 8Encoder: eight encoders, 0x41.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Holding a button this long is a long press (as the GPIO encoder)
pub const LONG_PRESS: Duration = Duration::from_millis(600);
/// Counts per detent the boards can be set to
pub const STEPS: [u8; 3] = [1, 2, 4];

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
}

impl Module {
    pub const ALL: [Module; 3] = [Module::Seesaw, Module::SeesawQuad, Module::M5Stack8];

    pub fn name(self) -> &'static str {
        match self {
            Module::Seesaw => "Adafruit encoder",
            Module::SeesawQuad => "Adafruit Quad",
            Module::M5Stack8 => "M5Stack 8Encoder",
        }
    }

    fn addresses(self) -> std::ops::RangeInclusive<u8> {
        match self {
            Module::Seesaw => 0x36..=0x3D,
            Module::SeesawQuad => 0x49..=0x50,
            Module::M5Stack8 => 0x41..=0x41,
        }
    }

    /// Encoders on one board
    pub fn encoders(self) -> usize {
        match self {
            Module::Seesaw => 1,
            Module::SeesawQuad => 4,
            Module::M5Stack8 => 8,
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

impl Module {
    /// Product number in a seesaw board's version word
    fn product(self) -> u16 {
        match self {
            Module::Seesaw => 4991,
            Module::SeesawQuad => 5752,
            Module::M5Stack8 => 0,
        }
    }

    /// Seesaw pins of the push buttons, by channel
    fn button_pins(self) -> &'static [u32] {
        match self {
            Module::Seesaw => &[24],
            Module::SeesawQuad => &[12, 14, 17, 9],
            Module::M5Stack8 => &[],
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
            Module::M5Stack8 => {
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

    /// Each encoder's counts since the last read (clockwise positive) and
    /// whether its button is down
    fn read(&self, bus: &mut dyn Bus) -> std::io::Result<Vec<(i32, bool)>> {
        match self.module {
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
                        Ok((-i32::from_be_bytes(delta), levels & (1 << pin) == 0))
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

/// What an encoder did, `index` counting from 0 across the boards
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        self.counts += counts;
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
            (true, Some(since)) if !self.long_sent && now - since >= LONG_PRESS => {
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
}

impl Encoders {
    /// Finds and sets up the boards; none found is an error
    pub fn open(bus: &mut dyn Bus, settings: I2cEncoders) -> anyhow::Result<Self> {
        let boards = scan(bus, settings.module);
        if boards.is_empty() {
            anyhow::bail!("No {} answers on the I2C bus", settings.module.name());
        }
        for board in &boards {
            board.init(bus)?;
        }
        let count = boards.len() * settings.module.encoders();
        Ok(Self {
            settings,
            boards,
            trackers: vec![Tracker::default(); count],
        })
    }

    pub fn count(&self) -> usize {
        self.trackers.len()
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
        let sign = if self.settings.reverse { -1 } else { 1 };
        let mut events = Vec::new();
        let mut index = 0;
        for board in &self.boards {
            for (counts, down) in board.read(bus)? {
                self.trackers[index].update(index, sign * counts, down, steps, now, &mut events);
                index += 1;
            }
        }
        Ok(events)
    }
}

/// Simulated boards, for the tests and to try the app without them
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
                register: Vec::new(),
            }
        }

        fn answer(&mut self, buffer: &mut [u8]) {
            let data: Vec<u8> = match (self.module, self.register.as_slice()) {
                (Module::M5Stack8, [M5_FIRMWARE]) => vec![2],
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
            if device.module != Module::M5Stack8
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
        ]);
        let found =
            |bus: &mut SimBus, m| scan(bus, m).iter().map(|b| b.address).collect::<Vec<_>>();
        assert_eq!(found(&mut bus, Module::Seesaw), [0x36, 0x37]);
        // A quad board is not taken for single encoders, and the reverse
        assert_eq!(found(&mut bus, Module::SeesawQuad), [0x49]);
        assert_eq!(found(&mut bus, Module::M5Stack8), [0x41]);
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
}
