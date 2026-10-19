use anyhow::Result;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use tracing::{info, warn};

use super::instance::PluginInstance;
use super::scanner::{PluginInfo, PluginScanner};

/// LV2 host: owns the lilv world, the plugin catalogue, and instantiates plugins.
/// `rescan` replaces both after a plugin is installed or removed.
pub struct Lv2Host {
    current: RwLock<Arc<Catalogue>>,
    /// Catalogues replaced by a rescan, kept for the whole process: the
    /// plugins instantiated from their world may still be running, and
    /// lilv frees a plugin's library with its world
    retired: Mutex<Vec<Arc<Catalogue>>>,
    /// LV2 path scanned instead of the default one (`LV2_PATH`), for tests
    path: Option<String>,
}

/// The plugins found by one scan, and the lilv world they come from
pub struct Catalogue {
    scanner: PluginScanner,
    #[cfg(feature = "lv2")]
    world: Option<lilv::World>,
}

impl std::ops::Deref for Catalogue {
    type Target = PluginScanner;

    fn deref(&self) -> &PluginScanner {
        &self.scanner
    }
}

impl Catalogue {
    /// Discover installed LV2 plugins. Falls back to the demo catalogue when
    /// none is usable, so the UI still works on a development machine.
    fn scan(path: Option<&str>) -> Self {
        #[cfg(feature = "lv2")]
        {
            let world = match path {
                Some(path) => world_on(path),
                None => {
                    let world = lilv::World::with_load_all();
                    load_data_bundles(&world);
                    world
                }
            };
            let plugins = discovery::scan(&world);
            if !plugins.is_empty() {
                info!("LV2 host ready with {} plugins", plugins.len());
                return Self {
                    scanner: PluginScanner::from_plugins(
                        plugins.into_iter().chain(super::scanner::builtin_plugins()),
                    ),
                    world: Some(world),
                };
            }
            warn!("No usable LV2 plugin found (check LV2_PATH); using demo plugins");
        }

        #[cfg(not(feature = "lv2"))]
        {
            let _ = path;
            warn!("Built without the `lv2` feature; using demo plugins");
        }

        Self::demo()
    }

    fn demo() -> Self {
        Self {
            scanner: PluginScanner::demo(),
            #[cfg(feature = "lv2")]
            world: None,
        }
    }
}

impl Lv2Host {
    pub fn new() -> Result<Self> {
        info!("Initializing LV2 host");
        Ok(Self::with(Catalogue::scan(None), None))
    }

    /// Host with the demo catalogue only (no LV2 world)
    pub fn demo() -> Self {
        Self::with(Catalogue::demo(), None)
    }

    fn with(catalogue: Catalogue, path: Option<String>) -> Self {
        Self {
            current: RwLock::new(Arc::new(catalogue)),
            retired: Mutex::new(Vec::new()),
            path,
        }
    }

    /// Host with a given catalogue (no LV2 world), for tests
    #[cfg(test)]
    pub(crate) fn with_scanner(scanner: PluginScanner) -> Self {
        Self::with(
            Catalogue {
                scanner,
                #[cfg(feature = "lv2")]
                world: None,
            },
            None,
        )
    }

    /// Scan the LV2 bundles again (after an install or a removal) and
    /// return the number of plugins found. Running instances are left
    /// alone; new blocks use the new scan. Not real-time safe: call it off
    /// the audio thread.
    pub fn rescan(&self) -> usize {
        info!("Rescanning LV2 plugins");
        let fresh = Arc::new(Catalogue::scan(self.path.as_deref()));
        let count = fresh.plugins().len();
        let old = std::mem::replace(&mut *self.current.write().unwrap(), fresh);
        self.retired.lock().unwrap().push(old);
        count
    }

    /// The plugins of the latest scan
    pub fn scanner(&self) -> Arc<Catalogue> {
        self.current.read().unwrap().clone()
    }

    /// True when only demo plugins are available
    pub fn is_demo(&self) -> bool {
        self.scanner().is_demo()
    }

    /// Instantiate and activate a plugin. `file` is the model of blocks
    /// that load one. Not real-time safe: call it off the audio thread.
    pub fn instantiate(
        &self,
        uri: &str,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
        file: Option<&Path>,
    ) -> Result<PluginInstance> {
        self.instantiate_channel(uri, instance_id, sample_rate, max_block, file, None)
    }

