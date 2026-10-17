#[cfg(feature = "gpio")]
mod gpio;
mod hardware_ui;
mod live_ui;
#[cfg(feature = "midi")]
mod midi_io;
mod midi_ui;
#[cfg(feature = "tone3000")]
mod tone3000_ui;
#[cfg(feature = "update")]
mod update_ui;
mod wifi_ui;

use anyhow::Result;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::Cell;
use std::io::IsTerminal;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use pedalboard::audio::looper::LooperState as EngineLooperState;
use pedalboard::audio::{jack_client, AudioEngine, ChainNode, SplitType};
use pedalboard::midi::MidiTarget;
use pedalboard::monitor::{self, Level, SystemMonitor};
use pedalboard::plugins::scanner;
use pedalboard::state::{AppState, EffectChainState, ModelPack};
use pedalboard::ui::slint_bridge as views;
use pedalboard::ui::{chain_layout, text_edit};

slint::include_modules!();

/// UI refresh / polling period
const POLL_MS: u64 = 50;
/// How long a status message stays visible
const STATUS_SECONDS: f32 = 4.0;
/// View indexes of the block editor, the tuner and the Live page
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
const EDITOR_VIEW: i32 = 1;
const TUNER_VIEW: i32 = 3;
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
const LIVE_VIEW: i32 = 9;
/// The tuner shows the last note this long after the string dies out
const TUNER_HOLD: Duration = Duration::from_millis(600);
/// Resource monitor refresh period
const MONITOR_MS: u64 = 1000;
/// How often the JACK server is checked when audio is off
const AUDIO_WATCH_MS: u64 = 3000;
/// Set by the OS image, where a supervisor restarts the app: quit when the
/// JACK server stops or starts late (audio interface unplugged / plugged), so
/// the restarted app runs on the new server
const EXIT_ON_AUDIO_CHANGE: &str = "PEDALBOARD_EXIT_ON_AUDIO_CHANGE";
/// Set by the OS image: a file the app writes `poweroff` or `reboot` to
/// before quitting, for the supervisor to switch the board off or restart it
const POWER_REQUEST: &str = "PEDALBOARD_POWER_REQUEST";
/// Set by the OS image: a file the app writes again every second while its
/// event loop runs, for the supervisor to tell a running app from a hung one
/// (an app update on trial is kept only if it does)
const ALIVE_FILE: &str = "PEDALBOARD_ALIVE_FILE";

pub(crate) fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn id(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

/// Split type from its UI name
fn split_type(kind: &str) -> Result<SplitType> {
    match kind {
        "ab" => Ok(SplitType::Ab),
        "mix" => Ok(SplitType::Mix),
        "lr" => Ok(SplitType::Lr),
        other => Err(anyhow::anyhow!("Unknown split type: {}", other)),
    }
}

/// Show an error in the status toast (and the log)
pub(crate) fn report<T>(app: &App, result: Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            warn!("{}", e);
            show_status(app, e.to_string().into());
            None
        }
    }
}

thread_local! {
    /// Counts toasts, so the same text shown twice restarts the timeout
    static STATUS_SHOWN: Cell<u32> = const { Cell::new(0) };
}

/// Show a toast for `STATUS_SECONDS`
pub(crate) fn show_status(app: &App, message: SharedString) {
    STATUS_SHOWN.with(|n| n.set(n.get().wrapping_add(1)));
    app.set_status_message(message);
}

pub(crate) fn refresh_chain(app: &App, state: &AppState) {
    let chain = state.chain();
    let chain_layout::ChainLayout {
        nodes,
        connectors,
        width,
        min_y,
        max_y,
        ..
    } = chain_layout::layout_chain_with(&chain.nodes, &state.signal_map(&chain));

    let nodes: Vec<PositionedNode> = nodes
        .into_iter()
        .map(|n| PositionedNode {
            instance_id: n.instance_id as i32,
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
            name: if n.is_split {
                n.name
            } else {
                block_name(state, &chain, n.instance_id, &n.uri)
            }
            .into(),
            is_split: n.is_split,
            split_type: n.split_type.into(),
            bypassed: n.bypassed,
            ab_select_a: n.ab_select_a,
            mix_ratio: n.mix_ratio,
            pan_a: n.pan[0],
            pan_b: n.pan[1],
            kind: scanner::file_block(&n.uri).map_or("", |b| b.shape).into(),
            tint: rgb(block_tint(state, &chain, n.instance_id, &n.uri)),
            tinted: chain
                .colors
                .get(&n.instance_id)
                .is_some_and(|c| views::block_color(c).is_some()),
            t3k: !n.is_split
                && chain
                    .files
                    .get(&n.instance_id)
                    .and_then(|f| state.model_pack(&n.uri, f))
                    .is_some_and(|p| p.tone3000),
        })
        .collect();
    let connectors: Vec<Connector> = connectors
        .into_iter()
        .map(|c| Connector {
            x1: c.x1,
            y1: c.y1,
            x2: c.x2,
            y2: c.y2,
            is_stereo: c.is_stereo,
            is_mono: c.is_mono,
            wide: c.wide,
        })
        .collect();

    app.set_positioned_nodes(model(nodes));
    app.set_connectors(model(connectors));
    app.set_chain_width(width);
    app.set_chain_top(min_y);
    app.set_chain_bottom(max_y);
    app.set_stereo_mode(chain.stereo_mode);
    refresh_editor(app, state, &chain);
    // Scenes and stomp switches follow the chain
    live_ui::refresh(app, state);
}

