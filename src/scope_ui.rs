//! What a delay, reverb, drive, modulation or cabinet block does to the sound
//! (see plugins::scope), above its parameters: a delay's echoes from its
//! controls (dragged to set them), a drive's response at its settings
//! (plugins::response, measured on an instance of its own), and measured on
//! what goes through (block_sound), the level in and out over the last
//! seconds or the spectrum.

use std::cell::{Cell, RefCell};

use pedalboard::plugins::response::{self, Probe};
use pedalboard::plugins::scope::{Echoes, Scope, ScopeKind};
use pedalboard::plugins::view_infer;
use pedalboard::state::AppState;
use slint::ComponentHandle;

use crate::level_history::{amplitude_up, down, History};
use crate::plugin_view::{self, freq, line, Cache, Controls, VIEW};
use crate::{App, EchoBeat, EchoTap, PlotHandle, ScopeLive, ScopeView};

/// How fast the shown levels fall back, dB per second: fast, so a reverb's
/// tail or a delay's echoes show as they are, not as a meter falls
const RELEASE: f32 = 300.0;
/// Room left and right of the echoes, in path coordinates
const MARGIN: f32 = 40.0;
/// Steps of a response across the plot, and the gains it spans, dB
const RESPONSE_STEPS: usize = 120;
const RESPONSE_RANGE: f32 = 48.0;

/// A plugin at the values of its parameters (by index), as measured
#[derive(Clone, PartialEq)]
struct Setting {
    uri: String,
    values: Vec<(usize, u32)>,
}

thread_local! {
    static VIEWS: Cache<Scope> = Cache::default();
    /// Time across the echoes when the drag of the first began: it is
    /// dragged against that, as the plot follows the time
    static HELD_SPAN: Cell<Option<f32>> = const { Cell::new(None) };
    /// The last response asked for, and the last measured (gains in dB at
    /// `freq::steps(RESPONSE_STEPS)`)
    static ASKED: RefCell<Option<Setting>> = const { RefCell::new(None) };
    static MEASURED: RefCell<Option<(Setting, Vec<f32>)>> = const { RefCell::new(None) };
}

/// Show what the selected block does, if it has no EQ or level curve
pub(crate) fn refresh(app: &App, state: &AppState) {
    let current = app.get_selected_scope();
    let (mut view, taps, beats) = view(app, state).unwrap_or_default();
    view.echo_taps = plugin_view::in_place(&current.echo_taps, taps);
    view.echo_beats = plugin_view::in_place(&current.echo_beats, beats);
    if view != current {
        app.set_selected_scope(view);
    }
}

/// A delay's echoes as drawn: the dry sound and each echo (seconds,
/// amplitude), the time between echoes, the time across the plot (at least
/// two echoes, where the feedback would put the second, and half a delay
/// after the last) and the amplitude of the first echo; none while synced
/// to the tempo
struct Layout {
    taps: Vec<(f32, f32)>,
    time: f32,
    span: f32,
    first: f32,
    /// Ping-pong: the echoes take the left (top) and right (bottom) lanes
    /// in turn
    lanes: bool,
}

impl Layout {
    fn of(echoes: &Echoes, value: &impl Fn(&str) -> Option<f32>) -> Option<Self> {
        let taps = echoes.taps(value)?;
        let time = echoes.seconds(value)?;
        let span = taps.last().map_or(time, |t| t.0).max(2.0 * time) + time / 2.0;
        let first = taps.get(1).map_or(0.0, |t| t.1);
        Some(Self {
            taps,
            time,
            span,
            first,
            lanes: echoes.ping_pong(value),
        })
    }

    /// Where tap `i` stands and how much of the height it has, 0 to 1 from
    /// the bottom: all of it, or with lanes, the top half for the odd
    /// echoes (left) and the bottom half for the even ones (right); the dry
    /// sound is on both
    fn lane(&self, i: usize) -> (f32, f32) {
        match (self.lanes, i) {
            (false, _) | (true, 0) => (0.0, 1.0),
            (true, i) if i % 2 == 1 => (0.5, 0.5),
            (true, _) => (0.0, 0.5),
        }
    }

    /// Height of tap `i` at `amplitude`, 0 to 1 from the bottom of the plot
    fn height(&self, i: usize, amplitude: f32) -> f32 {
        let (base, scale) = self.lane(i);
        base + scale * amplitude_up(amplitude)
    }

    /// Amplitude of tap `i` at height `y` (the reverse of `height`)
    fn amplitude(&self, i: usize, y: f32) -> f32 {
        let (base, scale) = self.lane(i);
        let up = (y - base) / scale;
        match up > 0.0 {
            true => 10f32.powf(down(up) / 20.0),
            false => 0.0,
        }
    }
}

/// Beats drawn behind the echoes: none when they would be too close
const MOST_BEATS: usize = 48;