    /// Like `instantiate`; impulse response blocks load the given channel
    /// of their file (one side of a stereo response)
    pub fn instantiate_channel(
        &self,
        uri: &str,
        instance_id: usize,
        sample_rate: f32,
        max_block: usize,
        file: Option<&Path>,
        channel: Option<usize>,
    ) -> Result<PluginInstance> {
        let catalogue = self.scanner();
        let info = catalogue
            .find_by_uri(uri)
            .ok_or_else(|| anyhow::anyhow!("Plugin not found: {}", uri))?
            .clone();

        use super::scanner::FileKind;
        match super::scanner::file_block(uri).map(|b| b.kind) {
            Some(FileKind::ImpulseResponse) => {
                return PluginInstance::ir(
                    info,
                    instance_id,
                    sample_rate,
                    max_block,
                    file,
                    channel,
                );
            }
            #[cfg(feature = "nam")]
            Some(FileKind::Neural) => {
                return PluginInstance::nam(info, instance_id, sample_rate, max_block, file);
            }
            _ => {}
        }
        let _ = (file, channel);

        if catalogue.is_demo() {
            return Ok(PluginInstance::passthrough(info, instance_id, max_block));
        }

        #[cfg(feature = "lv2")]
        if let Some(world) = &catalogue.world {
            return PluginInstance::lv2(world, info, instance_id, sample_rate, max_block);
        }

        let _ = sample_rate;
        Err(anyhow::anyhow!("No LV2 world to instantiate {}", uri))
    }
}

impl Default for Lv2Host {
    fn default() -> Self {
        Self::new().expect("Failed to initialize LV2 host")
    }
}

/// Build an LV2 host restricted to the bundles in `path` (used by tests).
/// Unlike the default host, it does not fall back to the demo plugins.
#[cfg(feature = "lv2")]
pub fn host_from_path(path: &str) -> Lv2Host {
    let world = world_on(path);
    let plugins = discovery::scan(&world);
    Lv2Host::with(
        Catalogue {
            scanner: PluginScanner::from_plugins(plugins),
            world: Some(world),
        },
        Some(path.to_string()),
    )
}

/// The LV2 folder of the data directory, where the store installs and the
/// owner may copy bundles by hand: loaded even when `LV2_PATH` (set by the
/// OS image to include it) does not list it, as on a desktop
#[cfg(feature = "lv2")]
fn load_data_bundles(world: &lilv::World) {
    let dir = crate::state::data_dir().join("lv2");
    let separator = if cfg!(windows) { ';' } else { ':' };
    let listed = std::env::var("LV2_PATH").is_ok_and(|path| {
        path.split(separator)
            .any(|p| Path::new(p).canonicalize().ok() == dir.canonicalize().ok())
    });
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    if listed {
        return;
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !path.is_dir() || !name.ends_with(".lv2") || name.starts_with('.') {
            continue;
        }
        let mut bundle = path.display().to_string();
        if !bundle.ends_with(std::path::MAIN_SEPARATOR) {
            bundle.push(std::path::MAIN_SEPARATOR);
        }
        world.load_bundle(&world.new_file_uri(None, &bundle));
    }
}

#[cfg(feature = "lv2")]
fn world_on(path: &str) -> lilv::World {
    let world = lilv::World::new();
    world.set_option(
        "http://drobilla.net/ns/lilv#lv2-path",
        &world.new_string(path),
    );
    world.load_all();
    world
}

#[cfg(feature = "lv2")]
mod discovery {
    use super::super::lv2_features::SUPPORTED_FEATURES;
    use super::super::scanner::{PluginCategory, PluginInfo, PortInfo, PortType};
    use lilv::node::Node;
    use lilv::World;
    use tracing::debug;

    const LV2: &str = "http://lv2plug.in/ns/lv2core#";

    fn number(node: &Node) -> Option<f32> {
        node.as_float().or_else(|| node.as_int().map(|i| i as f32))
    }

    /// Short symbol for the common lv2 units
    fn unit_symbol(world: &World, unit: &Node) -> Option<String> {
        let uri = unit.as_uri()?;
        let known = match uri.rsplit('#').next()? {
            "db" => "dB",
            "hz" => "Hz",
            "khz" => "kHz",
            "ms" => "ms",
            "s" => "s",
            "pc" => "%",
            "bpm" => "BPM",
            "cent" => "ct",
            "semitone12TET" => "st",
            "degree" => "°",
            "oct" => "oct",
            "frame" => "smp",
            _ => "",
        };
        if !known.is_empty() {
            return Some(known.to_string());
        }
        // Custom unit: units:symbol on the unit resource
        let symbol = world.new_uri("http://lv2plug.in/ns/extensions/units#symbol");
        world
            .get(Some(unit), Some(&symbol), None)
            .and_then(|n| n.as_str().map(str::to_string))
    }

