//! Routing view: the input feeding the effects, what each output of the
//! interface plays, and the send and return blocks of the preset.
//! Output changes are heard at once and saved to routing.json a moment after
//! the last one, so a knob drag writes the file once; the send and return
//! blocks are set in the rack, like any block.
//! Each output or pair has a global EQ and a limiter, edited in a panel over
//! the view (OutputMaster in ui/pages/routing_view.slint).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel, Weak};
use tracing::{error, warn};

use pedalboard::audio::master::{self, Limiter, OutputEq, BANDS, BAND_KINDS};
use pedalboard::audio::routing::{Fold, PortMeter, Routing, RoutingControl, Source, MAX_PORTS};
use pedalboard::audio::{AudioEngine, ChainNode};
use pedalboard::state::{AppState, EffectChainState};
use pedalboard::ui::slint_bridge::{meter_position, METER_FLOOR_DB};

use pedalboard::plugins::scanner::{self, ret, send};

use crate::plugin_view::{self, area, freq, line, VIEW as PLOT};
use crate::{
    block_name, model, App, EqView, OutputMaster, PlotHandle, RoutingBlock, RoutingMaster,
    RoutingOutput,
};

/// The routing view, as `current-view` in ui/app.slint
const VIEW: i32 = 10;
/// Meter refresh while the view is shown
const METER_PERIOD: Duration = Duration::from_millis(33);
/// Meter fall, as the main meters
const FALL_DB_PER_S: f32 = 24.0;
/// Wait after the last change before saving
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// Gain axis of the output EQ curve: ± this many dB
const EQ_RANGE: f32 = 18.0;
/// Points of the curve
const EQ_STEPS: usize = 160;
/// Gain reduction meter fall, dB per second
const REDUCTION_FALL_DB_PER_S: f32 = 20.0;

/// The EQ or limiter panel
#[derive(Debug, Clone, Copy)]
struct MasterEdit {
    /// First output of the strip edited, when the panel is open
    port: Option<u8>,
    /// 0 = EQ, 1 = limiter
    tab: i32,
    band: usize,
    /// Width over height of the curve's plot
    aspect: f32,
}

impl Default for MasterEdit {
    fn default() -> Self {
        Self {
            port: None,
            tab: 0,
            band: 2,
            aspect: 4.0,
        }
    }
}

struct Editor {
    routing: Routing,
    control: RoutingControl,
    /// Rows of the view, updated in place so a knob being dragged stays
    rows: Rc<VecModel<RoutingOutput>>,
    saver: Timer,
    master: MasterEdit,
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
        error!(error = format_args!("{e:#}"), "Routing not saved");
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
        // A change made from inside another (a Slint re-entry) is dropped
        // rather than borrowing the editor twice
        let Ok(mut editor) = self.editor.try_borrow_mut() else {
            warn!("Routing editor in use, change skipped");
            return;
        };
        let editor = &mut *editor;
        let (inputs, outputs) = (editor.control.inputs, editor.control.outputs);
        let before = editor.routing.clone();
        edit(&mut editor.routing, inputs, outputs);
        if editor.routing == before {
            return;
        }
        editor.control.apply(&editor.routing);
        show(&app, &editor.routing, &editor.rows, inputs, outputs);
        show_master(&app, editor);
        let routing = editor.routing.clone();
        editor
            .saver
            .start(TimerMode::SingleShot, SAVE_DELAY, move || save(&routing));
    }
}

impl Context {
    /// Change what the EQ / limiter panel shows (not the routing)
    fn panel(&self, edit: impl FnOnce(&mut MasterEdit)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Ok(mut editor) = self.editor.try_borrow_mut() else {
            return;
        };
        edit(&mut editor.master);
        show_master(&app, &editor);
    }

    /// The strip and band the panel shows
    fn edited(&self) -> Option<(u8, usize)> {
        let editor = self.editor.try_borrow().ok()?;
        Some((editor.master.port?, editor.master.band))
    }

    /// Change the EQ of the strip the panel shows
    fn eq(&self, edit: impl FnOnce(&mut OutputEq, usize)) {
        if let Some((port, band)) = self.edited() {
            self.change(|r, _, _| r.edit(port, |s| edit(&mut s.eq, band)));
        }
    }