/// The tempo's beats across a plot `span` seconds wide (4/4 bars)
fn beats(bpm: f32, span: f32) -> Vec<EchoBeat> {
    if !bpm.is_finite() || !span.is_finite() || bpm <= 0.0 || span <= 0.0 {
        return Vec::new();
    }
    let beat = 60.0 / bpm;
    let count = (span / beat).floor() as usize;
    if count > MOST_BEATS {
        return Vec::new();
    }
    (1..=count)
        .map(|k| EchoBeat {
            x: x(k as f32 * beat, span),
            bar: k % 4 == 0,
        })
        .collect()
}

/// Where `seconds` sits across a plot `span` seconds wide, 0 to 1, and back
fn x(seconds: f32, span: f32) -> f32 {
    (MARGIN + (VIEW - 2.0 * MARGIN) * seconds / span) / VIEW
}

fn seconds_at(x: f32, span: f32) -> f32 {
    (x * VIEW - MARGIN) / (VIEW - 2.0 * MARGIN) * span
}

/// The view, the dry sound and echoes, and the beats behind them
fn view(app: &App, state: &AppState) -> Option<(ScopeView, Vec<EchoTap>, Vec<EchoBeat>)> {
    if app.get_selected_eq().shown || app.get_selected_dyn().shown {
        return None;
    }
    let controls = Controls::of_selection(app, state)?;
    let scope = controls.inferred(&VIEWS, view_infer::scope)?;
    let value = |symbol: &str| controls.value(symbol);
    let mut view = ScopeView {
        shown: true,
        kind: format!("{:?}", scope.kind).into(),
        ..Default::default()
    };
    match scope.kind {
        ScopeKind::Delay | ScopeKind::Reverb => view.right = "history".into(),
        ScopeKind::Drive => {
            view.right = "spectrum".into();
            if let Some((path, summary)) = drawn_response(app, state, &controls) {
                view.response = path.into();
                view.summary = summary.into();
            }
        }
        ScopeKind::Modulation | ScopeKind::Cabinet => view.right = "spectrum".into(),
    }
    let Some(echoes) = scope.echoes else {
        return Some((view, Vec::new(), Vec::new()));
    };
    let Some(layout) = Layout::of(&echoes, &value) else {
        view.summary = if echoes.synced(&value) {
            "tempo sync"
        } else {
            "no delay"
        }
        .into();
        return Some((view, Vec::new(), Vec::new()));
    };
    let Layout { time, first, .. } = layout;
    view.left = "history".into();
    view.right = "echoes".into();
    let taps = layout
        .taps
        .iter()
        .enumerate()
        .map(|(i, &(t, level))| EchoTap {
            x: x(t, layout.span),
            base: layout.lane(i).0,
            height: layout.height(i, level) - layout.lane(i).0,
        })
        .collect();
    view.echo_lanes = layout.lanes;
    view.echo_span = seconds(layout.span).into();
    view.echo_time = PlotHandle {
        shown: true,
        name: "time".into(),
        label: "T".into(),
        on: true,
        x: x(time, layout.span),
        y: layout.height(1, first),
    };
    view.echo_feedback = PlotHandle {
        shown: echoes.feedback.is_some() && first > 1e-3,
        name: "feedback".into(),
        label: "F".into(),
        on: true,
        x: x(2.0 * time, layout.span),
        y: layout.height(2, first * echoes.feedback(&value)),
    };
    let count = layout.taps.len().saturating_sub(1);
    view.summary = format!(
        "{} · {count} echo{}",
        seconds(time),
        if count == 1 { "" } else { "es" }
    )
    .into();
    Some((view, taps, beats(state.tempo(), layout.span)))
}

/// The response of the selected block at its settings, as a line across the
/// wide plot (the loudest gain near the top) and what it says ("+18 dB at
/// 720 Hz"); measured first if not yet, then drawn when it comes
fn drawn_response(app: &App, state: &AppState, controls: &Controls) -> Option<(String, String)> {
    if !response::measurable(&controls.uri) {
        return None;
    }
    let values = controls.values();
    let setting = Setting {
        uri: controls.uri.clone(),
        values: values.iter().map(|&(i, v)| (i, v.to_bits())).collect(),
    };
    let measured = MEASURED.with_borrow(|m| {
        m.as_ref()
            .filter(|(s, _)| *s == setting)
            .map(|(_, gains)| gains.clone())
    });
    if measured.is_none() && ASKED.with_borrow(|a| a.as_ref() != Some(&setting)) {
        ask(app, state, setting, values);
    }
    Some(drawn(&measured?)).flatten()
}

