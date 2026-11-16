//! Events written into the atom input ports of LV2 plugins before each run:
//! the transport (`time:Position` at every run, so tempo-synced plugins
//! follow the pedalboard tempo and bar) and MIDI messages, plus the `patch:Set` messages that
//! give a plugin its files; and the `openrig:Frame` objects a plugin sends
//! on a stream output for its view, read after a run. Plain byte layout
//! from the LV2 atom spec, written and read in place without allocating
//! (audio thread).

use crate::audio::transport::{Position, BEATS_PER_BAR, BEAT_UNIT};
use crate::midi::RawMidi;

pub const ATOM_OBJECT: &str = "http://lv2plug.in/ns/ext/atom#Object";
pub const ATOM_FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
pub const ATOM_DOUBLE: &str = "http://lv2plug.in/ns/ext/atom#Double";
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
const TIME_FRAME: &str = "http://lv2plug.in/ns/ext/time#frame";
const TIME_BEAT: &str = "http://lv2plug.in/ns/ext/time#beat";
const TIME_SPEED: &str = "http://lv2plug.in/ns/ext/time#speed";
const TIME_BPM: &str = "http://lv2plug.in/ns/ext/time#beatsPerMinute";
const TIME_BAR: &str = "http://lv2plug.in/ns/ext/time#bar";
const TIME_BAR_BEAT: &str = "http://lv2plug.in/ns/ext/time#barBeat";
const TIME_BEAT_UNIT: &str = "http://lv2plug.in/ns/ext/time#beatUnit";
const TIME_BEATS_PER_BAR: &str = "http://lv2plug.in/ns/ext/time#beatsPerBar";
const ATOM_VECTOR: &str = "http://lv2plug.in/ns/ext/atom#Vector";
const STREAM_FRAME: &str = "https://openrig.eu/ns/view#Frame";
const STREAM_CHANNELS: &str = "https://openrig.eu/ns/view#channels";
const STREAM_POSITION: &str = "https://openrig.eu/ns/view#position";
const STREAM_VALUES: &str = "https://openrig.eu/ns/view#values";

/// URIDs used in the events, mapped once per plugin instance
#[derive(Debug, Clone, Copy, Default)]
pub struct AtomUrids {
    pub sequence: u32,
    pub object: u32,
    pub float: u32,
    pub double: u32,
    pub long: u32,
    pub int: u32,
    pub midi_event: u32,
    pub position: u32,
    pub frame: u32,
    pub beat: u32,
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
    pub vector: u32,
    pub stream_frame: u32,
    pub stream_channels: u32,
    pub stream_position: u32,
    pub stream_values: u32,
}

impl AtomUrids {
    pub fn new(sequence: u32, map: impl Fn(&str) -> u32) -> Self {
        Self {
            sequence,
            object: map(ATOM_OBJECT),
            float: map(ATOM_FLOAT),
            double: map(ATOM_DOUBLE),
            long: map(ATOM_LONG),
            int: map(ATOM_INT),
            midi_event: map(MIDI_EVENT),
            position: map(TIME_POSITION),
            frame: map(TIME_FRAME),
            beat: map(TIME_BEAT),
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
            vector: map(ATOM_VECTOR),
            stream_frame: map(STREAM_FRAME),
            stream_channels: map(STREAM_CHANNELS),
            stream_position: map(STREAM_POSITION),
            stream_values: map(STREAM_VALUES),
        }
    }
}

