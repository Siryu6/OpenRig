use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Port type enumeration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PortType {
    AudioInput,
    AudioOutput,
    #[default]
    ControlInput,
    ControlOutput,
    AtomInput,
    AtomOutput,
    CVInput,
    CVOutput,
}

/// Port information
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortInfo {
    pub index: usize,
    pub name: String,
    pub symbol: String,
    pub port_type: PortType,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// lv2:toggled — on/off switch
    #[serde(default)]
    pub toggled: bool,
    /// lv2:integer — stepped values
    #[serde(default)]
    pub integer: bool,
    /// lv2:enumeration — only the scale points are valid values
    #[serde(default)]
    pub enumeration: bool,
    /// pprops:logarithmic — knob should move on a log scale
    #[serde(default)]
    pub logarithmic: bool,
    /// Named values (label, value), used for enumerations
    #[serde(default)]
    pub scale_points: Vec<(String, f32)>,
    /// Unit symbol ("dB", "ms", "Hz"…)
    #[serde(default)]
    pub unit: Option<String>,
    /// Value shown as "Off" (a filter at the end of its range)
    #[serde(default)]
    pub off_value: Option<f32>,
    /// Follows the global tempo (lv2:designation time:beatsPerMinute, or a
    /// BPM unit)
    #[serde(default)]
    pub tempo: bool,
    /// Atom input taking MIDI events (atom:supports midi:MidiEvent)
    #[serde(default)]
    pub midi: bool,
    /// Atom input taking the transport (atom:supports time:Position)
    #[serde(default)]
    pub time: bool,
}

impl PortInfo {
    fn audio(index: usize, symbol: &str, port_type: PortType) -> Self {
        Self {
            index,
            name: symbol.to_string(),
            symbol: symbol.to_string(),
            port_type,
            max: 1.0,
            ..Default::default()
        }
    }

    fn control(index: usize, name: &str, min: f32, max: f32, default: f32) -> Self {
        Self {
            index,
            name: name.to_string(),
            symbol: name.to_lowercase().replace(' ', "_"),
            port_type: PortType::ControlInput,
            min,
            max,
            default,
            ..Default::default()
        }
    }
}

/// Plugin metadata from LV2 discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub uri: String,
    pub name: String,
    pub author: String,
    pub category: PluginCategory,
    pub num_audio_inputs: usize,
    pub num_audio_outputs: usize,
    pub num_control_ports: usize,
    pub ports: Vec<PortInfo>,
}

impl PluginInfo {
    /// Control input ports, in the order they are exposed as parameters
    pub fn control_inputs(&self) -> impl Iterator<Item = &PortInfo> {
        self.ports
            .iter()
            .filter(|p| p.port_type == PortType::ControlInput)
    }

    /// Number of audio inputs and outputs
    pub fn audio_ports(&self) -> (usize, usize) {
        let count = |t| self.ports.iter().filter(|p| p.port_type == t).count();
        (count(PortType::AudioInput), count(PortType::AudioOutput))
    }

    /// Positions of the parameters that follow the global tempo
    pub fn tempo_parameters(&self) -> Vec<usize> {
        self.control_inputs()
            .enumerate()
            .filter(|(_, p)| p.tempo)
            .map(|(i, _)| i)
            .collect()
    }

    /// Default value of every parameter
    pub fn default_parameters(&self) -> Vec<f32> {
        self.control_inputs().map(|p| p.default).collect()
    }
}

/// Plugin categories (LV2 classes)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PluginCategory {
    Dynamics,   // Compressor, Limiter, Gate
    Distortion, // Overdrive, Fuzz, Distortion
    Filter,     // EQ, Wah, Filter
    Modulation, // Chorus, Flanger, Phaser, Tremolo
    Delay,      // Delay, Echo
    Reverb,     // Reverb, Room, Hall
    Simulator,  // Amp sim, Cabinet sim
    Utility,    // Tuner, Analyzer, Mixer
    Generator,  // Oscillator, Noise
    Unknown,
}