/// Name of a block: blocks loading a file are named after its tone
/// ("Boss MT-2")
/// Picture saved with a downloaded tone (none without TONE3000)
fn pack_picture(pack: Option<&ModelPack>) -> slint::Image {
    #[cfg(feature = "tone3000")]
    if let Some(path) = pack.and_then(|p| p.picture.as_deref()) {
        return tone3000_ui::local_picture(path);
    }
    let _ = pack;
    slint::Image::default()
}

pub(crate) fn block_name(
    state: &AppState,
    chain: &EffectChainState,
    instance_id: usize,
    uri: &str,
) -> String {
    match chain.files.get(&instance_id) {
        Some(file) => state
            .model_pack(uri, file)
            .map_or_else(|| ModelPack::preset_name(file), |p| p.short_title()),
        None => state.plugin_name(uri),
    }
}

/// Colour of a block: the user's, else the colour of its family
pub(crate) fn block_tint(
    state: &AppState,
    chain: &EffectChainState,
    instance_id: usize,
    uri: &str,
) -> u32 {
    chain
        .colors
        .get(&instance_id)
        .and_then(|c| views::block_color(c))
        .unwrap_or_else(|| {
            views::default_block_color(
                scanner::file_block(uri).map_or("", |b| b.shape),
                state.plugin_category(uri).as_ref(),
            )
        })
}

/// URI of the plugin `instance_id`, if it is one
pub(crate) fn block_uri(chain: &EffectChainState, instance_id: usize) -> Option<String> {
    match chain.find(instance_id)? {
        ChainNode::Plugin { uri, .. } => Some(uri.clone()),
        ChainNode::Split { .. } => None,
    }
}

