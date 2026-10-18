//! Routing view: the input feeding the effects, what each output of the
//! interface plays, and the sends and returns of the preset's blocks.
//! Output changes are heard at once and saved to routing.json a moment after
//! the last one, so a knob drag writes the file once; sends and returns are
//! part of the preset, and rebuild the chain.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel, Weak};
use tracing::warn;

use pedalboard::audio::routing::{
    BlockIo, Fold, PortMeter, Routing, RoutingControl, Source, MAX_PORTS, MAX_SENDS,
};
use pedalboard::audio::{AudioEngine, ChainNode};
use pedalboard::state::{AppState, EffectChainState};
use pedalboard::ui::slint_bridge::{meter_position, METER_FLOOR_DB};

use crate::{block_name, model, refresh_chain, report, App, RoutingBlock, RoutingOutput};

/// The routing view, as `current-view` in ui/app.slint
const VIEW: i32 = 10;
/// Meter refresh while the view is shown
const METER_PERIOD: Duration = Duration::from_millis(33);
/// Meter fall, as the main meters
const FALL_DB_PER_S: f32 = 24.0;
/// Wait after the last change before saving
const SAVE_DELAY: Duration = Duration::from_secs(1);

struct Editor {
    routing: Routing,
    control: RoutingControl,
    /// Rows of the view, updated in place so a knob being dragged stays
    rows: Rc<VecModel<RoutingOutput>>,
    saver: Timer,
}

impl Drop for Editor {
    /// Save a change still waiting when the app quits
    fn drop(&mut self) {
        if self.saver.running() {
            save(&self.routing);
        }
    }
}

fn save(routing: &Routing) {
    if let Err(e) = routing.save(&pedalboard::state::data_dir()) {
        warn!("Could not save the routing: {e:#}");
    }
}

/// What the callbacks share
#[derive(Clone)]
struct Context {
    app: Weak<App>,
    editor: Rc<RefCell<Editor>>,
}

impl Context {
    /// Change the routing: heard at once, shown, and saved a moment later
    fn change(&self, edit: impl FnOnce(&mut Routing, usize, usize)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let editor = &mut *self.editor.borrow_mut();
        let (inputs, outputs) = (editor.control.inputs, editor.control.outputs);
        let before = editor.routing.clone();
        edit(&mut editor.routing, inputs, outputs);
        if editor.routing == before {
            return;
        }
        editor.control.apply(&editor.routing);
        show(&app, &editor.routing, &editor.rows, inputs, outputs);
        let routing = editor.routing.clone();
        editor
            .saver
            .start(TimerMode::SingleShot, SAVE_DELAY, move || save(&routing));
    }
}