impl PluginCategory {
    /// Parse from LV2 class URI
    pub fn from_lv2_class(class_uri: &str) -> Self {
        match class_uri {
            uri if uri.contains("CompressorPlugin") => Self::Dynamics,
            uri if uri.contains("LimiterPlugin") => Self::Dynamics,
            uri if uri.contains("GatePlugin") => Self::Dynamics,
            uri if uri.contains("ExpanderPlugin") => Self::Dynamics,
            uri if uri.contains("DynamicsPlugin") => Self::Dynamics,
            uri if uri.contains("DistortionPlugin") => Self::Distortion,
            uri if uri.contains("WaveshaperPlugin") => Self::Distortion,
            uri if uri.contains("EQPlugin") => Self::Filter,
            uri if uri.contains("FilterPlugin") => Self::Filter,
            uri if uri.contains("ChorusPlugin") => Self::Modulation,
            uri if uri.contains("FlangerPlugin") => Self::Modulation,
            uri if uri.contains("PhaserPlugin") => Self::Modulation,
            uri if uri.contains("ModulatorPlugin") => Self::Modulation,
            uri if uri.contains("DelayPlugin") => Self::Delay,
            uri if uri.contains("ReverbPlugin") => Self::Reverb,
            uri if uri.contains("SimulatorPlugin") => Self::Simulator,
            uri if uri.contains("AmplifierPlugin") => Self::Simulator,
            uri if uri.contains("UtilityPlugin") => Self::Utility,
            uri if uri.contains("AnalyserPlugin") => Self::Utility,
            uri if uri.contains("MixerPlugin") => Self::Utility,
            uri if uri.contains("GeneratorPlugin") => Self::Generator,
            uri if uri.contains("OscillatorPlugin") => Self::Generator,
            _ => Self::Unknown,
        }
    }
}

/// Catalogue of the plugins the host can load
pub struct PluginScanner {
    plugins: HashMap<String, PluginInfo>,
    /// True when no real LV2 plugin was found and the demo catalogue is used
    demo: bool,
}

impl PluginScanner {
    /// Build a catalogue from discovered plugins
    pub fn from_plugins(plugins: impl IntoIterator<Item = PluginInfo>) -> Self {
        Self {
            plugins: plugins.into_iter().map(|p| (p.uri.clone(), p)).collect(),
            demo: false,
        }
    }

    /// Demo catalogue used when no LV2 plugin is installed. Demo plugins pass
    /// audio through unchanged; they let the UI be developed without plugins.
    pub fn demo() -> Self {
        let mut scanner = Self::from_plugins(demo_plugins().into_iter().chain(builtin_plugins()));
        scanner.demo = true;
        scanner
    }

    /// True when the catalogue holds demo plugins only
    pub fn is_demo(&self) -> bool {
        self.demo
    }

    /// Get all discovered plugins
    pub fn plugins(&self) -> &HashMap<String, PluginInfo> {
        &self.plugins
    }

    /// Get plugins by category
    pub fn plugins_by_category(&self, category: PluginCategory) -> Vec<&PluginInfo> {
        self.plugins
            .values()
            .filter(|p| p.category == category)
            .collect()
    }

    /// Find plugin by URI
    pub fn find_by_uri(&self, uri: &str) -> Option<&PluginInfo> {
        self.plugins.get(uri)
    }
}

/// URI of the built-in amp block (NAM). Kept from when it was the only
/// neural block, so older presets still load.
pub const NAM_URI: &str = "urn:pedalboard:nam";

/// URI of the built-in amp + cabinet block (NAM capture of a combo or of
/// an amp with its cabinet and mic: no cabinet IR needed after it)
pub const NAM_AMP_CAB_URI: &str = "urn:pedalboard:nam-amp-cab";

/// URI of the built-in pedal / outboard block (NAM)
pub const NAM_PEDAL_URI: &str = "urn:pedalboard:nam-pedal";

/// URI of the built-in cabinet block (impulse response)
pub const IR_URI: &str = "urn:pedalboard:ir";

/// URI of the built-in space block (room / reverb impulse response)
pub const IR_SPACE_URI: &str = "urn:pedalboard:ir-space";

/// Engine of a block that loads a file
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Neural model (NAM, RTNeural)
    Neural,
    /// Impulse response (WAV)
    ImpulseResponse,
}

impl FileKind {
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            FileKind::Neural => &["nam", "json", "aidax"],
            FileKind::ImpulseResponse => &["wav"],
        }
    }
}