pub(crate) fn rgb(value: u32) -> slint::Color {
    slint::Color::from_rgb_u8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// Drop slots of the current chain for the dragged block
fn drop_slots(state: &AppState, dragged: usize) -> Vec<chain_layout::DropSlot> {
    let chain = state.chain();
    chain_layout::drop_slots(&chain_layout::layout_chain(&chain.nodes), &chain, dragged)
}

/// Fill the editor with the selected node of `chain` (a snapshot of the
/// state's chain), or clear the selection if the node is gone
fn refresh_editor(app: &App, state: &AppState, chain: &EffectChainState) {
    let selected =
        id(app.get_selected_instance_id()).and_then(|i| views::selected_node_view(state, i));
    let Some(view) = selected else {
        app.set_selected_instance_id(-1);
        app.set_selected_parameters(model(Vec::new()));
        if app.get_current_view() == 1 {
            app.set_current_view(0);
        }
        return;
    };
    let parameters: Vec<ParameterInfo> = view
        .parameters
        .into_iter()
        .map(|p| ParameterInfo {
            index: p.index as i32,
            name: p.name.into(),
            min: p.min,
            max: p.max,
            default: p.default,
            value: p.value,
            unit: p.unit.into(),
            kind: p.kind.as_str().into(),
            options: model(p.options.into_iter().map(SharedString::from).collect()),
            option_values: model(p.option_values),
            has_off: p.off_value.is_some(),
            off_value: p.off_value.unwrap_or(0.0),
            midi: midi_label(chain, view.instance_id, MidiTarget::Parameter(p.index)),
        })
        .collect();
    let color = chain
        .colors
        .get(&view.instance_id)
        .cloned()
        .unwrap_or_default();
    app.set_selected_auto_color(rgb(view.auto_color));
    app.set_selected_color_value(rgb(views::block_color(&color).unwrap_or(view.auto_color)));
    app.set_selected_color(color.into());
    app.set_selected_plugin_name(view.name.into());
    app.set_selected_plugin_category(view.category.into());
    app.set_selected_plugin_bypassed(view.bypassed);
    app.set_selected_bypass_midi(midi_label(chain, view.instance_id, MidiTarget::Bypass));
    app.set_selected_parameters(model(parameters));
    app.set_selected_uses_file(view.uses_file);
    let pack = match chain.find(view.instance_id) {
        Some(ChainNode::Plugin { uri, .. }) if !view.file.is_empty() => {
            state.model_pack(uri, &view.file)
        }
        _ => None,
    };
    let labels = pack
        .as_ref()
        .map(ModelPack::preset_labels)
        .unwrap_or_default();
    let presets: Vec<ModelChoice> = pack
        .iter()
        .flat_map(|p| &p.files)
        .zip(&labels)
        .map(|(f, label)| ModelChoice {
            title: label.as_str().into(),
            subtitle: SharedString::new(),
            image: Default::default(),
            t3k: false,
            file: f.as_str().into(),
            current: *f == view.file,
        })
        .collect();
    let preset_name = presets.iter().find(|p| p.current).map_or_else(
        || ModelPack::preset_name(&view.file).into(),
        |p| p.title.clone(),
    );
    app.set_selected_file_title(
        pack.as_ref()
            .map(|p| p.title.clone())
            .unwrap_or_default()
            .into(),
    );
    app.set_selected_file_subtitle(
        pack.as_ref()
            .and_then(|p| p.creator.as_ref())
            .map(|c| format!("by @{c}"))
            .unwrap_or_default()
            .into(),
    );
    app.set_selected_file_t3k(pack.as_ref().is_some_and(|p| p.tone3000));
    app.set_selected_file_image(pack_picture(pack.as_ref()));
    app.set_selected_preset_name(preset_name);
    app.set_selected_presets(model(presets));
    app.set_selected_file(view.file.into());
    app.set_selected_file_label(view.file_label.into());
    app.set_selected_file_types(view.file_types.into());
}

/// "CC n" when a controller drives this target of the block
fn midi_label(chain: &EffectChainState, instance_id: usize, target: MidiTarget) -> SharedString {
    chain
        .midi_controller(instance_id, target)
        .map(|cc| format!("CC {cc}").into())
        .unwrap_or_default()
}

/// Show a bypass change in place (a footswitch may be pressed while a knob
/// of the editor is being dragged)
pub(crate) fn show_bypass(app: &App, instance_id: i32, bypassed: bool) {
    update_node(app, instance_id, |n| n.bypassed = bypassed);
    if app.get_selected_instance_id() == instance_id {
        app.set_selected_plugin_bypassed(bypassed);
    }
}

/// Update one rack node in place (keeps sliders alive while dragging)
/// The rack node of `instance_id`
fn node(app: &App, instance_id: i32) -> Option<PositionedNode> {
    app.get_positioned_nodes()
        .iter()
        .find(|n| n.instance_id == instance_id)
}

fn update_node(app: &App, instance_id: i32, f: impl FnOnce(&mut PositionedNode)) {
    let nodes = app.get_positioned_nodes();
    for row in 0..nodes.row_count() {
        if let Some(mut node) = nodes.row_data(row) {
            if node.instance_id == instance_id {
                f(&mut node);
                nodes.set_row_data(row, node);
                return;
            }
        }
    }
}

pub(crate) fn refresh_presets(app: &App, state: &AppState) {
    let presets: Vec<PresetInfo> = views::preset_views(state)
        .into_iter()
        .map(|p| PresetInfo {
            id: p.id.into(),
            name: p.name.into(),
            description: p.description.into(),
            plugin_count: p.plugin_count as i32,
            modified_date: p.modified.into(),
        })
        .collect();
    app.set_presets(model(presets));
    app.set_current_preset_name(state.current_preset_name().into());
    app.set_current_preset_id(state.current_preset_id().unwrap_or_default().into());
}

pub(crate) fn refresh_scenes(app: &App, state: &AppState) {
    let scenes: Vec<SceneInfo> = views::scene_views(state)
        .into_iter()
        .map(|s| SceneInfo {
            id: s.id.into(),
            name: s.name.into(),
            description: s.description.into(),
            preset_count: s.preset_count as i32,
        })
        .collect();
    let slots: Vec<SceneSlotInfo> = views::scene_slot_views(state)
        .into_iter()
        .map(|s| SceneSlotInfo {
            slot_number: s.slot as i32,
            preset_id: s.preset_id.into(),
            preset_name: s.preset_name.into(),
            is_empty: s.is_empty,
            active: s.active,
        })
        .collect();
    let current = state
        .current_scene_id()
        .and_then(|id| state.list_scenes().into_iter().find(|s| s.id == id))
        .map(|s| s.name)
        .unwrap_or_else(|| "No setlist".to_string());
    app.set_scenes(model(scenes));
    app.set_scene_slots(model(slots));
    app.set_current_scene_name(current.into());
    live_ui::refresh(app, state);
}

fn refresh_all(app: &App, state: &AppState) {
    refresh_chain(app, state);
    refresh_presets(app, state);
    refresh_scenes(app, state);
}

/// Wire a callback that needs the app and the state:
/// `on!(app, state, on_x, |app, state, arg1, arg2| { ... })`
macro_rules! on {
    ($app:ident, $state:ident, $setter:ident, |$a:ident, $s:ident $(, $arg:ident)*| $body:block) => {{
        let weak = $app.as_weak();
        let state = $state.clone();
        $app.$setter(move |$($arg),*| {
            let Some($a) = weak.upgrade() else { return };
            let $s: &AppState = &state;
            $body
        });
    }};
}

fn setup_callbacks(app: &App, state: &Arc<AppState>) {
    // === Chain editing ===
    app.set_block_colors(model(
        views::BLOCK_COLORS
            .iter()
            .map(|(name, label, value)| ColorChoice {
                name: (*name).into(),
                label: (*label).into(),
                color: rgb(*value),
            })
            .collect(),
    ));
    on!(app, state, on_drag_started, |app, state, instance_id| {
        let slots = id(instance_id).map_or_else(Vec::new, |i| drop_slots(state, i));
        app.set_drop_slots(model(
            slots
                .into_iter()
                .map(|s| DropSlotView { x: s.x, y: s.y })
                .collect(),
        ));
    });
    on!(
        app,
        state,
        on_node_dropped,
        |app, state, instance_id, x, y| {
            let Some(instance_id) = id(instance_id) else {
                return;
            };
            let slots = drop_slots(state, instance_id);
            let Some(slot) = chain_layout::slot_at(&slots, x, y) else {
                return;
            };
            if report(&app, state.move_node_to(instance_id, slot.place)).is_some() {
                refresh_chain(&app, state);
                // Keep the moved block selected
                let nodes = app.get_positioned_nodes();
                if let Some(index) = (0..nodes.row_count()).find(|&i| {
                    nodes
                        .row_data(i)
                        .is_some_and(|n| n.instance_id == instance_id as i32)
                }) {
                    app.set_selected_plugin(index as i32);
                    app.set_selected_instance_id(instance_id as i32);
                }
            }
        }
    );
    on!(app, state, on_set_color, |app, state, instance_id, name| {
        if let Some(instance_id) = id(instance_id) {
            let name = Some(name.to_string()).filter(|n| views::block_color(n).is_some());
            state.set_color(instance_id, name);
            refresh_chain(&app, state);
        }
    });
    on!(app, state, on_add_plugin_to_chain, |app, state, uri| {
        if report(&app, state.add_plugin(&uri)).is_some() {
            refresh_chain(&app, state);
            warn_if_loaded(&app);
        }
    });
    on!(app, state, on_add_split, |app, state, kind| {
        let result = split_type(&kind).and_then(|kind| state.add_split(kind));
        if report(&app, result).is_some() {
            refresh_chain(&app, state);
        }
    });
    on!(app, state, on_set_split_type, |app, state, split, kind| {
        let result = id(split)
            .ok_or_else(|| anyhow::anyhow!("No split selected"))
            .and_then(|split| state.set_split_type(split, split_type(&kind)?));
        if report(&app, result).is_some() {
            refresh_chain(&app, state);
        }
    });
    on!(
        app,
        state,
        on_add_plugin_to_split_top,
        |app, state, split, uri| {
            let result = id(split)
                .ok_or_else(|| anyhow::anyhow!("No split selected"))
                .and_then(|split| state.add_plugin_to_split(split, true, &uri));
            if report(&app, result).is_some() {
                refresh_chain(&app, state);
                warn_if_loaded(&app);
            }
        }
    );
    on!(
        app,
        state,
        on_add_plugin_to_split_bottom,
        |app, state, split, uri| {
            let result = id(split)
                .ok_or_else(|| anyhow::anyhow!("No split selected"))
                .and_then(|split| state.add_plugin_to_split(split, false, &uri));
            if report(&app, result).is_some() {
                refresh_chain(&app, state);
                warn_if_loaded(&app);
            }
        }
    );
    on!(
        app,
        state,
        on_remove_node_by_id,
        |app, state, instance_id| {
            if let Some(instance_id) = id(instance_id) {
                if report(&app, state.remove_node(instance_id)).is_some() {
                    refresh_chain(&app, state);
                }
            }
        }
    );

    // === Bypass, splits and parameters (live, no chain rebuild) ===
    on!(
        app,
        state,
        on_toggle_node_bypass,
        |app, state, instance_id| {
            let Some(target) = id(instance_id) else {
                return;
            };
            if let Some(bypassed) = report(&app, state.toggle_bypass(target)) {
                show_bypass(&app, instance_id, bypassed);
            }
        }
    );
    on!(app, state, on_toggle_split_ab, |app, state, split| {
        let Some(target) = id(split) else { return };
        if let Some(select_a) = report(&app, state.toggle_split_ab(target)) {
            update_node(&app, split, |n| n.ab_select_a = select_a);
        }
    });
    on!(
        app,
        state,
        on_set_split_mix_ratio,
        |app, state, split, ratio| {
            let Some(target) = id(split) else { return };
            if let Some(ratio) = report(&app, state.set_split_mix(target, ratio)) {
                update_node(&app, split, |n| n.mix_ratio = ratio);
            }
        }
    );
    on!(app, state, on_set_split_pan, |app, state, split, a, b| {
        let Some(target) = id(split) else { return };
        // Panned hard to opposite sides, a final Mix is drawn as an L/R split
        let hard = |pan: [f32; 2]| pan[0].abs() == 1.0 && pan[1] == -pan[0];
        let was_hard = node(&app, split).is_some_and(|n| hard([n.pan_a, n.pan_b]));
        if let Some((pan, rebuilt)) = report(&app, state.set_split_pan(target, [a, b])) {
            if rebuilt || hard(pan) != was_hard {
                // The split now outputs mono or stereo: redraw the lines
                refresh_chain(&app, state);
            } else {
                update_node(&app, split, |n| [n.pan_a, n.pan_b] = pan);
            }
        }
    });
    on!(app, state, on_refresh_models, |app, state| {
        let uri = id(app.get_selected_instance_id()).and_then(|i| match state.chain().find(i) {
            Some(ChainNode::Plugin { uri, .. }) => Some(uri.clone()),
            _ => None,
        });
        let current = app.get_selected_file();
        let packs: Vec<ModelChoice> = uri
            .map(|uri| state.list_packs(&uri))
            .unwrap_or_default()
            .into_iter()
            .map(|pack| {
                let presets = match pack.files.len() {
                    1 => "1 preset".to_string(),
                    n => format!("{n} presets"),
                };
                ModelChoice {
                    image: pack_picture(Some(&pack)),
                    t3k: pack.tone3000,
                    subtitle: match &pack.creator {
                        Some(creator) => format!("by @{creator} · {presets}"),
                        None => presets,
                    }
                    .into(),
                    current: pack.files.iter().any(|f| *f == current.as_str()),
                    // A tone loads its first preset; the others are in the editor
                    file: pack.files.first().cloned().unwrap_or_default().into(),
                    title: pack.title.into(),
                }
            })
            .collect();
        app.set_available_packs_pictures(packs.iter().any(|p| p.t3k));
        app.set_available_packs(model(packs));
        app.set_models_dir(state.models_dir().display().to_string().into());
    });
    on!(app, state, on_set_model, |app, state, instance_id, file| {
        let Some(target) = id(instance_id) else {
            return;
        };
        let file = (!file.is_empty()).then(|| file.to_string());
        if report(&app, state.set_plugin_file(target, file)).is_some() {
            refresh_chain(&app, state);
            warn_if_loaded(&app);
        }
    });
    let power_request = std::env::var_os(POWER_REQUEST).map(std::path::PathBuf::from);
    app.global::<Features>().set_power(power_request.is_some());
    let weak = app.as_weak();
    app.on_power(move |action| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = &power_request else { return };
        if let Err(e) = std::fs::write(path, action.as_bytes()) {
            warn!("Cannot ask the system to {}: {}", action, e);
            app.set_power_dialog(false);
            show_status(&app, format!("Cannot {action}: {e}").into());
            return;
        }
        info!("Quitting to {}", action);
        // Leave the message on screen for a moment; quitting saves the session
        slint::Timer::single_shot(Duration::from_millis(300), || {
            let _ = slint::quit_event_loop();
        });
    });
    on!(app, state, on_t3k_add_model, |app, state, uri, file| {
        let added = state.add_plugin(&uri).and_then(|id| {
            state.set_plugin_file(id, Some(file.to_string()))?;
            Ok(id)
        });
        if let Some(id) = report(&app, added) {
            app.set_selected_instance_id(id as i32);
            refresh_chain(&app, state);
            warn_if_loaded(&app);
        }
    });
    on!(app, state, on_select_node, |app, state, instance_id| {
        app.set_selected_instance_id(instance_id);
        refresh_editor(&app, state, &state.chain());
    });
    on!(
        app,
        state,
        on_set_parameter,
        |app, state, instance_id, index, value| {
            let (Some(target), Ok(index)) = (id(instance_id), usize::try_from(index)) else {
                return;
            };
            let Some(applied) = report(&app, state.set_parameter(target, index, value)) else {
                return;
            };
            // Keep the editor model in sync (the value may have been clamped).
            // The row is updated in place so the control being dragged stays.
            if app.get_selected_instance_id() == instance_id {
                let params = app.get_selected_parameters();
                if let Some(mut param) = params.row_data(index) {
                    if param.value != applied {
                        param.value = applied;
                        params.set_row_data(index, param);
                    }
                }
            }
        }
    );
    on!(app, state, on_set_stereo_mode, |app, state, stereo| {
        state.set_stereo_mode(stereo);
        // Lines are drawn single or double after the signal
        refresh_chain(&app, state);
    });

    // === Presets ===
    on!(app, state, on_save_preset, |app, state, name| {
        let name = name.trim();
        let name = if name.is_empty() { "Untitled" } else { name };
        if report(&app, state.save_preset(name.to_string())).is_some() {
            show_status(&app, format!("Preset \"{}\" saved", name).into());
            refresh_presets(&app, state);
        }
    });
    on!(app, state, on_update_preset, |app, state| {
        if report(&app, state.update_preset()).is_some() {
            show_status(
                &app,
                format!("Preset \"{}\" saved", state.current_preset_name()).into(),
            );
            refresh_presets(&app, state);
        }
    });
    on!(app, state, on_load_preset, |app, state, preset_id| {
        load_preset(&app, state, &preset_id);
    });
    on!(app, state, on_delete_preset, |app, state, preset_id| {
        report(&app, state.delete_preset(&preset_id));
        refresh_presets(&app, state);
        refresh_scenes(&app, state);
    });

    // === Scenes ===
    on!(app, state, on_create_scene, |app, state, name| {
        let name = name.trim();
        let name = if name.is_empty() { "Untitled" } else { name };
        if let Some(scene_id) = report(&app, state.create_scene(name.to_string())) {
            report(&app, state.load_scene(&scene_id));
        }
        refresh_scenes(&app, state);
    });
    on!(app, state, on_load_scene, |app, state, scene_id| {
        report(&app, state.load_scene(&scene_id));
        refresh_scenes(&app, state);
    });
    on!(app, state, on_delete_scene, |app, state, scene_id| {
        report(&app, state.delete_scene(&scene_id));
        refresh_scenes(&app, state);
    });
    on!(
        app,
        state,
        on_scene_slot_assign,
        |app, state, slot, preset_id| {
            report(
                &app,
                state.set_current_scene_slot(slot as usize, Some(preset_id.to_string())),
            );
            refresh_scenes(&app, state);
        }
    );
    on!(app, state, on_scene_slot_clear, |app, state, slot| {
        report(&app, state.set_current_scene_slot(slot as usize, None));
        refresh_scenes(&app, state);
    });
    on!(app, state, on_footswitch_pressed, |app, state, slot| {
        setlist_slot(&app, state, slot as usize);
    });
    on!(app, state, on_tap_switch_changed, |app, state, on| {
        state.set_tap_switch(on);
        live_ui::refresh(&app, state);
    });

    // === Looper ===
    let looper = state.looper.clone();
    let with_looper = move |f: fn(&mut pedalboard::audio::Looper)| {
        let looper = looper.clone();
        move || pedalboard::audio::Looper::control(&looper, f)
    };
    app.on_looper_record(with_looper(|l| l.record()));
    app.on_looper_play(with_looper(|l| l.play()));
    app.on_looper_stop(with_looper(|l| l.stop()));
    app.on_looper_overdub(with_looper(|l| l.overdub()));
    app.on_looper_undo(with_looper(|l| l.undo()));
    app.on_looper_clear(with_looper(|l| l.clear()));
    let looper = state.looper.clone();
    app.on_looper_level_changed(move |level| {
        pedalboard::audio::Looper::control(&looper, |l| l.set_level(level))
    });

    // === Tuner ===
    let tuner = state.tuner.clone();
    app.on_tuner_mute_toggled(move |enabled| tuner.lock().unwrap().set_mute(enabled));
    let tuner = state.tuner.clone();
    app.on_tuner_reference_pitch_changed(move |hz| tuner.lock().unwrap().set_reference_pitch(hz));

    let edit = |(text, cursor): (String, usize)| TextEdit {
        text: text.into(),
        cursor: cursor as i32,
    };
    let editing = app.global::<TextEditing>();
    editing.on_replace(move |text, start, end, insert| {
        edit(text_edit::replace(&text, start, end, &insert))
    });
    editing.on_erase(move |text, start, end| edit(text_edit::erase(&text, start, end)));
}