/// Appends 8-byte aligned atoms to a buffer; drops what does not fit
/// (callers check `fits` first: past the end, bytes are not written)
struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_ne_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        let end = self.at.saturating_add(b.len());
        if let Some(dst) = self.buf.get_mut(self.at..end) {
            dst.copy_from_slice(b);
        }
        self.at = end;
    }

    fn pad(&mut self) {
        while !self.at.is_multiple_of(8) {
            if let Some(byte) = self.buf.get_mut(self.at) {
                *byte = 0;
            }
            self.at += 1;
        }
    }

    fn fits(&self, size: usize) -> bool {
        self.at.saturating_add(size) <= self.buf.len()
    }

    /// Write the size of the sequence, now that its events are known
    fn finish(self) {
        let size = self.at.saturating_sub(8) as u32;
        if let Some(header) = self.buf.get_mut(..4) {
            header.copy_from_slice(&size.to_ne_bytes());
        }
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
/// eight properties, each padded to 24 bytes)
const POSITION_BODY: usize = 8 + 8 * 24;

/// Write an input sequence holding the transport `position` (when given)
/// and `midi`, all at frame 0. The buffer must start 8-byte aligned.
pub fn write_input(
    buf: &mut [u8],
    urids: &AtomUrids,
    position: Option<&Position>,
    midi: &[RawMidi],
) {
    let mut w = Writer { buf, at: 0 };
    // Sequence header; its size is set once the events are known
    w.u32(0);
    w.u32(urids.sequence);
    w.u32(0); // unit: frames
    w.u32(0);
    if let Some(p) = position.filter(|_| w.fits(16 + POSITION_BODY)) {
        w.event(POSITION_BODY as u32, urids.object);
        w.u32(0); // id
        w.u32(urids.position);
        w.property(urids.frame, urids.long, &(p.frame as i64).to_ne_bytes());
        w.property(urids.speed, urids.float, &p.speed().to_ne_bytes());
        w.property(urids.bpm, urids.float, &p.bpm.to_ne_bytes());
        w.property(urids.bar, urids.long, &p.bar().to_ne_bytes());
        w.property(urids.bar_beat, urids.float, &p.bar_beat().to_ne_bytes());
        w.property(urids.beat, urids.double, &p.beat.to_ne_bytes());
        w.property(
            urids.beat_unit,
            urids.int,
            &(BEAT_UNIT as i32).to_ne_bytes(),
        );
        w.property(
            urids.beats_per_bar,
            urids.float,
            &(BEATS_PER_BAR as f32).to_ne_bytes(),
        );
    }
    for message in midi {
        let bytes = message.as_slice();
        if !w.fits(16 + bytes.len().next_multiple_of(8)) {
            break;
        }
        w.event(bytes.len() as u32, urids.midi_event);
        w.bytes(bytes);
        w.pad();
    }
    w.finish();
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
    w.finish();
    written
}

/// A frame a plugin sent on a stream output (`openrig:Frame`): its rows,
/// position and values, channel after channel, read in place
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame<'a> {
    pub channels: u32,
    pub position: f32,
    /// The values as the plugin wrote them, 4 bytes each
    values: &'a [u8],
}

impl Frame<'_> {
    pub fn values(&self) -> impl Iterator<Item = f32> + '_ {
        self.values
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b))
    }

    pub fn len(&self) -> usize {
        self.values.len() / 4
    }

    pub fn is_empty(&self) -> bool {
        self.values.len() < 4
    }
}