/// A built-in block that loads a model or impulse response file
#[derive(Debug)]
pub struct FileBlock {
    pub uri: &'static str,
    pub name: &'static str,
    pub kind: FileKind,
    /// TONE3000 gear types offered for this block
    pub gears: &'static [&'static str],
    /// What the file is called in the editor
    pub file_label: &'static str,
    /// Bass / middle / treble controls (amp blocks)
    pub tone_stack: bool,
    /// How the rack draws it: "amp", "combo", "cab", "pedal", "space"
    pub shape: &'static str,
    /// What to load in it, shown in the plugin browser
    pub hint: &'static str,
}

pub const FILE_BLOCKS: [FileBlock; 5] = [
    FileBlock {
        uri: NAM_URI,
        name: "Amp",
        kind: FileKind::Neural,
        gears: &["amp"],
        file_label: "Amp",
        tone_stack: true,
        shape: "amp",
        hint: "NAM · amp head only, add a cabinet after it",
    },
    FileBlock {
        uri: NAM_AMP_CAB_URI,
        name: "Amp + Cab",
        kind: FileKind::Neural,
        gears: &["amp-cab"],
        file_label: "Amp + cab",
        tone_stack: true,
        shape: "combo",
        hint: "NAM · full rig capture, no cabinet needed",
    },
    FileBlock {
        uri: NAM_PEDAL_URI,
        name: "Pedal",
        kind: FileKind::Neural,
        gears: &["pedal", "outboard", "experimental"],
        file_label: "Pedal",
        tone_stack: false,
        shape: "pedal",
        hint: "NAM · pedal, preamp or outboard capture",
    },
    FileBlock {
        uri: IR_URI,
        name: "Cabinet",
        kind: FileKind::ImpulseResponse,
        gears: &["cab"],
        file_label: "Cabinet",
        tone_stack: false,
        shape: "cab",
        hint: "IR · speaker cabinet, after an amp head",
    },
    FileBlock {
        uri: IR_SPACE_URI,
        name: "Space",
        kind: FileKind::ImpulseResponse,
        gears: &["space", "experimental"],
        file_label: "Space",
        tone_stack: false,
        shape: "space",
        hint: "IR · room or reverb, blended with the dry signal",
    },
];

/// The file block `uri`, if it is one
pub fn file_block(uri: &str) -> Option<&'static FileBlock> {
    FILE_BLOCKS.iter().find(|b| b.uri == uri)
}

/// Extensions of the files a block loads; empty for blocks without a file
pub fn file_extensions(uri: &str) -> &'static [&'static str] {
    file_block(uri).map_or(&[], |b| b.kind.extensions())
}

/// True for blocks that load a file chosen in the editor
pub fn uses_file(uri: &str) -> bool {
    file_block(uri).is_some()
}

/// Last control of the neural blocks: full model or a lighter size, for
/// model files that hold several (see `nam::file_has_quality`)
pub const NAM_QUALITY: &str = "Quality";

/// Metadata kept next to downloaded models, never a model itself
pub const TONE_INFO_FILE: &str = "tone.json";

/// True if `path` is a file the block `uri` can load
pub fn accepts_file(uri: &str, path: &std::path::Path) -> bool {
    path.file_name().is_some_and(|n| n != TONE_INFO_FILE)
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| file_extensions(uri).contains(&e.to_ascii_lowercase().as_str()))
}