pub fn setup(app: &App, state: &Arc<AppState>, engine: Option<&AudioEngine>) -> Option<Timer> {
    let Some(engine) = engine else {
        app.set_routing_available(false);
        app.set_routing_summary("Available once the audio interface is running.".into());
        return None;
    };
    let control = engine.routing();
    let (inputs, outputs) = (control.inputs, control.outputs);
    let routing = control.initial.clone();
    let rows = Rc::new(VecModel::default());
    app.set_routing_available(true);
    app.set_routing_inputs(inputs as i32);
    app.set_routing_outputs(ModelRc::from(rows.clone()));
    show(app, &routing, &rows, inputs, outputs);

    let context = Context {
        app: app.as_weak(),
        editor: Rc::new(RefCell::new(Editor {
            routing,
            control,
            rows,
            saver: Timer::default(),
        })),
    };
    let port = |p: i32| p.clamp(0, MAX_PORTS as i32 - 1) as u8;

    let c = context.clone();
    app.on_routing_step_input(move |delta| {
        c.change(|r, inputs, _| r.step_input(delta.signum(), inputs))
    });
    let c = context.clone();
    app.on_routing_step_source(move |p, delta| {
        c.change(|r, inputs, outputs| r.step_source(port(p), delta.signum(), inputs, outputs))
    });
    let c = context.clone();
    app.on_routing_set_linked(move |p, linked| {
        c.change(|r, _, outputs| r.set_linked(port(p), linked, outputs))
    });
    let c = context.clone();
    app.on_routing_set_gain(move |p, db| c.change(|r, _, _| r.edit(port(p), |s| s.gain_db = db)));
    let c = context.clone();
    app.on_routing_set_pan(move |p, pan| c.change(|r, _, _| r.edit(port(p), |s| s.pan = pan)));
    let c = context.clone();
    app.on_routing_set_fold(move |p, fold| {
        c.change(|r, _, _| r.edit(port(p), |s| s.fold = Fold::from_index(fold)))
    });
    let c = context;
    app.on_routing_toggle(move |p, what| {
        c.change(|r, _, _| {
            r.edit(port(p), |s| match what.as_str() {
                "mute" => s.mute = !s.mute,
                "solo" => s.solo = !s.solo,
                "invert" => s.invert = !s.invert,
                _ => {}
            })
        })
    });

    // Sends and returns of the blocks
    let step = move |io: &mut Option<u8>, delta: i32, count: usize| {
        let index = io.map_or(-1, i32::from) + delta.signum();
        *io = (0..count as i32).contains(&index).then_some(index as u8);
    };
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_routing_block_input(move |id, delta| {
        let Some(app) = weak.upgrade() else { return };
        edit_block(&app, &st, id, |io| step(&mut io.input, delta, inputs));
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_routing_block_send(move |id, delta| {
        let Some(app) = weak.upgrade() else { return };
        edit_block(&app, &st, id, |io| step(&mut io.send, delta, MAX_SENDS));
    });

    Some(start_meters(
        app,
        state.clone(),
        engine.port_levels(),
        inputs,
        outputs,
    ))
}

/// Change the send or return of block `id`, then show it here and in the rack
fn edit_block(app: &App, state: &AppState, id: i32, change: impl FnOnce(&mut BlockIo)) {
    let Ok(id) = usize::try_from(id) else { return };
    let mut io = state.chain().io.get(&id).copied().unwrap_or_default();
    change(&mut io);
    report(app, state.set_block_io(id, io));
    refresh_chain(app, state);
}

/// The chain changed (edit, preset, scene, undo): list its blocks again if
/// the view is open
pub(crate) fn chain_changed(app: &App, state: &AppState) {
    if app.get_current_view() == VIEW {
        show_blocks(app, state);
    }
}

/// The blocks of the current preset, in signal order
fn show_blocks(app: &App, state: &AppState) {
    let chain = state.chain();
    let mut rows = Vec::new();
    collect_blocks(state, &chain, &chain.nodes, "", &mut rows);
    app.set_routing_blocks(model(rows));
}

fn collect_blocks(
    state: &AppState,
    chain: &EffectChainState,
    nodes: &[ChainNode],
    place: &str,
    rows: &mut Vec<RoutingBlock>,
) {
    for node in nodes {
        let (id, name) = match node {
            ChainNode::Plugin {
                instance_id, uri, ..
            } => (*instance_id, block_name(state, chain, *instance_id, uri)),
            ChainNode::Split {
                instance_id,
                split_type,
                ..
            } => (*instance_id, split_type.label().to_string()),
        };
        let io = chain.io.get(&id).copied().unwrap_or_default();
        rows.push(RoutingBlock {
            instance_id: id as i32,
            name: name.clone().into(),
            place: place.into(),
            input: io.input.map_or(-1, i32::from),
            send: io.send.map_or(-1, i32::from),
        });
        if let ChainNode::Split {
            top_chain,
            bottom_chain,
            ..
        } = node
        {
            let inner = |branch: &str| match place {
                "" => format!("{name} · {branch}"),
                outer => format!("{outer} › {name} · {branch}"),
            };
            collect_blocks(state, chain, top_chain, &inner("A"), rows);
            collect_blocks(state, chain, bottom_chain, &inner("B"), rows);
        }
    }
}

/// The routing as the view shows it: one entry per strip, and one per
/// output playing nothing
fn show(
    app: &App,
    routing: &Routing,
    shown: &VecModel<RoutingOutput>,
    inputs: usize,
    outputs: usize,
) {
    let outputs = outputs.min(MAX_PORTS);
    let mut rows = Vec::new();
    let mut port = 0;
    while port < outputs {
        let row = match routing.strip(port as u8) {
            Some(strip) => {
                let (source, bus) = match strip.source {
                    Source::Main => ("Main mix".to_string(), true),
                    Source::Send(s) => (format!("Send {}", s + 1), true),
                    Source::Input(i) => (format!("Input {}", i + 1), false),
                };
                let stereo = strip.stereo && port + 1 < outputs;
                RoutingOutput {
                    port: port as i32,
                    label: if stereo {
                        format!("{}–{}", port + 1, port + 2).into()
                    } else {
                        (port + 1).to_string().into()
                    },
                    stereo,
                    can_link: routing.can_link(port as u8, outputs),
                    assigned: true,
                    source: source.into(),
                    bus,
                    gain_db: strip.gain_db,
                    pan: strip.pan,
                    fold: strip.fold.index(),
                    mute: strip.mute,
                    solo: strip.solo,
                    invert: strip.invert,
                }
            }
            None => RoutingOutput {
                port: port as i32,
                label: (port + 1).to_string().into(),
                source: "Off".into(),
                ..Default::default()
            },
        };
        port += if row.stereo { 2 } else { 1 };
        rows.push(row);
    }
    app.set_routing_input(routing.input as i32);
    app.set_routing_summary(summary(routing, inputs, outputs).into());
    if shown.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            if shown.row_data(i).as_ref() != Some(&row) {
                shown.set_row_data(i, row);
            }
        }
    } else {
        shown.set_vec(rows);
    }
}

