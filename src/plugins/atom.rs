//! Events written into the atom input ports of LV2 plugins before each run:
//! the transport (`time:Position`, so tempo-synced plugins follow the
//! pedalboard tempo) and MIDI messages. Plain byte layout from the LV2 atom
//! spec, written in place without allocating (audio thread).

use crate::midi::RawMidi;

pub const ATOM_OBJECT: &str = "http://lv2plug.in/ns/ext/atom#Object";
pub const ATOM_FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
pub const ATOM_LONG: &str = "http://lv2plug.in/ns/ext/atom#Long";
pub const ATOM_INT: &str = "http://lv2plug.in/ns/ext/atom#Int";
pub const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
pub const TIME_POSITION: &str = "http://lv2plug.in/ns/ext/time#Position";
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
}