    /// Change its limiter
    fn limiter(&self, edit: impl FnOnce(&mut Limiter)) {
        if let Some((port, _)) = self.edited() {
            self.change(|r, _, _| r.edit(port, |s| edit(&mut s.limiter)));
        }
    }
}

/// Round to `step`
fn snap(value: f32, step: f32) -> f32 {
    (value / step).round() * step
}

/// Release knob position (0..1, log) and back
fn release_position(ms: f32) -> f32 {
    let (low, high) = master::RELEASE_RANGE_MS;
    ((ms / low).ln() / (high / low).ln()).clamp(0.0, 1.0)
}

fn release_ms(position: f32) -> f32 {
    let (low, high) = master::RELEASE_RANGE_MS;
    low * (high / low).powf(position.clamp(0.0, 1.0))
}

/// Frequency at a knob or point position, rounded as a desk shows it
fn frequency_at(x: f32) -> f32 {
    let f = freq::at(x.clamp(0.0, 1.0));
    match f {
        f if f < 100.0 => snap(f, 1.0),
        f if f < 1000.0 => snap(f, 5.0),
        f => snap(f, 50.0),
    }
}

fn freq_text(f: f32) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{f:.0} Hz")
    }
}

fn db_text(db: f32) -> String {
    format!("{}{db:.1} dB", if db > 0.0 { "+" } else { "" })
}

/// The handle of output EQ band `band`, and back (PlotEvents)
fn band_handle(band: usize) -> String {
    format!("out band {band}")
}

pub(crate) fn eq_band(handle: &str) -> Option<usize> {
    handle.strip_prefix("out band ")?.parse().ok()
}

/// Show the EQ / limiter panel of the strip edited, or hide it
fn show_master(app: &App, editor: &Editor) {
    let global = app.global::<OutputMaster>();
    let edit = editor.master;
    let strip = edit.port.and_then(|p| editor.routing.strip(p));
    let (Some(port), Some(strip)) = (edit.port, strip) else {
        // Closed, or the output was switched off
        if global.get_edit().shown {
            global.set_edit(RoutingMaster::default());
        }
        return;
    };
    let outputs = editor.control.outputs.min(MAX_PORTS);
    let first = port as usize + 1;
    let title = match strip.stereo && first < outputs {
        true => format!("OUT {first}–{}", first + 1),
        false => format!("OUT {first}"),
    };
    let band = edit.band.min(BANDS - 1);
    let values = strip.eq.bands()[band];
    let kind = BAND_KINDS[band].0;
    let limiter = strip.limiter;
    let view = RoutingMaster {
        shown: true,
        port: port as i32,
        title: title.into(),
        tab: edit.tab,
        eq_on: strip.eq.on,
        band: band as i32,
        band_on: values.on,
        has_gain: kind.has_gain(),
        has_q: kind.has_q(),
        freq: freq::x(values.freq),
        freq_text: freq_text(values.freq).into(),
        gain: values.gain_db,
        gain_text: db_text(values.gain_db).into(),
        q: values.q,
        q_text: format!("{:.2}", values.q).into(),
        limiter_on: limiter.on,
        ceiling: limiter.ceiling_db,
        ceiling_text: db_text(limiter.ceiling_db).into(),
        release: release_position(limiter.release_ms),
        release_text: format!("{:.0} ms", limiter.release_ms).into(),
        latency_text: format!("{} ms", master::LOOKAHEAD_MS).into(),
    };
    if global.get_edit() != view {
        global.set_edit(view);
    }
    let (eq, points) = eq_curve(&strip.eq, edit.aspect);
    if global.get_eq() != eq {
        global.set_eq(eq);
    }
    let current = global.get_points();
    let points = plugin_view::in_place(&current, points);
    if points != current {
        global.set_points(points);
    }
}

/// Height of a gain on the curve, 0 to 1 from the bottom
fn up(db: f32) -> f32 {
    (0.5 + db / (2.0 * EQ_RANGE)).clamp(0.0, 1.0)
}