/// Load a preset of the preset list
pub(crate) fn load_preset(app: &App, state: &AppState, preset_id: &str) {
    if report(app, state.load_preset(preset_id)).is_some() {
        app.set_selected_instance_id(-1);
        refresh_chain(app, state);
        refresh_presets(app, state);
    }
}

/// Load the preset of slot `slot` of the current setlist
pub(crate) fn setlist_slot(app: &App, state: &AppState, slot: usize) {
    if state.current_scene_id().is_none() {
        show_status(app, "No setlist loaded".into());
        return;
    }
    match report(app, state.switch_to_scene_slot(slot)) {
        Some(true) => {
            app.set_selected_instance_id(-1);
            refresh_chain(app, state);
            refresh_presets(app, state);
            refresh_scenes(app, state);
        }
        Some(false) => show_status(app, format!("Slot {} is empty", slot + 1).into()),
        None => {}
    }
}

/// Periodic UI updates: looper, tuner, audio errors, status timeout
fn start_poll_timer(
    app: &App,
    state: &Arc<AppState>,
    engine: Option<&AudioEngine>,
) -> slint::Timer {
    let weak = app.as_weak();
    let state = state.clone();
    let tuner_rx = engine.map(|e| e.tuner_receiver());
    let levels = engine.map(|e| e.levels());
    let mut meters = views::MeterView::default();
    let status_age = Rc::new(Cell::new(0.0f32));
    let last_status = Cell::new(0u32);
    let last_pitch = Cell::new(std::time::Instant::now());
    let tuner_shown = Cell::new(state.tuner.lock().unwrap().is_active());

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(POLL_MS),
        move || {
            let Some(app) = weak.upgrade() else { return };

            if let Err(e) = state.autosave() {
                warn!("Could not save the session: {}", e);
            }

            // Looper
            // (values copied out first: the audio thread skips the looper
            // for a cycle when the lock is taken)
            let (looper_state, length, position, layers) = {
                let looper = state.looper.lock().unwrap();
                (
                    looper.state(),
                    looper.loop_length_seconds(),
                    looper.position_seconds(),
                    looper.layer_count(),
                )
            };
            app.set_looper_state(match looper_state {
                EngineLooperState::Stopped => LooperState::Stopped,
                EngineLooperState::Recording => LooperState::Recording,
                EngineLooperState::Playing => LooperState::Playing,
                EngineLooperState::Overdubbing => LooperState::Overdubbing,
            });
            app.set_looper_length(length);
            app.set_looper_position(position);
            app.set_looper_layers(layers as i32);

            // Tuner: only analyse audio while the tuner page is shown
            // (locked only on a change: the audio thread skips the tuner, and
            // its mute, for a cycle when the lock is taken)
            let tuner_visible = app.get_current_view() == TUNER_VIEW;
            if tuner_shown.replace(tuner_visible) != tuner_visible {
                state.tuner.lock().unwrap().set_active(tuner_visible);
            }
            if let Some(rx) = &tuner_rx {
                if let Some(result) = rx.try_iter().last() {
                    last_pitch.set(std::time::Instant::now());
                    app.set_tuner_frequency(result.frequency);
                    app.set_tuner_note(result.note_name.into());
                    app.set_tuner_octave(result.octave);
                    app.set_tuner_cents(result.cents);
                    app.set_tuner_is_tuned(result.is_tuned);
                } else if last_pitch.get().elapsed() > TUNER_HOLD {
                    // The string has died out: back to "Play a string"
                    app.set_tuner_frequency(0.0);
                }
            }

            // Level meters (rack)
            if let Some(levels) = &levels {
                let shown = meters.lit();
                meters.update(levels.take(), POLL_MS as f32 / 1000.0);
                if meters.lit() != shown {
                    app.set_levels(Levels {
                        input: meters.bars[0],
                        left: meters.bars[1],
                        right: meters.bars[2],
                        clip_in: meters.clip_in,
                        clip_out: meters.clip_out,
                    });
                }
            }

            // Errors from the chain builder
            if let Some(message) = state.poll_audio_error() {
                show_status(&app, message.into());
            }

            // Hide the status message after a while
            let shown = STATUS_SHOWN.with(Cell::get);
            if shown != last_status.get() {
                last_status.set(shown);
                status_age.set(0.0);
            } else if !app.get_status_message().is_empty() {
                status_age.set(status_age.get() + POLL_MS as f32 / 1000.0);
                if status_age.get() > STATUS_SECONDS {
                    app.set_status_message(SharedString::new());
                }
            }
        },
    );
    timer
}

