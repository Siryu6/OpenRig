//! Events written into the atom input ports of LV2 plugins before each run:
//! the transport (`time:Position`, so tempo-synced plugins follow the
//! pedalboard tempo) and MIDI messages, plus the `patch:Set` messages that
//! give a plugin its files. Plain byte layout from the LV2 atom spec,
//! written in place without allocating (audio thread).

use crate::midi::RawMidi;

pub const ATOM_OBJECT: &str = "http://lv2plug.in/ns/ext/atom#Object";
pub const ATOM_FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
pub const ATOM_LONG: &str = "http://lv2plug.in/ns/ext/atom#Long";
pub const ATOM_INT: &str = "http://lv2plug.in/ns/ext/atom#Int";
pub const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
pub const TIME_POSITION: &str = "http://lv2plug.in/ns/ext/time#Position";
pub const PATCH_MESSAGE: &str = "http://lv2plug.in/ns/ext/patch#Message";
const ATOM_PATH: &str = "http://lv2plug.in/ns/ext/atom#Path";
const ATOM_URID: &str = "http://lv2plug.in/ns/ext/atom#URID";
const PATCH_SET: &str = "http://lv2plug.in/ns/ext/patch#Set";
const PATCH_PROPERTY: &str = "http://lv2plug.in/ns/ext/patch#property";
const PATCH_VALUE: &str = "http://lv2plug.in/ns/ext/patch#value";
const TIME_SPEED: &str = "http://lv2plug.in/ns/ext/time#speed";
const TIME_BPM: &str = "http://lv2plug.in/ns/ext/time#beatsPerMinute";
const TIME_BAR: &str = "http://lv2plug.in/ns/ext/time#bar";
const TIME_BAR_BEAT: &str = "http://lv2plug.in/ns/ext/time#barBeat";
const TIME_BEAT_UNIT: &str = "http://lv2plug.in/ns/ext/time#beatUnit";
const TIME_BEATS_PER_BAR: &str = "http://lv2plug.in/ns/ext/time#beatsPerBar";

/// URIDs used in the events, mapped once per plugin instance
#[derive(Debug, Clone, Copy, Default)]
pub struct AtomUrids {
    pub sequence: u32,
    pub object: u32,
    pub float: u32,
    pub long: u32,
    pub int: u32,
    pub midi_event: u32,
    pub position: u32,
    pub speed: u32,
    pub bpm: u32,
    pub bar: u32,
    pub bar_beat: u32,
    pub beat_unit: u32,
    pub beats_per_bar: u32,
    pub path: u32,
    pub urid: u32,
    pub patch_set: u32,
    pub patch_property: u32,
    pub patch_value: u32,
}

impl AtomUrids {
    pub fn new(sequence: u32, map: impl Fn(&str) -> u32) -> Self {
        Self {
            sequence,
            object: map(ATOM_OBJECT),
            float: map(ATOM_FLOAT),
            long: map(ATOM_LONG),
            int: map(ATOM_INT),
            midi_event: map(MIDI_EVENT),
            position: map(TIME_POSITION),
            speed: map(TIME_SPEED),
            bpm: map(TIME_BPM),
            bar: map(TIME_BAR),
            bar_beat: map(TIME_BAR_BEAT),
            beat_unit: map(TIME_BEAT_UNIT),
            beats_per_bar: map(TIME_BEATS_PER_BAR),
            path: map(ATOM_PATH),
            urid: map(ATOM_URID),
            patch_set: map(PATCH_SET),
            patch_property: map(PATCH_PROPERTY),
            patch_value: map(PATCH_VALUE),
        }
    }
}

/// Appends 8-byte aligned atoms to a buffer; drops what does not fit
struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_ne_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.buf[self.at..self.at + b.len()].copy_from_slice(b);
        self.at += b.len();
    }

    fn pad(&mut self) {
        while !self.at.is_multiple_of(8) {
            self.buf[self.at] = 0;
            self.at += 1;
        }
    }

    fn fits(&self, size: usize) -> bool {
        self.at + size <= self.buf.len()
    }

    /// Event header at frame 0, then the atom header of its body
    fn event(&mut self, size: u32, kind: u32) {
        self.bytes(&0i64.to_ne_bytes());
        self.u32(size);
        self.u32(kind);
    }

    /// Object property: key, context, then a 4- or 8-byte value atom
    fn property(&mut self, key: u32, kind: u32, value: &[u8]) {
        self.u32(key);
        self.u32(0);
        self.u32(value.len() as u32);
        self.u32(kind);
        self.bytes(value);
        self.pad();
    }
}

