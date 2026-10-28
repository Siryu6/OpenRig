//! Views the app draws for a plugin, read from the plugin's own ports: no
//! plugin needs to describe them. The LV2 class says what kind of view fits
//! (an EQ for `ParaEQPlugin`, a level curve for `CompressorPlugin`…); the names, units and properties of the
//! controls say what each one does ("Frequency 1" in Hz, "Gain 1" in dB) and
//! which ones belong together. A plugin read wrongly gets no view, and keeps
//! its knobs.

use super::dynamics::{Dynamics, Kind};
use super::eq_curve::{Band, Display, QUnit, Shape, ShapeChoice, Value};
use super::scanner::{PluginCategory, PluginInfo, PortInfo, PortType};
use super::scope::{Echoes, Level, Scope, ScopeKind};

/// LV2 classes drawn as an EQ
const EQ_CLASSES: [&str; 7] = [
    "EQPlugin",
    "ParaEQPlugin",
    "MultiEQPlugin",
    "FilterPlugin",
    "LowpassPlugin",
    "HighpassPlugin",
    "BandpassPlugin",
];

const FREQ_WORDS: [&str; 5] = ["freq", "frequency", "cutoff", "fc", "hz"];
const GAIN_WORDS: [&str; 3] = ["gain", "boost", "level"];
/// Words for a band's width, and how they are read
const Q_WORDS: [&str; 7] = [
    "q",
    "bandwidth",
    "bw",
    "resonance",
    "res",
    "slope",
    "quality",
];
/// Words of an on/off control, besides the band it turns on
const SWITCH_WORDS: [&str; 7] = [
    "enable", "enabled", "on", "active", "section", "band", "use",
];
/// Words of a filter type list
const TYPE_WORDS: [&str; 4] = ["type", "mode", "filter", "shape"];
/// Words of controls that belong to no band
const OTHER_WORDS: [&str; 12] = [
    "input",
    "output",
    "master",
    "volume",
    "dry",
    "wet",
    "mix",
    "reset",
    "hold",
    "meter",
    "threshold",
    "makeup",
];

/// What a control does in an EQ
#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Freq,
    Gain,
    Q(QWord),
    Switch,
    Type,
}

/// The word naming a band's width, which tells how to read it
#[derive(Debug, Clone, Copy, PartialEq)]
enum QWord {
    Q,
    Bandwidth,
    Resonance,
    Slope,
}

/// Words of a name, lower case: "HPfreq" → hp freq, "Gain1" → gain 1
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let split = match prev {
            None => false,
            Some(p) => {
                !c.is_alphanumeric()
                    || !p.is_alphanumeric()
                    || (p.is_ascii_digit() != c.is_ascii_digit())
                    || (p.is_lowercase() && c.is_uppercase())
            }
        };
        if split && !word.is_empty() {
            out.push(std::mem::take(&mut word).to_lowercase());
        }
        if c.is_alphanumeric() {
            word.push(c);
        }
        prev = Some(c);
    }
    if !word.is_empty() {
        out.push(word.to_lowercase());
    }
    out
}

/// One of the words is one of `names`
fn has_any(words: &[String], names: &[&str]) -> bool {
    words.iter().any(|w| names.contains(&w.as_str()))
}

/// Unit of a port, lower case ("" for none)
fn unit(port: &PortInfo) -> String {
    port.unit.as_deref().unwrap_or("").to_lowercase()
}

/// Shape a band's name gives ("Lowshelf", "Low Cut", "HPF"…)
fn shape_of(words: &[String]) -> Option<Shape> {
    let joined: String = words.concat();
    let has = |names: &[&str]| {
        names
            .iter()
            .any(|n| joined == *n || words.iter().any(|w| w == n))
    };
    if has(&["highpass", "hipass", "hp", "hpf", "lowcut", "locut", "lc"]) {
        Some(Shape::Highpass)
    } else if has(&["lowpass", "lopass", "lp", "lpf", "highcut", "hicut", "hc"]) {
        Some(Shape::Lowpass)
    } else if has(&["lowshelf", "loshelf", "ls", "lsh", "lowshelving"]) {
        Some(Shape::Lowshelf)
    } else if has(&["highshelf", "hishelf", "hs", "hsh", "highshelving"]) {
        Some(Shape::Highshelf)
    } else if has(&["notch", "bandstop", "bandreject"]) {
        Some(Shape::Notch)
    } else if has(&["bandpass", "bp", "bpf"]) {
        Some(Shape::Bandpass)
    } else if has(&["bell", "peak", "peaking", "parametric", "para"]) {
        Some(Shape::Peak)
    } else {
        None
    }
}