/// The curve of an output EQ, for a plot `aspect` times wider than high,
/// and its band points
fn eq_curve(eq: &OutputEq, aspect: f32) -> (EqView, Vec<PlotHandle>) {
    let bands: Vec<_> = eq
        .bands()
        .iter()
        .zip(BAND_KINDS)
        .map(|(b, (kind, _))| b.curve(kind))
        .collect();
    let width = PLOT * aspect;
    let xy: Vec<(f32, f32)> = freq::steps(EQ_STEPS)
        .into_iter()
        .map(|f| {
            let db: f32 = bands.iter().map(|b| b.gain_at(f)).sum();
            (freq::x(f) * width, (1.0 - up(db)) * PLOT)
        })
        .collect();
    let flat = xy.iter().all(|(_, y)| (y - PLOT / 2.0).abs() < 0.05);
    let view = EqView {
        shown: true,
        enabled: eq.on,
        curve: line(xy.iter().copied()).into(),
        // Nothing to fill under a flat curve (an empty area is drawn as a seam)
        fill: if flat {
            String::new()
        } else {
            area(xy, PLOT / 2.0)
        }
        .into(),
        range: EQ_RANGE,
    };
    let points = bands
        .iter()
        .zip(BAND_KINDS)
        .enumerate()
        .map(|(i, (b, (kind, _)))| PlotHandle {
            shown: true,
            name: band_handle(i).into(),
            label: ["LC", "LS", "1", "2", "HS", "HC"][i].into(),
            on: b.on,
            x: freq::x(b.freq),
            // A cut has no gain: its point sits on the curve
            y: if kind.has_gain() {
                up(b.gain)
            } else {
                up(b.gain_at(b.freq).min(0.0))
            },
        })
        .collect();
    (view, points)
}

/// Answer the EQ / limiter panel
fn setup_master(app: &App, context: &Context) {
    let global = app.global::<OutputMaster>();
    let c = context.clone();
    global.on_open(move |port, tab| {
        c.panel(|m| {
            m.port = u8::try_from(port).ok();
            m.tab = tab.clamp(0, 1);
        })
    });
    let c = context.clone();
    global.on_close(move || c.panel(|m| m.port = None));
    let c = context.clone();
    global.on_set_tab(move |tab| c.panel(|m| m.tab = tab.clamp(0, 1)));
    let c = context.clone();
    global.on_select_band(move |band| c.panel(|m| m.band = (band.max(0) as usize).min(BANDS - 1)));
    let c = context.clone();
    global.on_resized(move |w, h| {
        if w > 0.0 && h > 0.0 {
            c.panel(|m| m.aspect = w / h)
        }
    });
    let c = context.clone();
    global.on_set_eq(move |what, value| {
        c.eq(|eq, band| {
            let on = value >= 0.5;
            if what == "on" {
                eq.on = on;
            } else if what == "flat" {
                eq.flatten();
            } else if let Some(b) = eq.band_mut(band) {
                match what.as_str() {
                    "band-on" => b.on = on,
                    "freq" => b.freq = frequency_at(value),
                    "gain" => b.gain_db = snap(value, 0.1),
                    "q" => b.q = snap(value, 0.01),
                    _ => {}
                }
            }
        })
    });
    let c = context.clone();
    global.on_set_limiter(move |what, value| {
        c.limiter(|l| match what.as_str() {
            "on" => l.on = value >= 0.5,
            "ceiling" => l.ceiling_db = snap(value, 0.1),
            "release" => l.release_ms = snap(release_ms(value), 1.0),
            _ => {}
        })
    });
    // A point dragged sets its band's frequency and gain, and switches the
    // band and the EQ on
    let c = context.clone();
    global.on_drag(move |band, x, y| {
        let band = (band.max(0) as usize).min(BANDS - 1);
        c.panel(|m| m.band = band);
        c.eq(|eq, band| {
            eq.on = true;
            if let Some(b) = eq.band_mut(band) {
                b.on = true;
                b.freq = frequency_at(x);
                if BAND_KINDS[band].0.has_gain() {
                    b.gain_db = snap((y - 0.5) * 2.0 * EQ_RANGE, 0.1);
                }
            }
        })
    });
    let c = context.clone();
    global.on_toggle_band(move |band| {
        let band = (band.max(0) as usize).min(BANDS - 1);
        c.panel(|m| m.band = band);
        c.eq(|eq, band| {
            if let Some(b) = eq.band_mut(band) {
                b.on = !b.on;
            }
        })
    });
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
            master: MasterEdit::default(),
        })),
    };
    setup_master(app, &context);
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
    let c = context.clone();
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

    Some(start_meters(
        app,
        state.clone(),
        engine.port_levels(),
        inputs,
        outputs,
        context,
    ))
}