/// After adding an effect: warn when the board was already busy, since the
/// new block only adds to the load
fn warn_if_loaded(app: &App) {
    let resources = app.get_resources();
    if resources.dsp_load >= monitor::DSP_WARNING || resources.level >= Level::Critical as i32 {
        show_status(
            app,
            format!(
                "Load was already high ({:.0}% DSP): watch the monitor for dropouts",
                resources.dsp_load.max(0.0) * 100.0
            )
            .into(),
        );
    }
}

fn throttling_text(throttling: monitor::Throttling) -> &'static str {
    if throttling.under_voltage {
        "Under-voltage"
    } else if throttling.throttled {
        "Throttled"
    } else if throttling.frequency_capped {
        "Freq. capped"
    } else if throttling.soft_temperature_limit {
        "Temp. limit"
    } else {
        "OK"
    }
}

/// Resource monitor: DSP load, xruns and board health, once per second
fn start_monitor_timer(app: &App, engine: Option<Rc<AudioEngine>>) -> slint::Timer {
    let weak = app.as_weak();
    let mut system = SystemMonitor::new();
    let meter = engine.as_ref().map(|e| e.meter());
    let mut last_xruns = 0u32;
    let mut last_level = Level::Ok;
    // Selected block, its time spent and when it was read
    let mut last_block: Option<(i32, u64, std::time::Instant)> = None;

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(MONITOR_MS),
        move || {
            let Some(app) = weak.upgrade() else { return };
            let stats = system.sample();
            let dsp = meter.as_ref().map(|m| m.take());
            let xruns = dsp.map_or(0, |d| d.xruns);
            let new_xruns = xruns.saturating_sub(last_xruns);
            last_xruns = xruns;
            let (level, message) = monitor::assess(dsp.as_ref(), new_xruns, &stats);

            // Share of a core the selected block uses
            let selected = app.get_selected_instance_id();
            let now = std::time::Instant::now();
            let spent = id(selected).and_then(|i| monitor::BlockMeter::global().spent(i));
            let load = match (last_block, spent) {
                (Some((last, before, then)), Some(after)) if last == selected => {
                    monitor::block_load(before, after, now - then)
                }
                _ => -1.0,
            };
            app.set_selected_cpu(load);
            last_block = spent.map(|s| (selected, s, now));

            if level > last_level {
                warn!("Resources: {}", message);
                if level == Level::Critical {
                    show_status(&app, message.clone().into());
                }
            }
            last_level = level;

            let known = |value: Option<f32>| value.unwrap_or(-1.0);
            let running = dsp.filter(|d| d.cycles > 0);
            app.set_resources(ResourceStatus {
                dsp_load: running.map_or(-1.0, |d| d.average),
                dsp_peak: running.map_or(-1.0, |d| d.peak),
                jack_load: engine.as_ref().map_or(-1.0, |e| e.jack_load()),
                cpu: known(stats.cpu),
                cpu_max_core: known(stats.cpu_max_core),
                temperature: known(stats.temperature),
                memory_used_mb: stats.memory_used_mb.unwrap_or(0) as i32,
                memory_total_mb: stats.memory_total_mb.unwrap_or(0) as i32,
                xruns: xruns as i32,
                latency_ms: engine
                    .as_ref()
                    .map_or(-1.0, |e| e.buffer_size() as f32 / e.sample_rate() * 1000.0),
                buffer_info: engine
                    .as_ref()
                    .map(|e| format!("{} @ {:.1} kHz", e.buffer_size(), e.sample_rate() / 1000.0))
                    .unwrap_or_default()
                    .into(),
                throttling: stats
                    .throttling
                    .map(throttling_text)
                    .unwrap_or_default()
                    .into(),
                level: level as i32,
                message: message.into(),
            });
        },
    );
    timer
}