/// Shapes of a filter type list, if it is one: each choice is a filter
/// shape or "off"
fn shape_choices(port: &PortInfo) -> Option<Vec<(f32, Option<Shape>)>> {
    if port.scale_points.len() < 2 {
        return None;
    }
    let mut shapes = 0;
    let options: Option<Vec<_>> = port
        .scale_points
        .iter()
        .map(|(label, value)| {
            let w = words(label);
            if has_any(&w, &["off", "none", "bypass"]) {
                return Some((*value, None));
            }
            let shape = shape_of(&w)?;
            shapes += 1;
            Some((*value, Some(shape)))
        })
        .collect();
    options.filter(|_| shapes >= 2)
}

/// Role of a control and the words naming its band
fn role(port: &PortInfo) -> Option<(Role, Vec<String>)> {
    let w = words(&port.name);
    let unit = unit(port);
    let without = |list: &[&str]| -> Vec<String> {
        w.iter()
            .filter(|x| !list.contains(&x.as_str()))
            .cloned()
            .collect()
    };
    if port.enumeration && shape_choices(port).is_some() {
        return Some((Role::Type, without(&TYPE_WORDS)));
    }
    if port.toggled {
        return Some((Role::Switch, without(&SWITCH_WORDS)));
    }
    if has_any(&w, &OTHER_WORDS) {
        return None;
    }
    // A dB control named after its frequency is a graphic EQ band's gain
    if unit == "db" {
        return Some((Role::Gain, without(&GAIN_WORDS)));
    }
    if unit == "hz" || unit == "khz" || has_any(&w, &FREQ_WORDS) {
        return Some((Role::Freq, without(&FREQ_WORDS)));
    }
    if let Some(q) = w.iter().find_map(|x| match x.as_str() {
        "q" | "quality" => Some(QWord::Q),
        "bandwidth" | "bw" => Some(QWord::Bandwidth),
        "resonance" | "res" => Some(QWord::Resonance),
        "slope" => Some(QWord::Slope),
        _ => None,
    }) {
        return Some((Role::Q(q), without(&Q_WORDS)));
    }
    if has_any(&w, &GAIN_WORDS) {
        return Some((Role::Gain, without(&GAIN_WORDS)));
    }
    None
}

/// Controls found for one band
#[derive(Default)]
struct Parts {
    words: Vec<String>,
    /// The frequency control, and Hz per unit of it
    freq: Option<(String, f32)>,
    gain: Option<String>,
    q: Option<(String, QWord)>,
    switch: Option<String>,
    choice: Option<ShapeChoice>,
}

/// Frequency a band's name gives, for a graphic EQ ("125", "1 kHz", "2k")
fn fixed_freq(words: &[String]) -> Option<f32> {
    let number: f32 = words.first()?.parse().ok()?;
    let kilo = words.iter().skip(1).any(|w| w == "k" || w == "khz");
    let rest_ok = words
        .iter()
        .skip(1)
        .all(|w| ["k", "khz", "hz"].contains(&w.as_str()));
    let f = if kilo { number * 1000.0 } else { number };
    (rest_ok && (16.0..=22_000.0).contains(&f)).then_some(f)
}