/// Blocks implemented by the pedalboard itself, available with or without
/// LV2 plugins
pub fn builtin_plugins() -> Vec<PluginInfo> {
    use super::dsp::Cuts;
    let mut plugins = Vec::new();
    for block in &FILE_BLOCKS {
        let plugin = match block.kind {
            FileKind::ImpulseResponse => {
                // Rooms are blended in, cabinets replace the signal
                let mix = if block.uri == IR_SPACE_URI {
                    35.0
                } else {
                    100.0
                };
                let mut ir = demo_plugin(
                    block.uri,
                    block.name,
                    "Impulse response",
                    if block.uri == IR_SPACE_URI {
                        PluginCategory::Reverb
                    } else {
                        PluginCategory::Simulator
                    },
                    false,
                    &[
                        ("Level", -24.0, 24.0, 0.0),
                        ("Mix", 0.0, 100.0, mix),
                        ("Normalize", 0.0, 1.0, 1.0),
                        ("Low cut", Cuts::LOW_OFF, 500.0, Cuts::LOW_OFF),
                        ("High cut", 1000.0, Cuts::HIGH_OFF, Cuts::HIGH_OFF),
                    ],
                );
                ir.ports[2].unit = Some("dB".to_string());
                ir.ports[3].unit = Some("%".to_string());
                ir.ports[4].toggled = true;
                ir.ports[5].unit = Some("Hz".to_string());
                ir.ports[6].unit = Some("Hz".to_string());
                ir.ports[5].off_value = Some(Cuts::LOW_OFF);
                ir.ports[6].off_value = Some(Cuts::HIGH_OFF);
                ir
            }
            #[cfg(feature = "nam")]
            FileKind::Neural => {
                use super::nam::{QUALITY_FULL, QUALITY_LIGHT};
                let mut controls = vec![
                    ("Input", -24.0, 24.0, 0.0),
                    ("Output", -24.0, 24.0, 0.0),
                    ("Normalize", 0.0, 1.0, 1.0),
                    // Off by default: one gate per chain is usually enough
                    ("Gate", 0.0, 1.0, 0.0),
                    ("Threshold", -100.0, 0.0, -80.0),
                ];
                if block.tone_stack {
                    let flat = super::dsp::ToneStack::FLAT;
                    controls.extend([
                        ("Bass", 0.0, 10.0, flat),
                        ("Middle", 0.0, 10.0, flat),
                        ("Treble", 0.0, 10.0, flat),
                    ]);
                }
                controls.push((NAM_QUALITY, QUALITY_LIGHT, QUALITY_FULL, QUALITY_FULL));
                let mut nam = demo_plugin(
                    block.uri,
                    block.name,
                    "Neural Amp Modeler",
                    if block.uri == NAM_PEDAL_URI {
                        PluginCategory::Distortion
                    } else {
                        PluginCategory::Simulator
                    },
                    false,
                    &controls,
                );
                for i in [2, 3, 6] {
                    nam.ports[i].unit = Some("dB".to_string());
                }
                nam.ports[4].toggled = true;
                nam.ports[5].toggled = true;
                let quality = nam.ports.last_mut().expect("quality port");
                quality.enumeration = true;
                quality.scale_points = vec![
                    ("Full".to_string(), QUALITY_FULL),
                    ("Light".to_string(), QUALITY_LIGHT),
                ];
                nam
            }
            #[cfg(not(feature = "nam"))]
            FileKind::Neural => continue,
        };
        plugins.push(plugin);
    }
    plugins
}

fn demo_plugin(
    uri: &str,
    name: &str,
    author: &str,
    category: PluginCategory,
    stereo: bool,
    controls: &[(&str, f32, f32, f32)],
) -> PluginInfo {
    let mut ports = Vec::new();
    if stereo {
        ports.push(PortInfo::audio(0, "in_l", PortType::AudioInput));
        ports.push(PortInfo::audio(1, "in_r", PortType::AudioInput));
        ports.push(PortInfo::audio(2, "out_l", PortType::AudioOutput));
        ports.push(PortInfo::audio(3, "out_r", PortType::AudioOutput));
    } else {
        ports.push(PortInfo::audio(0, "in", PortType::AudioInput));
        ports.push(PortInfo::audio(1, "out", PortType::AudioOutput));
    }
    let channels = if stereo { 2 } else { 1 };
    for &(control, min, max, default) in controls {
        ports.push(PortInfo::control(ports.len(), control, min, max, default));
    }

    PluginInfo {
        uri: uri.to_string(),
        name: name.to_string(),
        author: author.to_string(),
        category,
        num_audio_inputs: channels,
        num_audio_outputs: channels,
        num_control_ports: controls.len(),
        ports,
    }
}