/// Watch the JACK server: report when it stops and, under a supervisor
/// (`PEDALBOARD_EXIT_ON_AUDIO_CHANGE`), quit so the app restarts on a new one
fn start_audio_watch(app: &App, engine: Option<Rc<AudioEngine>>) -> slint::Timer {
    let weak = app.as_weak();
    let restart = std::env::var_os(EXIT_ON_AUDIO_CHANGE).is_some();
    let reported = Cell::new(false);

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(AUDIO_WATCH_MS),
        move || {
            let Some(app) = weak.upgrade() else { return };
            match &engine {
                Some(engine) if engine.stopped() && !reported.get() => {
                    reported.set(true);
                    app.set_audio_running(false);
                    if restart {
                        info!("JACK server stopped: quitting to restart");
                        let _ = slint::quit_event_loop();
                    } else {
                        show_status(&app, "Audio stopped: the JACK server shut down".into());
                    }
                }
                None if restart && jack_client::server_running() => {
                    info!("JACK server started: quitting to restart with audio");
                    let _ = slint::quit_event_loop();
                }
                _ => {}
            }
        },
    );
    timer
}

/// Writes the supervisor's alive file again whenever it was removed
fn start_alive_timer() -> Option<slint::Timer> {
    let path = std::path::PathBuf::from(std::env::var_os(ALIVE_FILE)?);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            if !path.exists() {
                if let Err(e) = std::fs::write(&path, b"") {
                    warn!("{}: {e}", path.display());
                }
            }
        },
    );
    Some(timer)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        // No color codes when piped to syslog (the OS image)
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    info!("Pedalboard v{}", env!("CARGO_PKG_VERSION"));

    let state = Arc::new(AppState::new()?);
    let demo = state.lv2_host.is_demo();
    if demo {
        warn!(
            "No LV2 plugin found: demo plugins only (install some, e.g. guitarix, or set LV2_PATH)"
        );
    } else {
        info!(
            "{} LV2 plugins available",
            state.lv2_host.scanner().plugins().len()
        );
    }

    let engine = match AudioEngine::new(
        state.lv2_host.clone(),
        state.tuner.clone(),
        state.looper.clone(),
    ) {
        Ok(engine) => {
            info!(
                "Audio running: {} Hz, {} frames",
                engine.sample_rate(),
                engine.buffer_size()
            );
            state.set_audio_control(engine.control());
            Some(Rc::new(engine))
        }
        Err(e) => {
            error!("Audio disabled: {:#}", e);
            error!("Start JACK first, e.g.: jackd -d alsa -r 48000 -p 128 -n 2");
            None
        }
    };

    match state.restore_session() {
        Ok(true) => info!("Previous session restored"),
        Ok(false) => {}
        Err(e) => warn!("Could not restore the previous session: {}", e),
    }

    let app = App::new()?;
    let plugins: Vec<PluginItem> = views::plugin_views(&state)
        .into_iter()
        .map(|p| PluginItem {
            uri: p.uri.into(),
            name: p.name.into(),
            author: p.author.into(),
            category: p.category.into(),
            kind: p.kind.into(),
            hint: p.hint.into(),
            color: rgb(p.color),
        })
        .collect();
    app.set_available_plugins(model(plugins));
    app.set_audio_running(engine.is_some());
    app.set_demo_plugins(demo);
    if engine.is_none() {
        show_status(&app, "Audio disabled: JACK is not running".into());
    }
    {
        let tuner = state.tuner.lock().unwrap();
        app.set_tuner_mute_enabled(tuner.is_muted());
        app.set_tuner_reference_pitch(tuner.reference_pitch());
    }
    app.set_looper_level(state.looper.lock().unwrap().level());

    setup_callbacks(&app, &state);
    #[cfg(feature = "tone3000")]
    tone3000_ui::setup(&app, &state);
    wifi_ui::setup(&app);
    #[cfg(feature = "update")]
    update_ui::setup(&app);
    live_ui::setup(&app, &state);
    midi_ui::setup(&app, &state);
    refresh_all(&app, &state);
    let _timer = start_poll_timer(&app, &state, engine.as_deref());
    let _monitor = start_monitor_timer(&app, engine.clone());
    let _audio_watch = start_audio_watch(&app, engine.clone());
    let _alive = start_alive_timer();

    // Footswitches and encoder, on the GPIO with the gpio feature
    hardware_ui::setup(&app, &state);

    app.run()?;

    if let Err(e) = state.save_session() {
        warn!("Could not save the session: {}", e);
    }
    drop(_monitor);
    drop(_audio_watch);
    drop(engine);
    Ok(())
}