/// The plugin as an EQ: one band per group of controls sharing a name
pub fn eq(info: &PluginInfo) -> Option<Display> {
    // A plugin without a class falls back on its category
    let unclassed_filter = info.class.is_empty() && info.category == PluginCategory::Filter;
    if !(EQ_CLASSES.contains(&info.class.as_str()) || unclassed_filter) {
        return None;
    }
    let mut parts: Vec<Parts> = Vec::new();
    let (mut enable, mut gain) = (None, None);
    for port in info.control_inputs() {
        let Some((role, key)) = role(port) else {
            continue;
        };
        let symbol = port.symbol.clone();
        if key.is_empty() {
            // A control of the whole plugin
            match role {
                Role::Switch
                    if words(&port.name)
                        .iter()
                        .any(|w| w.starts_with("enable") || w == "on" || w == "active") =>
                {
                    enable = Some(symbol)
                }
                Role::Gain => gain = Some(symbol),
                _ => {}
            }
            continue;
        }
        let i = match parts.iter().position(|p| p.words == key) {
            Some(i) => i,
            None => {
                parts.push(Parts {
                    words: key,
                    ..Default::default()
                });
                parts.len() - 1
            }
        };
        let part = &mut parts[i];
        match role {
            Role::Freq => {
                let scale = if unit(port) == "khz" { 1000.0 } else { 1.0 };
                part.freq = Some((symbol, scale))
            }
            Role::Gain => part.gain = Some(symbol),
            Role::Q(word) => part.q = Some((symbol, word)),
            Role::Switch => part.switch = Some(symbol),
            Role::Type => {
                part.choice = shape_choices(port).map(|options| ShapeChoice {
                    control: symbol,
                    options,
                })
            }
        }
    }

    let bands: Vec<Band> = parts
        .into_iter()
        .filter_map(|p| {
            let (freq, freq_scale) = match (&p.freq, fixed_freq(&p.words)) {
                (Some((symbol, scale)), _) => (Value::Control(symbol.clone()), *scale),
                (None, Some(f)) if p.gain.is_some() => (Value::Fixed(f), 1.0),
                _ => return None,
            };
            let named = shape_of(&p.words);
            let first_choice = p
                .choice
                .as_ref()
                .and_then(|c| c.options.iter().find_map(|o| o.1));
            let shape = named
                .or(first_choice)
                .or(p.gain.is_some().then_some(Shape::Peak))?;
            // Shelves and bells need a gain
            if matches!(shape, Shape::Peak | Shape::Lowshelf | Shape::Highshelf)
                && p.gain.is_none()
                && p.choice.is_none()
            {
                return None;
            }
            let shelf = matches!(shape, Shape::Lowshelf | Shape::Highshelf);
            let (q_unit, q_map) = match p.q.as_ref().map(|q| q.1) {
                Some(QWord::Bandwidth) if shelf => (QUnit::Slope, Vec::new()),
                Some(QWord::Bandwidth) => (QUnit::Octaves, Vec::new()),
                Some(QWord::Slope) => (QUnit::Slope, Vec::new()),
                // A resonance from no bump (0) to a peak at the cutoff (1)
                Some(QWord::Resonance) => (QUnit::Q, vec![(0.0, 0.5), (0.7, 0.707), (1.0, 1.0)]),
                Some(QWord::Q) | None => (QUnit::Q, Vec::new()),
            };
            Some(Band {
                shape,
                shape_choice: p.choice,
                switch: p.switch,
                freq,
                freq_scale,
                gain: p.gain,
                q: p.q.map(|q| Value::Control(q.0)),
                q_unit,
                q_map,
            })
        })
        .collect();
    (!bands.is_empty()).then_some(Display {
        enable,
        gain,
        bands,
    })
}