/// A response (gains in dB at `freq::steps(RESPONSE_STEPS)`) as a line
/// across the wide plot, its loudest gain near the top, and what it says
/// ("+18 dB at 720 Hz")
fn drawn(gains: &[f32]) -> Option<(String, String)> {
    let freqs = freq::steps(RESPONSE_STEPS);
    let (peak, at) = gains
        .iter()
        .zip(&freqs)
        .fold((f32::NEG_INFINITY, 0.0), |best, (&g, &f)| {
            if g > best.0 {
                (g, f)
            } else {
                best
            }
        });
    if !peak.is_finite() {
        return None;
    }
    // The loudest gain an eighth of the range from the top, on 6 dB steps
    let top = ((peak + RESPONSE_RANGE / 8.0) / 6.0).ceil() * 6.0;
    let width = VIEW * plugin_view::aspect();
    let path = line(gains.iter().zip(&freqs).map(|(&g, &f)| {
        let y = ((top - g) / RESPONSE_RANGE).clamp(0.0, 1.0) * VIEW;
        (freq::x(f) * width, y)
    }));
    Some((path, format!("{peak:+.0} dB at {}", hertz(at))))
}

/// Measure the response of `setting` on the chain builder; the view is drawn
/// again when it comes
fn ask(app: &App, state: &AppState, setting: Setting, values: Vec<(usize, f32)>) {
    let weak = app.as_weak();
    let measured = setting.clone();
    let probe = Probe {
        uri: setting.uri.clone(),
        values,
        freqs: freq::steps(RESPONSE_STEPS),
        done: Box::new(move |gains| {
            let Some(gains) = gains else { return };
            crate::ui(&weak, move |app| {
                // Drawn if still the one shown
                let shown = ASKED.with_borrow(|a| a.as_ref() == Some(&measured));
                let drawn = drawn(&gains);
                MEASURED.set(Some((measured, gains)));
                let mut view = app.get_selected_scope();
                if let (true, Some((path, summary))) = (shown && view.shown, drawn) {
                    view.response = path.into();
                    view.summary = summary.into();
                    app.set_selected_scope(view);
                }
            });
        }),
    };
    if state.measure_response(probe) {
        ASKED.set(Some(setting));
    }
}

/// "720 Hz", "2.4 kHz"
fn hertz(f: f32) -> String {
    if f < 1000.0 {
        format!("{f:.0} Hz")
    } else {
        format!("{:.1} kHz", f / 1000.0)
    }
}

/// A delay's first echo dragged across to `x` (its time), or its second up
/// and down to `y` (the feedback, against the first)
pub(crate) fn drag(app: &App, state: &AppState, handle: &str, x: f32, y: f32) {
    let Some(controls) = Controls::of_selection(app, state) else {
        return;
    };
    let Some(echoes) = controls
        .inferred(&VIEWS, view_infer::scope)
        .and_then(|s| s.echoes)
    else {
        return;
    };
    let value = |symbol: &str| controls.value(symbol);
    let Some(layout) = Layout::of(&echoes, &value) else {
        return;
    };
    if handle == "time" {
        let span = HELD_SPAN.get().unwrap_or(layout.span);
        HELD_SPAN.set(Some(span));
        let time = seconds_at(x, span).max(0.001);
        controls.set(app, Some(&echoes.time), echoes.time_value(time));
    } else if let Some(feedback) = &echoes.feedback {
        let amount = layout.amplitude(2, y) / layout.first;
        controls.set(
            app,
            Some(&feedback.symbol),
            feedback.value(amount.clamp(0.0, 0.99)),
        );
    }
}

/// The handle dragged was let go
pub(crate) fn released() {
    HELD_SPAN.set(None);
}

/// "250 ms", "1.2 s"
fn seconds(t: f32) -> String {
    if t < 1.0 {
        format!("{:.0} ms", t * 1000.0)
    } else {
        format!("{t:.1} s")
    }
}

/// What went through, frame after frame
#[derive(Default)]
pub(crate) struct Live {
    history: History,
}

impl Live {
    /// Show the samples that went in and out since the last frame (`recent`),
    /// `elapsed` seconds ago
    pub fn frame(&mut self, app: &App, recent: (&[f32], &[f32]), elapsed: f32) {
        let view = app.get_selected_scope();
        let mut live = ScopeLive::default();
        if view.right == "history" || view.left == "history" {
            self.history.levels(recent.0, recent.1, RELEASE * elapsed);
            self.history.push(0.0);
            // In the square, else across
            let aspect = match view.left == "history" {
                true => 1.0,
                false => plugin_view::aspect(),
            };
            live.history_in = self.history.area(false, aspect).into();
            live.history_out = self.history.area(true, aspect).into();
        }
        if live != app.get_selected_scope_live() {
            app.set_selected_scope_live(live);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_axis_goes_both_ways() {
        assert!((seconds_at(x(0.25, 1.0), 1.0) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn beats_follow_the_tempo() {
        // 120 BPM over 2.1 s: four beats, the fourth starts a bar
        let b = beats(120.0, 2.1);
        assert_eq!(b.len(), 4);
        assert!(b.windows(2).all(|w| w[0].x < w[1].x));
        assert_eq!(b.iter().filter(|b| b.bar).count(), 1);
        assert!(b.last().is_some_and(|b| b.bar));
        // Too many, or no tempo: none
        assert!(beats(300.0, 60.0).is_empty());
        assert!(beats(0.0, 2.0).is_empty());
        assert!(beats(f32::NAN, 2.0).is_empty());
    }
}
