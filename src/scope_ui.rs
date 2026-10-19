//! What a delay, reverb, drive, modulation or cabinet block does to the sound
//! (see plugins::scope), above its parameters: a delay's echoes from its
//! controls (dragged to set them), and measured on what goes through
//! (block_sound), the level in and out over the last seconds, the spectrum,
//! or the waves in and out.

use std::cell::Cell;

use pedalboard::audio::spectrum;
use pedalboard::plugins::scope::{Echoes, Scope, ScopeKind};
use pedalboard::plugins::view_infer;
use pedalboard::state::AppState;

use crate::level_history::{amplitude_up, down, History};
use crate::plugin_view::{self, line, Cache, Controls, VIEW};
use crate::{App, EchoTap, PlotHandle, ScopeLive, ScopeView};

/// How fast the shown levels fall back, dB per second: fast, so a reverb's
/// tail or a delay's echoes show as they are, not as a meter falls
const RELEASE: f32 = 300.0;
/// Samples of the waves shown (20 ms at 48 kHz: two periods of a low A),
/// and the stretches they are drawn in
const SPAN: usize = 960;
const POINTS: usize = 240;
/// Room left and right of the echoes, in path coordinates
const MARGIN: f32 = 40.0;

thread_local! {
    static VIEWS: Cache<Scope> = Cache::default();
    /// Time across the echoes when the drag of the first began: it is
    /// dragged against that, as the plot follows the time
    static HELD_SPAN: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Show what the selected block does, if it has no EQ or level curve
pub(crate) fn refresh(app: &App, state: &AppState) {
    let current = app.get_selected_scope();
    let (mut view, taps) = view(app, state).unwrap_or_default();
    view.echo_taps = plugin_view::in_place(&current.echo_taps, taps);
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
        })
    }
}

/// Where `seconds` sits across a plot `span` seconds wide, 0 to 1, and back
fn x(seconds: f32, span: f32) -> f32 {
    (MARGIN + (VIEW - 2.0 * MARGIN) * seconds / span) / VIEW
}

fn seconds_at(x: f32, span: f32) -> f32 {
    (x * VIEW - MARGIN) / (VIEW - 2.0 * MARGIN) * span
}

fn view(app: &App, state: &AppState) -> Option<(ScopeView, Vec<EchoTap>)> {
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
            view.left = "wave".into();
            view.right = "spectrum".into();
        }
        ScopeKind::Modulation | ScopeKind::Cabinet => view.right = "spectrum".into(),
    }
    let Some(echoes) = scope.echoes else {
        return Some((view, Vec::new()));
    };
    let Some(layout) = Layout::of(&echoes, &value) else {
        view.summary = if echoes.synced(&value) {
            "tempo sync"
        } else {
            "no delay"
        }
        .into();
        return Some((view, Vec::new()));
    };
    let Layout { time, first, .. } = layout;
    view.left = "history".into();
    view.right = "echoes".into();
    let taps = layout
        .taps
        .iter()
        .map(|&(t, level)| EchoTap {
            x: x(t, layout.span),
            height: amplitude_up(level),
        })
        .collect();
    view.echo_span = seconds(layout.span).into();
    view.echo_time = PlotHandle {
        shown: true,
        name: "time".into(),
        label: "T".into(),
        on: true,
        x: x(time, layout.span),
        y: amplitude_up(first),
    };
    view.echo_feedback = PlotHandle {
        shown: echoes.feedback.is_some() && first > 1e-3,
        name: "feedback".into(),
        label: "F".into(),
        on: true,
        x: x(2.0 * time, layout.span),
        y: amplitude_up(first * echoes.feedback(&value)),
    };
    let count = layout.taps.len() - 1;
    view.summary = format!(
        "{} · {count} echo{}",
        seconds(time),
        if count == 1 { "" } else { "es" }
    )
    .into();
    Some((view, taps))
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
        let amount = match y > 0.0 {
            true => 10f32.powf(down(y) / 20.0) / layout.first,
            false => 0.0,
        };
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
    /// `elapsed` seconds ago, and the last window of them (`window`, `None`
    /// if nothing new went through)
    pub fn frame(
        &mut self,
        app: &App,
        recent: (&[f32], &[f32]),
        window: Option<(&[f32], &[f32])>,
        elapsed: f32,
    ) {
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
        if view.left == "wave" {
            if let Some(w) = window.and_then(|(i, o)| spectrum::waves(i, o, SPAN, POINTS)) {
                live.wave_in = wave(&w.input).into();
                live.wave_out = wave(&w.output).into();
                live.gain = format!("{:+.1} dB", w.gain).into();
            }
        }
        if live != app.get_selected_scope_live() {
            app.set_selected_scope_live(live);
        }
    }
}

/// Line through the lowest and highest sample of each stretch of a wave,
/// across the square
fn wave(stretches: &[(f32, f32)]) -> String {
    let y = |v: f32| (1.0 - (v.clamp(-1.0, 1.0) + 1.0) / 2.0) * VIEW;
    let last = stretches.len().saturating_sub(1).max(1) as f32;
    line(stretches.iter().enumerate().flat_map(|(i, (low, high))| {
        let x = i as f32 / last * VIEW;
        [(x, y(*low)), (x, y(*high))]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_axis_goes_both_ways() {
        assert!((seconds_at(x(0.25, 1.0), 1.0) - 0.25).abs() < 1e-5);
    }
}