/// The plugin as a compressor, limiter or gate: its class gives the kind,
/// the names of its controls their part
pub fn dynamics(info: &PluginInfo) -> Option<Dynamics> {
    let ratio_named = info
        .control_inputs()
        .any(|p| has_any(&words(&p.name), &["ratio"]));
    let kind = match info.class.as_str() {
        "CompressorPlugin" => Kind::Compressor,
        "LimiterPlugin" => Kind::Limiter,
        "GatePlugin" | "ExpanderPlugin" => Kind::Gate,
        "DynamicsPlugin" if ratio_named => Kind::Compressor,
        _ => return None,
    };
    let mut d = Dynamics {
        kind,
        threshold: String::new(),
        ratio: None,
        ratio_scale: Vec::new(),
        knee: None,
        makeup: None,
        input_gain: None,
        range: None,
        enable: None,
        reduction: None,
    };
    for port in &info.ports {
        let w = words(&port.name);
        let has = |names: &[&str]| has_any(&w, names);
        let db = unit(port) == "db";
        let symbol = Some(port.symbol.clone());
        match port.port_type {
            PortType::ControlOutput => {
                // Read as dB: not a reading in another unit, or a 0..1 factor
                let factor = port.min >= 0.0 && port.max > 0.0 && port.max <= 1.0;
                let in_db = db || (unit(port).is_empty() && !factor);
                if in_db && has(&["reduction", "gr", "gainr", "attenuation"]) {
                    d.reduction = symbol;
                }
            }
            PortType::ControlInput if port.toggled => {
                if has(&["enable", "enabled", "active"]) && w.len() == 1 {
                    d.enable = symbol;
                }
            }
            PortType::ControlInput => {
                if has(&["threshold", "thresh", "thr", "thres"]) {
                    d.threshold = port.symbol.clone();
                } else if has(&["ratio"]) {
                    d.ratio = symbol;
                    d.ratio_scale = ratio_scale(port);
                } else if has(&["knee"]) {
                    d.knee = symbol;
                } else if has(&["makeup", "make"]) || (db && has(&["output", "post"])) {
                    d.makeup = symbol;
                } else if db && has(&["input", "drive", "pre"]) {
                    d.input_gain = symbol;
                } else if db && has(&["range", "close", "floor", "depth"]) {
                    d.range = symbol;
                } else if db && w == ["gain"] && kind != Kind::Limiter {
                    d.makeup = symbol;
                } else if db && w == ["gain"] {
                    d.input_gain = symbol;
                }
            }
            _ => {}
        }
    }
    let usable = !d.threshold.is_empty() && (kind != Kind::Compressor || d.ratio.is_some());
    usable.then_some(d)
}

/// The view of a delay, reverb, drive, modulation or cabinet block, from its
/// class, else its category (and for simulators, its name: amp or cabinet)
pub fn scope(info: &PluginInfo) -> Option<Scope> {
    let named = |names: &[&str]| has_any(&words(&info.name), names);
    let kind = match info.class.as_str() {
        "DelayPlugin" => ScopeKind::Delay,
        "ReverbPlugin" => ScopeKind::Reverb,
        "DistortionPlugin" | "WaveshaperPlugin" | "AmplifierPlugin" => ScopeKind::Drive,
        "ChorusPlugin" | "FlangerPlugin" | "PhaserPlugin" | "ModulatorPlugin" => {
            ScopeKind::Modulation
        }
        _ => match info.category {
            PluginCategory::Delay => ScopeKind::Delay,
            PluginCategory::Reverb => ScopeKind::Reverb,
            PluginCategory::Distortion => ScopeKind::Drive,
            PluginCategory::Modulation => ScopeKind::Modulation,
            // An amp with its cabinet is drawn as the amp
            PluginCategory::Simulator if named(&["amp", "amplifier", "preamp", "head"]) => {
                ScopeKind::Drive
            }
            PluginCategory::Simulator if named(&["cab", "cabinet", "speaker"]) => {
                ScopeKind::Cabinet
            }
            _ => return None,
        },
    };
    let echoes = (kind == ScopeKind::Delay).then(|| echoes(info)).flatten();
    Some(Scope { kind, echoes })
}

