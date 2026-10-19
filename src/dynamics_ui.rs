//! Level curve above the parameters of a block read as a compressor, limiter
//! or gate (see plugins::view_infer), with what it does to the sound right
//! now: the sound on the curve, the last seconds in and out, and the gain
//! reduction (the plugin's own reading when it gives one, else measured).

use pedalboard::audio::spectrum::BlockTap;
use pedalboard::plugins::dynamics::{Dynamics, Kind};
use pedalboard::plugins::view_infer;
use pedalboard::state::AppState;

use crate::level_history::{down, up, History, FLOOR};
use crate::plugin_view::{self, line, Cache, Controls, VIEW};
use crate::{App, DynLive, DynView, PlotHandle};

/// Points of the curve
const STEPS: usize = 120;
/// Gain reduction over the whole height of the history and the meter, dB
/// (the meter in dynamics_graph.slint uses the same)
const REDUCTION_SCALE: f32 = 30.0;
/// How fast the shown levels fall back, dB per second (a rise shows at once)
const RELEASE: f32 = 40.0;
/// Steepest ratio the handle sets (a limiter's, for the curve)
const MAX_RATIO: f32 = 100.0;

thread_local! {
    static VIEWS: Cache<Dynamics> = Cache::default();
}

/// The selected block's controls, and its level curve
fn selected(app: &App, state: &AppState) -> Option<(Controls, Dynamics)> {
    let controls = Controls::of_selection(app, state)?;
    let d = controls.inferred(&VIEWS, view_infer::dynamics)?;
    Some((controls, d))
}

/// Show the curve of the selected block, or hide it if it has none
pub(crate) fn refresh(app: &App, state: &AppState) {
    let view = view(app, state).unwrap_or_default();
    if view != app.get_selected_dyn() {
        app.set_selected_dyn(view);
    }
}

/// The ratio control of a compressor
fn ratio_control(d: &Dynamics) -> Option<&str> {
    d.ratio.as_deref().filter(|_| d.kind == Kind::Compressor)
}

fn view(app: &App, state: &AppState) -> Option<DynView> {
    let (controls, d) = selected(app, state)?;
    let value = |symbol: &str| controls.value(symbol);
    let read = |symbol: &Option<String>| symbol.as_deref().and_then(value);

    let curve = line((0..=STEPS).map(|i| {
        let x = i as f32 / STEPS as f32;
        let y = up(d.output(&value, down(x)));
        (x * VIEW, (1.0 - y) * VIEW)
    }));

    let threshold = value(&d.threshold).unwrap_or(0.0);
    // Where the curve bends: the input that reaches the threshold
    let corner = threshold - read(&d.input_gain).unwrap_or(0.0);
    let summary = match d.kind {
        Kind::Compressor => {
            let ratio = match d.ratio(&value).unwrap_or(1.0) {
                r if r.is_finite() => format!("{r:.1}:1"),
                _ => "∞:1".to_string(),
            };
            match read(&d.knee).filter(|k| *k >= 0.5) {
                Some(knee) => format!("{ratio} · knee {knee:.0} dB"),
                None => ratio,
            }
        }
        Kind::Limiter => format!("ceiling {threshold:.1} dB"),
        Kind::Gate => match read(&d.range) {
            Some(range) => format!("range {range:.0} dB"),
            None => String::new(),
        },
    };
    let handle = |name: &str, label: &str, symbol: Option<&str>, x: f32, y: f32| PlotHandle {
        shown: controls.has(symbol),
        name: name.into(),
        label: label.into(),
        on: true,
        x,
        y,
    };
    Some(DynView {
        shown: true,
        enabled: read(&d.enable).is_none_or(|v| v >= 0.5),
        kind: format!("{:?}", d.kind).into(),
        summary: summary.into(),
        curve: curve.into(),
        threshold: handle(
            "threshold",
            "T",
            Some(&d.threshold),
            up(corner),
            up(d.output(&value, corner)),
        ),
        ratio: handle(
            "ratio",
            "R",
            ratio_control(&d),
            1.0,
            up(d.output(&value, 0.0)),
        ),
    })
}

/// The threshold handle dragged across to `x`, or the ratio handle up and
/// down to `y`
pub(crate) fn drag(app: &App, state: &AppState, handle: &str, x: f32, y: f32) {
    let Some((controls, d)) = selected(app, state) else {
        return;
    };
    let value = |symbol: &str| controls.value(symbol);
    if handle == "threshold" {
        // The point is where the input reaches the threshold, after the input gain
        let input_gain = d.input_gain.as_deref().and_then(value).unwrap_or(0.0);
        controls.set(app, Some(&d.threshold), down(x) + input_gain);
        return;
    }
    let Some(ratio) = d.ratio_for(&value, down(y)) else {
        return;
    };
    // Between the ratios the control reaches
    let symbol = ratio_control(&d);
    let (low, high) = controls.range(symbol);
    let ratio_at = |v: f32| d.ratio(&|s: &str| Some(v).filter(|_| Some(s) == symbol));
    let (min, max) = match (ratio_at(low), ratio_at(high)) {
        (Some(a), Some(b)) => (a.min(b), a.max(b).min(MAX_RATIO)),
        _ => (1.0, MAX_RATIO),
    };
    controls.set(app, symbol, d.ratio_value(ratio.clamp(min, max)));
}

/// Levels of the sound going through, frame after frame
#[derive(Default)]
pub(crate) struct Live {
    reduction: f32,
    history: History,
}

impl Live {
    /// Show the samples that went in and out since the last frame,
    /// `elapsed` seconds ago
    pub fn frame(
        &mut self,
        app: &App,
        state: &AppState,
        tap: &BlockTap,
        (input, output): (&[f32], &[f32]),
        elapsed: f32,
    ) {
        let Some((controls, d)) = selected(app, state) else {
            return;
        };
        let read = |symbol: &Option<String>| symbol.as_deref().and_then(|s| controls.value(s));
        let fall = RELEASE * elapsed;
        let (level_in, level_out) = self.history.levels(input, output, fall);
        let active = level_in > FLOOR;

        // The plugin's own reading, else what it took off: out against in,
        // less the gains it adds
        let port = d.reduction.as_deref().and_then(|symbol| {
            let port = controls.info.ports.iter().find(|p| p.symbol == symbol)?;
            tap.control(port.index)
        });
        let reduction = match port {
            Some(v) => -v.abs(),
            None if active => {
                let added = read(&d.input_gain).unwrap_or(0.0) + read(&d.makeup).unwrap_or(0.0);
                (level_out - level_in - added).min(0.0)
            }
            None => 0.0,
        };
        // Reduction shows at once and lets go like the levels
        self.reduction = reduction
            .min(self.reduction + fall)
            .clamp(-REDUCTION_SCALE, 0.0);
        self.history.push(self.reduction);

        let aspect = plugin_view::aspect();
        let live = DynLive {
            active,
            input: up(level_in),
            output: up(level_out),
            reduction: self.reduction,
            reduction_text: format!("{:.1}", self.reduction.min(-0.0)).into(),
            history_in: self.history.area(false, aspect).into(),
            history_out: self.history.area(true, aspect).into(),
            // Hanging from the top
            history_reduction: self
                .history
                .line(aspect, |r| (-r / REDUCTION_SCALE).max(0.001))
                .into(),
        };
        if live != app.get_selected_dyn_live() {
            app.set_selected_dyn_live(live);
        }
    }
}