/// One line for the settings card
fn summary(routing: &Routing, inputs: usize, outputs: usize) -> String {
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut used: Vec<usize> = routing
        .strips
        .iter()
        .filter(|s| s.source == Source::Main)
        .flat_map(|s| s.ports())
        .filter(|&p| p < outputs)
        .map(|p| p + 1)
        .collect();
    used.sort_unstable();
    let main = match used.as_slice() {
        [] => "the main mix plays on no output".to_string(),
        [one] => format!("main mix on output {one}"),
        many => {
            let list: Vec<String> = many.iter().map(|p| p.to_string()).collect();
            format!("main mix on outputs {}", list.join(", "))
        }
    };
    format!(
        "{} and {}. Effects on input {}, {main}.",
        plural(inputs, "input"),
        plural(outputs, "output"),
        routing.input + 1
    )
}

/// Polls the port meters while the view is shown, and lists the blocks of
/// the preset when it opens
fn start_meters(
    app: &App,
    state: Arc<AppState>,
    meter: Arc<PortMeter>,
    inputs: usize,
    outputs: usize,
) -> Timer {
    let timer = Timer::default();
    let weak = app.as_weak();
    let mut bars = ([0f32; MAX_PORTS], [0f32; MAX_PORTS]);
    let mut shown: Option<(Vec<usize>, Vec<usize>)> = None;
    timer.start(TimerMode::Repeated, METER_PERIOD, move || {
        let Some(app) = weak.upgrade() else { return };
        // Read even when hidden, so the peaks do not pile up
        let (input_peaks, output_peaks) = meter.take();
        if app.get_current_view() != VIEW {
            shown = None;
            return;
        }
        if shown.is_none() {
            show_blocks(&app, &state);
        }
        let fall = FALL_DB_PER_S * METER_PERIOD.as_secs_f32() / -METER_FLOOR_DB;
        let fold = |bar: &mut f32, peak: f32| *bar = meter_position(peak).max(*bar - fall);
        for (bar, &peak) in bars.0.iter_mut().zip(&input_peaks) {
            fold(bar, peak);
        }
        for (bar, &peak) in bars.1.iter_mut().zip(&output_peaks) {
            fold(bar, peak);
        }
        // Only repaint when a LED changes
        let lit = |b: &[f32]| {
            b.iter()
                .map(|v| (v * 24.0).ceil() as usize)
                .collect::<Vec<_>>()
        };
        let now = (lit(&bars.0[..inputs]), lit(&bars.1[..outputs]));
        if shown.as_ref() != Some(&now) {
            app.set_routing_input_levels(model(bars.0[..inputs].to_vec()));
            app.set_routing_output_levels(model(bars.1[..outputs].to_vec()));
            shown = Some(now);
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_names_the_main_outputs() {
        let routing = Routing::default();
        assert_eq!(
            summary(&routing, 1, 2),
            "1 input and 2 outputs. Effects on input 1, main mix on outputs 1, 2."
        );
        let mut routing = Routing::default();
        routing.step_source(0, -1, 4, 4);
        assert_eq!(
            summary(&routing, 4, 4),
            "4 inputs and 4 outputs. Effects on input 1, the main mix plays on no output."
        );
    }
}