/// A delay's time, feedback and mix, if its time has a unit
fn echoes(info: &PluginInfo) -> Option<Echoes> {
    let mut time = None;
    let (mut feedback, mut mix, mut crossfade, mut sync) = (None, None, false, None);
    for port in info.control_inputs() {
        let w = words(&port.name);
        let has = |names: &[&str]| has_any(&w, names);
        let unit = unit(port);
        let level = || Level {
            symbol: port.symbol.clone(),
            min: port.min,
            max: port.max,
            db: unit == "db",
        };
        if port.toggled {
            if has(&["sync", "bpm", "tempo"]) {
                sync = Some(port.symbol.clone());
            }
        } else if has(&["time", "delay", "length"]) && !has(&["pre", "predelay"]) {
            let seconds = match unit.as_str() {
                "ms" => 0.001,
                "s" => 1.0,
                _ => continue,
            };
            time.get_or_insert((port.symbol.clone(), seconds));
        } else if has(&[
            "feedback",
            "feedb",
            "fb",
            "regen",
            "regeneration",
            "repeats",
        ]) {
            feedback.get_or_insert_with(level);
        } else if has(&["mix", "blend"]) || (has(&["dry"]) && has(&["wet"])) {
            mix = Some(level());
            crossfade = true;
        } else if has(&["wet"]) && mix.is_none() {
            mix = Some(level());
        }
    }
    let (time, seconds) = time?;
    Some(Echoes {
        time,
        seconds,
        feedback,
        mix,
        crossfade,
        sync,
    })
}