/// Size of an event body holding a transport position (object body and its
/// six properties, each padded to 24 bytes)
const POSITION_BODY: usize = 8 + 6 * 24;

/// Write an input sequence holding the transport position at `bpm` (when
/// given) and `midi`, all at frame 0. The buffer must start 8-byte aligned.
pub fn write_input(buf: &mut [u8], urids: &AtomUrids, bpm: Option<f32>, midi: &[RawMidi]) {
    let mut w = Writer { buf, at: 0 };
    // Sequence header; its size is set once the events are known
    w.u32(0);
    w.u32(urids.sequence);
    w.u32(0); // unit: frames
    w.u32(0);
    if let Some(bpm) = bpm.filter(|_| w.fits(16 + POSITION_BODY)) {
        w.event(POSITION_BODY as u32, urids.object);
        w.u32(0); // id
        w.u32(urids.position);
        // The pedalboard is always "playing": synced LFOs and delays run
        w.property(urids.speed, urids.float, &1.0f32.to_ne_bytes());
        w.property(urids.bpm, urids.float, &bpm.to_ne_bytes());
        w.property(urids.bar, urids.long, &0i64.to_ne_bytes());
        w.property(urids.bar_beat, urids.float, &0.0f32.to_ne_bytes());
        w.property(urids.beat_unit, urids.int, &4i32.to_ne_bytes());
        w.property(urids.beats_per_bar, urids.float, &4.0f32.to_ne_bytes());
    }
    for message in midi {
        if !w.fits(16 + 8) {
            break;
        }
        w.event(u32::from(message.len), urids.midi_event);
        w.bytes(message.as_slice());
        w.pad();
    }
    let size = (w.at - 8) as u32;
    w.buf[..4].copy_from_slice(&size.to_ne_bytes());
}

/// Size of the body of a `patch:Set` of a path of `len` bytes: object
/// header, the property (a URID) and the value (the path, NUL-terminated)
fn patch_set_body(len: usize) -> usize {
    8 + 24 + (16 + len + 1).next_multiple_of(8)
}