fn demo_plugins() -> Vec<PluginInfo> {
    let mut overdrive = demo_plugin(
        "urn:pedalboard:demo:overdrive",
        "Demo Overdrive",
        "Pedalboard",
        PluginCategory::Distortion,
        false,
        &[
            ("Drive", 0.0, 1.0, 0.5),
            ("Tone", 0.0, 1.0, 0.5),
            ("Level", -20.0, 20.0, 0.0),
        ],
    );
    // A toggle and an enumeration so the editor exercises every control kind
    let mut mode = PortInfo::control(overdrive.ports.len(), "Mode", 0.0, 2.0, 0.0);
    mode.integer = true;
    mode.enumeration = true;
    mode.scale_points = vec![
        ("Classic".to_string(), 0.0),
        ("Modern".to_string(), 1.0),
        ("Bright".to_string(), 2.0),
    ];
    overdrive.ports.push(mode);
    let mut boost = PortInfo::control(overdrive.ports.len(), "Boost", 0.0, 1.0, 0.0);
    boost.toggled = true;
    overdrive.ports.push(boost);
    overdrive.num_control_ports += 2;

    vec![
        demo_plugin(
            "urn:pedalboard:demo:compressor",
            "Demo Compressor",
            "Pedalboard",
            PluginCategory::Dynamics,
            true,
            &[
                ("Threshold", -60.0, 0.0, -20.0),
                ("Ratio", 1.0, 20.0, 4.0),
                ("Attack", 0.1, 100.0, 10.0),
                ("Release", 10.0, 1000.0, 100.0),
            ],
        ),
        overdrive,
        demo_plugin(
            "urn:pedalboard:demo:amp",
            "Demo Amp",
            "Pedalboard",
            PluginCategory::Simulator,
            true,
            &[
                ("Gain", 0.0, 10.0, 5.0),
                ("Bass", 0.0, 10.0, 5.0),
                ("Middle", 0.0, 10.0, 5.0),
                ("Treble", 0.0, 10.0, 5.0),
                ("Master", -40.0, 6.0, -6.0),
            ],
        ),
        demo_plugin(
            "urn:pedalboard:demo:chorus",
            "Demo Chorus",
            "Pedalboard",
            PluginCategory::Modulation,
            true,
            &[
                ("Rate", 0.1, 10.0, 1.0),
                ("Depth", 0.0, 1.0, 0.5),
                ("Mix", 0.0, 1.0, 0.5),
            ],
        ),
        demo_plugin(
            "urn:pedalboard:demo:delay",
            "Demo Delay",
            "Pedalboard",
            PluginCategory::Delay,
            true,
            &[
                ("Time", 1.0, 2000.0, 400.0),
                ("Feedback", 0.0, 1.0, 0.3),
                ("Mix", 0.0, 1.0, 0.3),
            ],
        ),
        demo_plugin(
            "urn:pedalboard:demo:reverb",
            "Demo Reverb",
            "Pedalboard",
            PluginCategory::Reverb,
            true,
            &[
                ("Room Size", 0.0, 1.0, 0.5),
                ("Damping", 0.0, 1.0, 0.5),
                ("Mix", 0.0, 1.0, 0.25),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_block_files() {
        use std::path::Path;
        assert!(accepts_file(NAM_URI, Path::new("amp.NAM")));
        assert!(accepts_file(NAM_URI, Path::new("dir/amp.aidax")));
        assert!(!accepts_file(NAM_URI, Path::new("cab.wav")));
        assert!(accepts_file(IR_URI, Path::new("cab.WAV")));
        assert!(!accepts_file(
            NAM_URI,
            Path::new("Tone3000/Amps/x/tone.json")
        ));
        assert!(accepts_file(NAM_PEDAL_URI, Path::new("drive.nam")));
        assert!(accepts_file(NAM_AMP_CAB_URI, Path::new("combo.nam")));
        assert!(accepts_file(IR_SPACE_URI, Path::new("hall.wav")));
        assert!(!uses_file("urn:pedalboard:demo:delay"));
    }

    #[test]
    fn test_demo_catalogue() {
        let scanner = PluginScanner::demo();
        assert!(scanner.is_demo());
        assert!(!scanner.plugins().is_empty());
    }

    #[test]
    fn test_category_filtering() {
        let scanner = PluginScanner::demo();
        assert!(!scanner
            .plugins_by_category(PluginCategory::Distortion)
            .is_empty());
    }

    #[test]
    fn test_port_information() {
        let scanner = PluginScanner::demo();

        for plugin in scanner.plugins().values() {
            let count = |t: PortType| plugin.ports.iter().filter(|p| p.port_type == t).count();
            assert_eq!(count(PortType::AudioInput), plugin.num_audio_inputs);
            assert_eq!(count(PortType::AudioOutput), plugin.num_audio_outputs);
            assert_eq!(count(PortType::ControlInput), plugin.num_control_ports);
            // Port indices must be contiguous: hosts connect every index
            for (i, port) in plugin.ports.iter().enumerate() {
                assert_eq!(port.index, i);
            }
        }
    }
}