/// Named values of a ratio control that are ratios ("1:4", "4:1", "Lim"):
/// (value, 1 / ratio), by value, if there are at least two
fn ratio_scale(port: &PortInfo) -> Vec<(f32, f32)> {
    let mut scale: Vec<(f32, f32)> = port
        .scale_points
        .iter()
        .filter_map(|(label, value)| {
            let label = label.trim().to_lowercase();
            if ["lim", "limit", "limiter", "inf", "∞"].contains(&label.as_str()) {
                return Some((*value, 0.0));
            }
            let (a, b) = label.split_once(':')?;
            let (a, b): (f32, f32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            (a > 0.0 && b > 0.0).then(|| (*value, a.min(b) / a.max(b)))
        })
        .collect();
    scale.sort_by(|a, b| a.0.total_cmp(&b.0));
    if scale.len() < 2 {
        scale.clear();
    }
    scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scanner::PortType;

    fn control(name: &str, symbol: &str, unit: Option<&str>) -> PortInfo {
        PortInfo {
            name: name.into(),
            symbol: symbol.into(),
            port_type: PortType::ControlInput,
            unit: unit.map(str::to_string),
            max: 1.0,
            ..Default::default()
        }
    }

    fn plugin(class: &str, ports: Vec<PortInfo>) -> PluginInfo {
        PluginInfo {
            uri: "urn:test".into(),
            name: "Test".into(),
            author: String::new(),
            category: PluginCategory::Filter,
            class: class.into(),
            num_audio_inputs: 1,
            num_audio_outputs: 1,
            num_control_ports: ports.len(),
            ports,
            file_params: Vec::new(),
        }
    }

    #[test]
    fn words_split_names_and_symbols() {
        assert_eq!(words("HPfreq"), ["hpfreq"]);
        assert_eq!(words("LowPass"), ["low", "pass"]);
        assert_eq!(words("Gain 1"), ["gain", "1"]);
        assert_eq!(words("Gain1"), ["gain", "1"]);
        assert_eq!(words("Lowshelf Frequency"), ["lowshelf", "frequency"]);
        assert_eq!(words("1 kHz"), ["1", "k", "hz"]);
    }

    #[test]
    fn fil4_is_read_as_its_eight_bands() {
        let fil4 = catalog::entries()
            .iter()
            .find(|e| e.id == "x42-fil4")
            .unwrap();
        let eq = eq(&fil4.info).expect("fil4 is an EQ");
        let shapes: Vec<Shape> = eq.bands.iter().map(|b| b.shape).collect();
        assert_eq!(
            shapes,
            [
                Shape::Highpass,
                Shape::Lowpass,
                Shape::Lowshelf,
                Shape::Peak,
                Shape::Peak,
                Shape::Peak,
                Shape::Peak,
                Shape::Highshelf
            ]
        );
        assert_eq!(eq.enable.as_deref(), Some("enable"));
        assert_eq!(eq.gain.as_deref(), Some("gain"));
        let peak = &eq.bands[3];
        assert_eq!(
            (
                peak.freq.control(),
                peak.gain.as_deref(),
                peak.switch.as_deref(),
                peak.q_unit
            ),
            (Some("freq1"), Some("gain1"), Some("sec1"), QUnit::Octaves)
        );
        assert_eq!(eq.bands[2].q_unit, QUnit::Slope);
    }

    #[test]
    fn a_frequency_in_khz_is_read_in_hz() {
        let ports = vec![
            control("Freq", "f", Some("kHz")),
            control("Gain", "g", Some("dB")),
        ];
        let ports = ports
            .into_iter()
            .map(|p| PortInfo {
                name: format!("Peak {}", p.name),
                ..p
            })
            .collect();
        let eq = eq(&plugin("ParaEQPlugin", ports)).unwrap();
        let band = &eq.bands[0];
        assert_eq!(band.freq_scale, 1000.0);
        let state = band.state(&|s: &str| (s == "f").then_some(2.0));
        assert_eq!(state.freq, 2000.0);
    }

    #[test]
    fn graphic_eq_bands_have_fixed_frequencies() {
        let ports = ["63 Hz", "250 Hz", "1 kHz", "4k"]
            .iter()
            .enumerate()
            .map(|(i, n)| control(n, &format!("g{i}"), Some("dB")))
            .collect();
        let eq = eq(&plugin("MultiEQPlugin", ports)).unwrap();
        let freqs: Vec<Value> = eq.bands.iter().map(|b| b.freq.clone()).collect();
        assert_eq!(
            freqs,
            [63.0, 250.0, 1000.0, 4000.0].map(Value::Fixed).to_vec()
        );
    }

    #[test]
    fn a_type_list_gives_the_band_its_shapes() {
        let mut kind = control("Filter type 0", "ft_0", None);
        kind.enumeration = true;
        kind.scale_points = vec![
            ("Off".into(), 0.0),
            ("Bell".into(), 1.0),
            ("Hi-pass".into(), 2.0),
            ("Lo-shelf".into(), 3.0),
        ];
        let ports = vec![
            kind,
            control("Frequency 0", "f_0", Some("Hz")),
            control("Gain 0", "g_0", Some("dB")),
            control("Quality 0", "q_0", None),
        ];
        let eq = eq(&plugin("ParaEQPlugin", ports)).unwrap();
        assert_eq!(eq.bands.len(), 1);
        assert_eq!(eq.bands[0].shape_choice.as_ref().unwrap().options.len(), 4);
    }

    #[test]
    fn other_plugins_get_no_eq() {
        for e in catalog::entries() {
            if e.id != "x42-fil4" {
                assert!(eq(&e.info).is_none(), "{} read as an EQ", e.id);
            }
        }
    }

    #[test]
    fn dynamics_plugins_are_read_as_their_curve() {
        let find = |id: &str| &catalog::entries().iter().find(|e| e.id == id).unwrap().info;
        let comp = dynamics(find("zam-comp")).expect("ZamComp");
        assert_eq!(
            (
                comp.kind,
                comp.threshold.as_str(),
                comp.ratio.as_deref(),
                comp.knee.as_deref(),
                comp.makeup.as_deref()
            ),
            (
                Kind::Compressor,
                "thr",
                Some("rat"),
                Some("kn"),
                Some("mak")
            )
        );
        let gate = dynamics(find("zam-gate")).expect("ZamGate");
        assert_eq!(
            (gate.kind, gate.range.as_deref()),
            (Kind::Gate, Some("close"))
        );
        let dpl = dynamics(find("x42-dpl")).expect("dpl");
        assert_eq!(
            (dpl.kind, dpl.input_gain.as_deref(), dpl.enable.as_deref()),
            (Kind::Limiter, Some("gain"), Some("enable"))
        );
        assert!(dynamics(find("x42-fil4")).is_none());
    }

    #[test]
    fn a_reduction_output_is_found() {
        let mut gr = control("Gain Reduction", "gr", None);
        gr.port_type = PortType::ControlOutput;
        gr.max = 20.0;
        let ports = vec![
            control("Threshold", "thr", Some("dB")),
            control("Ratio", "rat", None),
            gr,
        ];
        let d = dynamics(&plugin("CompressorPlugin", ports.clone())).unwrap();
        assert_eq!(d.reduction.as_deref(), Some("gr"));
        // A 0..1 factor is not a reduction in dB: it is measured instead
        let mut ports = ports;
        ports[2].max = 1.0;
        let d = dynamics(&plugin("CompressorPlugin", ports)).unwrap();
        assert_eq!(d.reduction, None);
    }

    #[test]
    fn a_ratio_named_by_its_values_is_mapped() {
        // x42-darc: a 0 to 1 control, its values named as ratios
        let mut ratio = control("Ratio", "Ratio", None);
        ratio.scale_points = [("Lim", 1.0), ("1:4", 0.75), ("1:2", 0.5), ("soft", 0.1)]
            .map(|(l, v)| (l.to_string(), v))
            .to_vec();
        let ports = vec![
            control("Input Gain", "inputgain", Some("dB")),
            control("Threshold", "threshold", Some("dB")),
            ratio,
        ];
        let d = dynamics(&plugin("CompressorPlugin", ports)).unwrap();
        assert_eq!(d.input_gain.as_deref(), Some("inputgain"));
        assert_eq!(d.ratio_scale, [(0.5, 0.5), (0.75, 0.25), (1.0, 0.0)]);
    }

    #[test]
    fn a_delay_with_a_time_in_ms_gets_its_echoes() {
        let find = |id: &str| &catalog::entries().iter().find(|e| e.id == id).unwrap().info;
        let delay = scope(find("zam-delay")).expect("ZamDelay");
        let echoes = delay.echoes.expect("echoes");
        assert_eq!(
            (
                echoes.time.as_str(),
                echoes.seconds,
                echoes.feedback.map(|f| f.symbol),
                echoes.mix.map(|m| m.symbol),
                echoes.crossfade,
                echoes.sync.as_deref()
            ),
            (
                "time",
                0.001,
                Some("feedb".into()),
                Some("drywet".into()),
                true,
                Some("sync")
            )
        );
        let kinds: Vec<_> = [
            "mverb",
            "gx-tubescreamer",
            "caps-phaser2",
            "caps-cabinet4",
            "caps-ampvts",
        ]
        .iter()
        .map(|id| scope(find(id)).map(|s| s.kind))
        .collect();
        use ScopeKind::*;
        assert_eq!(
            kinds,
            [
                Some(Reverb),
                Some(Drive),
                Some(Modulation),
                Some(Cabinet),
                Some(Drive)
            ]
        );
        assert_eq!(scope(find("x42-fil4")), None);
    }

    #[test]
    fn built_in_blocks_get_their_scope() {
        let kinds: Vec<_> = crate::plugins::scanner::builtin_plugins()
            .iter()
            .map(|p| (p.name.clone(), scope(p).map(|s| s.kind)))
            .collect();
        for (name, kind) in kinds {
            let expected = match name.as_str() {
                "Cabinet" => Some(ScopeKind::Cabinet),
                "Space" => Some(ScopeKind::Reverb),
                "Amp" | "Amp + Cab" | "Pedal" => Some(ScopeKind::Drive),
                _ => None,
            };
            assert_eq!(kind, expected, "{name}");
        }
    }

    /// The view each catalog plugin gets: `cargo test views_of_the_catalog -- --nocapture`
    #[test]
    fn views_of_the_catalog() {
        for e in catalog::entries() {
            let view = match (eq(&e.info), dynamics(&e.info), scope(&e.info)) {
                (Some(d), _, _) => format!("EQ, {} bands", d.bands.len()),
                (_, Some(d), _) => format!("{:?}", d.kind),
                (_, _, Some(s)) if s.echoes.is_some() => format!("{:?}, echoes", s.kind),
                (_, _, Some(s)) => format!("{:?}", s.kind),
                _ => "knobs".into(),
            };
            println!("{:<20} {:<18} {}", e.id, e.info.class, view);
        }
    }

    /// Plugins of the store catalog, as the LV2 scan would read them once
    /// installed: their ports, from a copy of the catalog index
    /// (tests/fixtures/catalog_index.json)
    mod catalog {
        use std::sync::OnceLock;

        use crate::plugins::scanner::{PluginCategory, PluginInfo, PortInfo, PortType};

        #[derive(serde::Deserialize)]
        struct Index {
            packages: Vec<Package>,
        }

        #[derive(serde::Deserialize)]
        struct Package {
            plugins: Vec<Plugin>,
        }

        #[derive(serde::Deserialize)]
        struct Plugin {
            id: String,
            uri: String,
            name: String,
            audio: Audio,
            /// LV2 classes, by local name
            #[serde(default)]
            classes: Vec<String>,
            controls: Vec<Control>,
        }

        #[derive(serde::Deserialize)]
        struct Audio {
            inputs: usize,
            outputs: usize,
        }

        #[derive(serde::Deserialize)]
        struct Control {
            symbol: String,
            name: String,
            min: Option<f32>,
            max: Option<f32>,
            default: Option<f32>,
            unit: Option<String>,
            #[serde(default)]
            properties: Vec<String>,
            #[serde(default)]
            scale_points: Vec<ScalePoint>,
        }

        #[derive(serde::Deserialize)]
        struct ScalePoint {
            value: Option<f32>,
            label: Option<String>,
        }

        /// A catalog plugin: its id in the index, and its ports
        pub struct Entry {
            pub id: String,
            pub info: PluginInfo,
        }

        pub fn entries() -> &'static [Entry] {
            static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
            ENTRIES.get_or_init(|| {
                let index: Index =
                    serde_json::from_str(include_str!("../../tests/fixtures/catalog_index.json"))
                        .expect("catalog fixture");
                index
                    .packages
                    .into_iter()
                    .flat_map(|pkg| pkg.plugins)
                    .map(|p| Entry {
                        info: info(&p),
                        id: p.id,
                    })
                    .collect()
            })
        }

        /// Audio ports first, then the controls, in the plugin's order
        fn info(p: &Plugin) -> PluginInfo {
            let mut ports: Vec<PortInfo> = (0..p.audio.inputs)
                .map(|_| PortType::AudioInput)
                .chain((0..p.audio.outputs).map(|_| PortType::AudioOutput))
                .enumerate()
                .map(|(index, port_type)| PortInfo {
                    index,
                    port_type,
                    max: 1.0,
                    ..Default::default()
                })
                .collect();
            for c in &p.controls {
                let (min, max) = (c.min.unwrap_or(0.0), c.max.unwrap_or(1.0));
                let has = |prop: &str| c.properties.iter().any(|p| p == prop);
                ports.push(PortInfo {
                    index: ports.len(),
                    name: c.name.clone(),
                    symbol: c.symbol.clone(),
                    port_type: PortType::ControlInput,
                    min,
                    max,
                    default: c.default.unwrap_or(min).clamp(min.min(max), max.max(min)),
                    toggled: has("toggled"),
                    integer: has("integer"),
                    enumeration: has("enumeration"),
                    logarithmic: has("logarithmic"),
                    scale_points: c
                        .scale_points
                        .iter()
                        .filter_map(|s| Some((s.label.clone()?, s.value?)))
                        .collect(),
                    unit: c.unit.clone(),
                    ..Default::default()
                });
            }
            let class = p.classes.first().cloned().unwrap_or_default();
            PluginInfo {
                uri: p.uri.clone(),
                name: p.name.clone(),
                author: String::new(),
                // As the LV2 scan reads it
                category: PluginCategory::from_lv2_class(&class),
                class,
                num_audio_inputs: p.audio.inputs,
                num_audio_outputs: p.audio.outputs,
                num_control_ports: p.controls.len(),
                ports,
                file_params: Vec::new(),
            }
        }
    }
}
