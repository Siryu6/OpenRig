//! MIDI input (feature `midi`). Every MIDI source is listened to: USB
//! controllers, the DIN input of an audio interface, a DAW. Devices can be
//! plugged in or out while running; nothing has to be selected. Messages are
//! handed to the UI thread (`midi_ui`), the clock is turned into a tempo
//! here, on each connection's own timestamps. Messages also go straight to
//! `forward` (the LV2 plugins taking MIDI), without the UI thread's delay.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use midir::{Ignore, MidiInput, MidiInputConnection};
use tracing::{info, warn};

use pedalboard::midi::{parse, ClockTracker, MidiMessage};

/// How often devices are looked for. Short: a port that goes and comes back
/// between two scans keeps its id (ALSA "client:port") and would be taken
/// for the old, dead connection; listing ports is cheap
const SCAN: Duration = Duration::from_millis(250);
/// A running clock is reported at least this often, so the UI knows it is
/// still there
const CLOCK_ALIVE: Duration = Duration::from_millis(250);
const CLIENT: &str = "Pedalboard";

/// What the MIDI thread tells the UI thread
#[derive(Debug, Clone)]
pub enum MidiEvent {
    Message(MidiMessage),
    /// The clock is running; with the tempo when it changed
    Clock(Option<f32>),
    /// Names of the devices listened to
    Devices(Vec<String>),
}

/// Stops the MIDI thread when dropped
pub struct MidiIo {
    stop: Arc<AtomicBool>,
}

impl Drop for MidiIo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Clock of one connection
struct Clock {
    tracker: ClockTracker,
    last_sent: Option<Instant>,
}

/// Device name shown to the user: ALSA names ports "Client:Port 20:0"
fn device_name(port_name: &str) -> String {
    port_name
        .split(':')
        .next()
        .unwrap_or(port_name)
        .trim()
        .to_string()
}

/// Ports that are not devices: the ALSA loopback and our own client
fn skipped(port_name: &str) -> bool {
    port_name.starts_with("Midi Through") || port_name.starts_with(CLIENT)
}

/// Listen to every MIDI input; `send` and `forward` run on the MIDI threads
pub fn start(
    send: impl Fn(MidiEvent) + Clone + Send + 'static,
    forward: impl Fn(&[u8]) + Clone + Send + 'static,
) -> Result<MidiIo> {
    // Fails early (no ALSA sequencer…) rather than in the thread
    let scanner = MidiInput::new(CLIENT)?;
    let stop = Arc::new(AtomicBool::new(false));
    let running = stop.clone();
    std::thread::Builder::new()
        .name("midi".into())
        .spawn(move || {
            let mut connections: BTreeMap<String, (String, MidiInputConnection<Clock>)> =
                BTreeMap::new();
            let mut shown: Option<Vec<String>> = None;
            // Ports that failed to open, warned about once (scans every 250 ms)
            let mut failed: std::collections::BTreeSet<String> = Default::default();
            while !running.load(Ordering::Relaxed) {
                let ports: Vec<(String, String)> = scanner
                    .ports()
                    .iter()
                    .filter_map(|p| Some((p.id(), scanner.port_name(p).ok()?)))
                    .filter(|(_, name)| !skipped(name))
                    .collect();
                connections.retain(|id, (name, _)| {
                    let present = ports.iter().any(|(p, _)| p == id);
                    if !present {
                        info!("MIDI device gone: {}", name);
                    }
                    present
                });
                for (id, name) in &ports {
                    if connections.contains_key(id) {
                        continue;
                    }
                    match connect(&scanner, id, send.clone(), forward.clone()) {
                        Ok(connection) => {
                            info!("MIDI input: {}", name);
                            failed.remove(id);
                            connections.insert(id.clone(), (name.clone(), connection));
                        }
                        // Busy or gone: tried again at the next scan
                        Err(e) => {
                            if failed.insert(id.clone()) {
                                warn!("MIDI input {}: {}", name, e);
                            }
                        }
                    }
                }
                let mut names: Vec<String> =
                    connections.values().map(|(n, _)| device_name(n)).collect();
                names.sort();
                names.dedup();
                if shown.as_ref() != Some(&names) {
                    send(MidiEvent::Devices(names.clone()));
                    shown = Some(names);
                }
                std::thread::sleep(SCAN);
            }
        })?;
    Ok(MidiIo { stop })
}

fn connect(
    scanner: &MidiInput,
    id: &str,
    send: impl Fn(MidiEvent) + Send + 'static,
    forward: impl Fn(&[u8]) + Send + 'static,
) -> Result<MidiInputConnection<Clock>> {
    let port = scanner
        .find_port_by_id(id)
        .ok_or_else(|| anyhow::anyhow!("port gone"))?;
    let mut input = MidiInput::new(CLIENT)?;
    // Keep the clock (timing messages)
    input.ignore(Ignore::SysexAndActiveSense);
    let clock = Clock {
        tracker: ClockTracker::default(),
        last_sent: None,
    };
    input
        .connect(
            &port,
            "pedalboard-in",
            move |stamp, bytes, clock| match parse(bytes) {
                // Notes, pitch bend, pressure: for the plugins only
                None => forward(bytes),
                Some(MidiMessage::Clock) => {
                    let bpm = clock.tracker.tick(stamp);
                    let now = Instant::now();
                    let due = clock
                        .last_sent
                        .is_none_or(|t| now.duration_since(t) >= CLOCK_ALIVE);
                    if bpm.is_some() || due {
                        clock.last_sent = Some(now);
                        send(MidiEvent::Clock(bpm));
                    }
                }
                Some(message) => {
                    forward(bytes);
                    send(MidiEvent::Message(message));
                }
            },
            clock,
        )
        .map_err(|e| anyhow::anyhow!("{}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_drop_the_alsa_port() {
        assert_eq!(
            device_name("AudioBox USB 96:AudioBox USB 96 MIDI 1 20:0"),
            "AudioBox USB 96"
        );
        assert_eq!(device_name("nanoKONTROL2"), "nanoKONTROL2");
        assert!(skipped("Midi Through:Midi Through Port-0 14:0"));
    }
}