/// The chain changed (edit, preset, scene, undo): list its blocks again if
/// the view is open
pub(crate) fn chain_changed(app: &App, state: &AppState) {
    if app.get_current_view() == VIEW {
        show_blocks(app, state);
    }
}

/// The send and return blocks of the current preset, in signal order
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
    let mut before = None;
    for node in nodes {
        match node {
            ChainNode::Plugin {
                instance_id,
                uri,
                enabled,
                ..
            } => {
                let name = block_name(state, chain, *instance_id, uri);
                if scanner::is_routing(uri) {
                    let after = match &before {
                        Some(block) => format!("after {block}"),
                        None => "at the start".to_string(),
                    };
                    rows.push(RoutingBlock {
                        instance_id: *instance_id as i32,
                        name: match uri.as_str() {
                            scanner::RETURN_URI => format!("Return · {name}"),
                            _ => name.clone(),
                        }
                        .into(),
                        place: match place {
                            "" => after,
                            branch => format!("{branch}, {after}"),
                        }
                        .into(),
                        bypassed: !enabled,
                    });
                }
                before = Some(name);
            }
            ChainNode::Split {
                split_type,
                top_chain,
                bottom_chain,
                ..
            } => {
                let name = split_type.label();
                let inner = |branch: &str| match place {
                    "" => format!("{name} · {branch}"),
                    outer => format!("{outer} › {name} · {branch}"),
                };
                collect_blocks(state, chain, top_chain, &inner("A"), rows);
                collect_blocks(state, chain, bottom_chain, &inner("B"), rows);
                before = Some(name.to_string());
            }
        }
    }
}

/// Name of a send or return block in the rack: "Send 1", "Input 2"
pub(crate) fn routing_block_name(uri: &str, params: Option<&Vec<f32>>) -> Option<String> {
    let value = |index: usize| params.and_then(|p| p.get(index)).map_or(0.0, |v| *v);
    match uri {
        scanner::SEND_URI => Some(format!("Send {}", value(send::BUS).round() as i32 + 1)),
        scanner::RETURN_URI => Some(format!("Input {}", value(ret::INPUT).round() as i32 + 1)),
        _ => None,
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
                    eq_on: strip.eq.on,
                    limiter_on: strip.limiter.on,
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

/// One line at the top of the Routing view
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
    context: Context,
) -> Timer {
    // The meters have MAX_PORTS slots, the interface may have more ports
    let (inputs, outputs) = (inputs.min(MAX_PORTS), outputs.min(MAX_PORTS));
    let timer = Timer::default();
    let weak = app.as_weak();
    let mut bars = ([0f32; MAX_PORTS], [0f32; MAX_PORTS]);
    let mut shown: Option<(Vec<usize>, Vec<usize>)> = None;
    let mut reductions = [0f32; MAX_PORTS];
    let mut reductions_shown: Option<Vec<i32>> = None;
    app.global::<OutputMaster>()
        .set_reductions(model(vec![0.0; MAX_PORTS]));
    timer.start(TimerMode::Repeated, METER_PERIOD, move || {
        let Some(app) = weak.upgrade() else { return };
        // Read even when hidden, so the peaks do not pile up
        let (input_peaks, output_peaks) = meter.take();
        let deepest = meter.take_reductions();
        if app.get_current_view() != VIEW {
            shown = None;
            reductions_shown = None;
            // The panel does not stay open behind another view
            if app.global::<OutputMaster>().get_edit().shown {
                context.panel(|m| m.port = None);
            }
            return;
        }
        // Gain reduction shows at once and falls back slowly
        let rise = REDUCTION_FALL_DB_PER_S * METER_PERIOD.as_secs_f32();
        for (shown, &db) in reductions.iter_mut().zip(&deepest) {
            *shown = db.min(*shown + rise).min(0.0);
        }
        let tenths: Vec<i32> = reductions
            .iter()
            .map(|r| (r * 10.0).round() as i32)
            .collect();
        if reductions_shown.as_ref() != Some(&tenths) {
            app.global::<OutputMaster>()
                .set_reductions(model(reductions.to_vec()));
            reductions_shown = Some(tenths);
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