/// Write an input sequence holding a `patch:Set` of each (property URID,
/// path), at frame 0. Sets that do not fit are dropped; returns how many
/// were written. The buffer must start 8-byte aligned.
pub fn write_patch_sets(buf: &mut [u8], urids: &AtomUrids, sets: &[(u32, &str)]) -> usize {
    let mut w = Writer { buf, at: 0 };
    w.u32(0);
    w.u32(urids.sequence);
    w.u32(0); // unit: frames
    w.u32(0);
    let mut written = 0;
    for &(property, path) in sets {
        let body = patch_set_body(path.len());
        if !w.fits(16 + body) {
            break;
        }
        w.event(body as u32, urids.object);
        w.u32(0); // id
        w.u32(urids.patch_set);
        w.property(urids.patch_property, urids.urid, &property.to_ne_bytes());
        w.u32(urids.patch_value);
        w.u32(0);
        w.u32(path.len() as u32 + 1);
        w.u32(urids.path);
        w.bytes(path.as_bytes());
        w.bytes(&[0]);
        w.pad();
        written += 1;
    }
    let size = (w.at - 8) as u32;
    w.buf[..4].copy_from_slice(&size.to_ne_bytes());
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urids() -> AtomUrids {
        let names = [
            ATOM_OBJECT,
            ATOM_FLOAT,
            ATOM_LONG,
            ATOM_INT,
            MIDI_EVENT,
            TIME_POSITION,
            TIME_SPEED,
            TIME_BPM,
            TIME_BAR,
            TIME_BAR_BEAT,
            TIME_BEAT_UNIT,
            TIME_BEATS_PER_BAR,
            ATOM_PATH,
            ATOM_URID,
            PATCH_SET,
            PATCH_PROPERTY,
            PATCH_VALUE,
        ];
        AtomUrids::new(1, |uri| {
            names.iter().position(|n| *n == uri).unwrap() as u32 + 2
        })
    }

    fn u32_at(buf: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn empty_sequence() {
        let mut buf = [0xffu8; 64];
        write_input(&mut buf, &urids(), None, &[]);
        assert_eq!(u32_at(&buf, 0), 8);
        assert_eq!(u32_at(&buf, 4), 1);
        assert_eq!(u32_at(&buf, 8), 0);
    }

    #[test]
    fn position_then_midi() {
        let u = urids();
        let mut buf = [0u8; 512];
        let note = RawMidi {
            bytes: [0x90, 60, 100],
            len: 3,
        };
        write_input(&mut buf, &u, Some(120.0), &[note]);
        // Position event: frame, body atom, object body
        assert_eq!(u32_at(&buf, 24), POSITION_BODY as u32);
        assert_eq!(u32_at(&buf, 28), u.object);
        assert_eq!(u32_at(&buf, 36), u.position);
        // Second property is the tempo
        let bpm = 40 + 24;
        assert_eq!(u32_at(&buf, bpm), u.bpm);
        assert_eq!(u32_at(&buf, bpm + 12), u.float);
        assert_eq!(
            f32::from_ne_bytes(buf[bpm + 16..bpm + 20].try_into().unwrap()),
            120.0
        );
        // MIDI event right after, 8-byte aligned
        let midi = 16 + 16 + POSITION_BODY;
        assert_eq!(midi % 8, 0);
        assert_eq!(u32_at(&buf, midi + 8), 3);
        assert_eq!(u32_at(&buf, midi + 12), u.midi_event);
        assert_eq!(&buf[midi + 16..midi + 19], &[0x90, 60, 100]);
        // Sequence size covers the body and both events
        assert_eq!(u32_at(&buf, 0) as usize, 8 + 16 + POSITION_BODY + 16 + 8);
    }

    #[test]
    fn events_that_do_not_fit_are_dropped() {
        let mut buf = [0u8; 48];
        let note = RawMidi {
            bytes: [0x80, 60, 0],
            len: 3,
        };
        write_input(&mut buf, &urids(), Some(90.0), &[note, note]);
        // No room for the position; one note fits
        assert_eq!(u32_at(&buf, 0), 8 + 24);
    }

    #[test]
    fn patch_set_of_a_path() {
        let u = urids();
        let mut buf = [0u8; 256];
        assert_eq!(write_patch_sets(&mut buf, &u, &[(99, "/m/a.nam")]), 1);
        // Event: frame, then an object atom of the patch:Set body
        let body = u32_at(&buf, 24) as usize;
        assert_eq!(body, patch_set_body(8));
        assert_eq!(u32_at(&buf, 28), u.object);
        assert_eq!(u32_at(&buf, 36), u.patch_set);
        // patch:property is the parameter, as a URID
        assert_eq!(u32_at(&buf, 40), u.patch_property);
        assert_eq!(u32_at(&buf, 52), u.urid);
        assert_eq!(u32_at(&buf, 56), 99);
        // patch:value is the path, NUL-terminated
        assert_eq!(u32_at(&buf, 64), u.patch_value);
        assert_eq!(u32_at(&buf, 72), 9);
        assert_eq!(u32_at(&buf, 76), u.path);
        assert_eq!(&buf[80..89], b"/m/a.nam\0");
        // The sequence covers its body and the padded event
        assert_eq!(u32_at(&buf, 0) as usize, 8 + 16 + body);
        assert_eq!(body % 8, 0);
    }

    #[test]
    fn patch_sets_that_do_not_fit_are_dropped() {
        let mut buf = [0u8; 96];
        let long = "x".repeat(64);
        assert_eq!(
            write_patch_sets(&mut buf, &urids(), &[(1, "a"), (2, &long)]),
            1
        );
    }
}