fn u32_in(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

/// The atom at `at` in `buf`: its type and body
fn atom_in(buf: &[u8], at: usize) -> Option<(u32, &[u8])> {
    let size = u32_in(buf, at)? as usize;
    let kind = u32_in(buf, at + 4)?;
    let start = at.checked_add(8)?;
    Some((kind, buf.get(start..start.checked_add(size)?)?))
}

/// The last `openrig:Frame` of an output sequence the plugin wrote (`None`
/// if it sent none in that run). Anything malformed is skipped; never
/// reads past the buffer (real-time safe).
pub fn last_frame<'a>(buf: &'a [u8], urids: &AtomUrids) -> Option<Frame<'a>> {
    let (kind, body) = atom_in(buf, 0)?;
    if kind != urids.sequence {
        return None;
    }
    // Sequence body: unit and padding, then events (time, atom)
    let mut at = 8;
    let mut last = None;
    while let Some((kind, event)) = atom_in(body, at + 8) {
        if kind == urids.object && u32_in(event, 4) == Some(urids.stream_frame) {
            last = Some(event);
        }
        at += 16 + event.len().next_multiple_of(8);
    }
    let object = last?;
    // Object body: id and type, then properties (key, context, value atom)
    let (mut channels, mut position, mut values) = (None, 0.0, None);
    let mut at = 8;
    while let Some(key) = u32_in(object, at) {
        let Some((kind, value)) = atom_in(object, at + 8) else {
            break;
        };
        let word = u32_in(value, 0);
        if key == urids.stream_channels && kind == urids.int {
            channels = word;
        } else if key == urids.stream_position && kind == urids.float {
            position = word.map_or(0.0, f32::from_bits);
        } else if key == urids.stream_values
            && kind == urids.vector
            && word == Some(4)
            && u32_in(value, 4) == Some(urids.float)
        {
            values = value.get(8..);
        }
        at += 16 + value.len().next_multiple_of(8);
    }
    Some(Frame {
        channels: channels.filter(|c| (1..=i32::MAX as u32).contains(c))?,
        position: if position.is_finite() { position } else { 0.0 },
        values: values?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urids() -> AtomUrids {
        let names = [
            ATOM_OBJECT,
            ATOM_FLOAT,
            ATOM_DOUBLE,
            ATOM_LONG,
            ATOM_INT,
            MIDI_EVENT,
            TIME_POSITION,
            TIME_FRAME,
            TIME_BEAT,
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
            ATOM_VECTOR,
            STREAM_FRAME,
            STREAM_CHANNELS,
            STREAM_POSITION,
            STREAM_VALUES,
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
        // Bar 2, beat 1.5 (5.5 beats from the start)
        let position = Position {
            frame: 132_000,
            beat: 5.5,
            bpm: 120.0,
            sample_rate: 48_000.0,
        };
        write_input(&mut buf, &u, Some(&position), &[note]);
        // Position event: frame, body atom, object body
        assert_eq!(u32_at(&buf, 24), POSITION_BODY as u32);
        assert_eq!(u32_at(&buf, 28), u.object);
        assert_eq!(u32_at(&buf, 36), u.position);
        // Properties in order: key at +0, value type at +12, value at +16
        let property = |n: usize| 40 + n * 24;
        let value = |n: usize| &buf[property(n) + 16..property(n) + 24];
        let f32_of = |n: usize| f32::from_ne_bytes(value(n)[..4].try_into().unwrap());
        let i64_of = |n: usize| i64::from_ne_bytes(value(n).try_into().unwrap());
        let keys: Vec<(u32, u32)> = (0..8)
            .map(|n| (u32_at(&buf, property(n)), u32_at(&buf, property(n) + 12)))
            .collect();
        assert_eq!(
            keys,
            [
                (u.frame, u.long),
                (u.speed, u.float),
                (u.bpm, u.float),
                (u.bar, u.long),
                (u.bar_beat, u.float),
                (u.beat, u.double),
                (u.beat_unit, u.int),
                (u.beats_per_bar, u.float),
            ]
        );
        assert_eq!(i64_of(0), 132_000);
        assert_eq!(f32_of(1), 1.0);
        assert_eq!(f32_of(2), 120.0);
        assert_eq!(i64_of(3), 1);
        assert_eq!(f32_of(4), 1.5);
        assert_eq!(f64::from_ne_bytes(value(5).try_into().unwrap()), 5.5);
        assert_eq!(i32::from_ne_bytes(value(6)[..4].try_into().unwrap()), 4);
        assert_eq!(f32_of(7), 4.0);
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
        let position = Position {
            frame: 0,
            beat: 0.0,
            bpm: 90.0,
            sample_rate: 48_000.0,
        };
        write_input(&mut buf, &urids(), Some(&position), &[note, note]);
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
    /// An output sequence as a plugin writes it: a MIDI event, then one
    /// `openrig:Frame` per (position, values), three rows each
    fn stream(u: &AtomUrids, frames: &[(f32, &[f32])]) -> Vec<u64> {
        let mut words = vec![0u64; 256];
        let buf: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        let mut w = Writer { buf, at: 0 };
        w.u32(0);
        w.u32(u.sequence);
        w.u32(0);
        w.u32(0);
        w.event(3, u.midi_event);
        w.bytes(&[0x90, 60, 100]);
        w.pad();
        for (position, values) in frames {
            let vector = 8 + 4 * values.len();
            let body = 8 + 2 * 24 + (16 + vector).next_multiple_of(8);
            w.event(body as u32, u.object);
            w.u32(0);
            w.u32(u.stream_frame);
            w.property(u.stream_channels, u.int, &3i32.to_ne_bytes());
            w.property(u.stream_position, u.float, &position.to_ne_bytes());
            w.u32(u.stream_values);
            w.u32(0);
            w.u32(vector as u32);
            w.u32(u.vector);
            w.u32(4);
            w.u32(u.float);
            for v in *values {
                w.bytes(&v.to_ne_bytes());
            }
            w.pad();
        }
        w.finish();
        words
    }

    #[test]
    fn the_last_frame_of_a_stream_is_read() {
        let u = urids();
        let words = stream(&u, &[(0.25, &[1.0, 0.0, 1.0]), (0.5, &[0.5, 2.0, -1.0])]);
        let buf: &[u8] = bytemuck::cast_slice(&words);
        let frame = last_frame(buf, &u).expect("a frame");
        assert_eq!((frame.channels, frame.position), (3, 0.5));
        assert_eq!(frame.values().collect::<Vec<_>>(), [0.5, 2.0, -1.0]);
        // No frame: a sequence of MIDI only, an empty one, a cut one
        let words = stream(&u, &[]);
        assert_eq!(last_frame(bytemuck::cast_slice(&words), &u), None);
        let mut empty = [0u8; 64];
        write_input(&mut empty, &u, None, &[]);
        assert_eq!(last_frame(&empty, &u), None);
        let words = stream(&u, &[(0.5, &[1.0; 12])]);
        let buf: &[u8] = bytemuck::cast_slice(&words);
        assert_eq!(last_frame(&buf[..80], &u), None);
        // A size past the buffer stops the read, nothing more
        let mut bad = buf.to_vec();
        bad[0..4].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert_eq!(last_frame(&bad, &u), None);
    }
}