    pub fn scan(world: &World) -> Vec<PluginInfo> {
        let uri = |s: &str| world.new_uri(s);
        let audio = uri(&format!("{LV2}AudioPort"));
        let control = uri(&format!("{LV2}ControlPort"));
        let cv = uri(&format!("{LV2}CVPort"));
        let input = uri(&format!("{LV2}InputPort"));
        let atom = uri("http://lv2plug.in/ns/ext/atom#AtomPort");
        let toggled = uri(&format!("{LV2}toggled"));
        let integer = uri(&format!("{LV2}integer"));
        let enumeration = uri(&format!("{LV2}enumeration"));
        let optional = uri(&format!("{LV2}connectionOptional"));
        let logarithmic = uri("http://lv2plug.in/ns/ext/port-props#logarithmic");
        let unit = uri("http://lv2plug.in/ns/extensions/units#unit");
        let designation = uri(&format!("{LV2}designation"));
        let bpm = uri("http://lv2plug.in/ns/ext/time#beatsPerMinute");
        let midi_event = uri(crate::plugins::atom::MIDI_EVENT);
        let position = uri(crate::plugins::atom::TIME_POSITION);

        let mut found = Vec::new();
        'plugins: for plugin in world.plugins().iter() {
            let plugin_uri = plugin.uri().as_uri().unwrap_or_default().to_string();
            if !plugin.verify() {
                debug!("Skipping invalid plugin {}", plugin_uri);
                continue;
            }

            for feature in plugin.required_features().iter() {
                let feature = feature.as_uri().unwrap_or_default();
                if !SUPPORTED_FEATURES.contains(&feature) {
                    debug!(
                        "Skipping {}: needs unsupported feature {}",
                        plugin_uri, feature
                    );
                    continue 'plugins;
                }
            }

            let mut ports = Vec::new();
            for port in plugin.iter_ports() {
                let is_input = port.is_a(&input);
                let port_type = if port.is_a(&audio) {
                    if is_input {
                        PortType::AudioInput
                    } else {
                        PortType::AudioOutput
                    }
                } else if port.is_a(&control) {
                    if is_input {
                        PortType::ControlInput
                    } else {
                        PortType::ControlOutput
                    }
                } else if port.is_a(&atom) {
                    if is_input {
                        PortType::AtomInput
                    } else {
                        PortType::AtomOutput
                    }
                } else if port.is_a(&cv) {
                    if is_input {
                        PortType::CVInput
                    } else {
                        PortType::CVOutput
                    }
                } else if port.has_property(&optional) {
                    continue;
                } else {
                    debug!("Skipping {}: unknown port type", plugin_uri);
                    continue 'plugins;
                };

                let range = port.range();
                let min = range.minimum.as_ref().and_then(number).unwrap_or(0.0);
                let max = range.maximum.as_ref().and_then(number).unwrap_or(1.0);
                let default = range
                    .default
                    .as_ref()
                    .and_then(number)
                    .unwrap_or(min)
                    .clamp(min.min(max), max.max(min));
                let scale_points = port
                    .scale_points()
                    .iter()
                    .filter_map(|sp| Some((sp.label().as_str()?.to_string(), number(&sp.value())?)))
                    .collect();

                let unit_name = port.get(&unit).and_then(|u| unit_symbol(world, &u));
                ports.push(PortInfo {
                    index: port.index(),
                    name: port
                        .name()
                        .and_then(|n| n.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    symbol: port
                        .symbol()
                        .and_then(|n| n.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    port_type,
                    min,
                    max,
                    default,
                    toggled: port.has_property(&toggled),
                    integer: port.has_property(&integer),
                    enumeration: port.has_property(&enumeration),
                    logarithmic: port.has_property(&logarithmic),
                    scale_points,
                    tempo: port_type == PortType::ControlInput
                        && (port.get(&designation).is_some_and(|d| d == bpm)
                            || unit_name.as_deref() == Some("BPM")),
                    unit: unit_name,
                    off_value: None,
                    midi: port_type == PortType::AtomInput && port.supports_event(&midi_event),
                    time: port_type == PortType::AtomInput && port.supports_event(&position),
                });
            }
            ports.sort_by_key(|p| p.index);

            let count = |t: PortType| ports.iter().filter(|p| p.port_type == t).count();
            let (audio_in, audio_out) = (count(PortType::AudioInput), count(PortType::AudioOutput));
            if audio_in == 0 || audio_out == 0 {
                debug!("Skipping {}: not an audio effect", plugin_uri);
                continue;
            }

            let category = plugin
                .class()
                .uri()
                .and_then(|n| n.as_uri().map(PluginCategory::from_lv2_class))
                .unwrap_or(PluginCategory::Unknown);

            found.push(PluginInfo {
                name: plugin
                    .name()
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| plugin_uri.clone()),
                author: plugin
                    .author_name()
                    .and_then(|n| n.as_str().map(str::to_string))
                    .unwrap_or_default(),
                uri: plugin_uri,
                category,
                num_audio_inputs: audio_in,
                num_audio_outputs: audio_out,
                num_control_ports: count(PortType::ControlInput),
                ports,
            });
        }
        found
    }
}

/// Plugin info lookup helper for callers holding only a host reference
pub fn plugin_name(host: &Lv2Host, uri: &str) -> String {
    host.scanner()
        .find_by_uri(uri)
        .map(|info: &PluginInfo| info.name.clone())
        .unwrap_or_else(|| {
            uri.rsplit(['/', '#', ':'])
                .next()
                .unwrap_or(uri)
                .to_string()
        })
}
