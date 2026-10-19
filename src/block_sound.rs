//! The sound going through the selected block, shown by its view while it is
//! (the audio thread only records that block, and only meanwhile): the
//! spectrum in and out, and what the dynamics and scope views measure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use pedalboard::audio::spectrum::{Analyzer, BlockTap, WINDOW};
use pedalboard::state::AppState;

use crate::plugin_view::{self, area, freq, VIEW};
use crate::{dynamics_ui, id, scope_ui, App, Spectrum, EDITOR_VIEW};

/// Frames
const FRAME_MS: u64 = 40;
/// Points of the spectrum
const STEPS: usize = 160;
/// Spectrum levels at the top and bottom of the plot, dB
const TOP: f32 = 0.0;
const BOTTOM: f32 = -84.0;
/// Levels rise this many dB per octave around 1 kHz, so the falling top
/// end of most sounds (pink noise, a guitar) reads about level
const TILT: f32 = 4.5;
/// How fast a level falls back, dB per second (a rise shows at once)
const RELEASE: f32 = 36.0;

/// Block whose sound is shown
struct Watch {
    block: usize,
    /// Samples the tap had written at the last frame
    read: usize,
    /// Samples of this block in the window (the rest is another block's)
    heard: usize,
    at: Instant,
    /// Shown spectrum levels in and out, dB
    input: Vec<f32>,
    output: Vec<f32>,
    dynamics: dynamics_ui::Live,
    scope: scope_ui::Live,
}

/// Show the sound going through the selected block while its view is shown
pub(crate) fn start(app: &App, state: &Arc<AppState>) -> slint::Timer {
    let weak = app.as_weak();
    let state = state.clone();
    let tap = BlockTap::global();
    let mut analyzer = Analyzer::default();
    let (mut input, mut output) = (vec![0.0; WINDOW], vec![0.0; WINDOW]);
    let freqs = freq::steps(STEPS);
    let tilt: Vec<f32> = freqs.iter().map(|f| TILT * (f / 1000.0).log2()).collect();
    let mut shown: Option<Watch> = None;

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(FRAME_MS),
        move || {
            let Some(app) = weak.upgrade() else { return };
            let eq_shown = app.get_selected_eq().shown;
            let dyn_shown = app.get_selected_dyn().shown;
            let scope = app.get_selected_scope();
            let watched = id(app.get_selected_instance_id()).filter(|_| {
                app.get_current_view() == EDITOR_VIEW && (eq_shown || dyn_shown || scope.shown)
            });
            if watched != shown.as_ref().map(|s| s.block) {
                tap.watch(watched);
                shown = watched.map(|block| Watch {
                    block,
                    read: tap.read(&mut input, &mut output),
                    heard: 0,
                    at: Instant::now(),
                    input: vec![BOTTOM; freqs.len()],
                    output: vec![BOTTOM; freqs.len()],
                    dynamics: dynamics_ui::Live::default(),
                    scope: scope_ui::Live::default(),
                });
                app.set_selected_spectrum(Spectrum::default());
                app.set_selected_dyn_live(Default::default());
                app.set_selected_scope_live(Default::default());
            }
            let Some(watch) = shown.as_mut() else {
                return;
            };

            let written = tap.read(&mut input, &mut output);
            let elapsed = watch.at.elapsed().as_secs_f32();
            watch.at = Instant::now();
            // Samples since the last frame (none: no audio running)
            let new = written.wrapping_sub(watch.read).min(WINDOW);
            watch.read = written;
            // What the window held before this block is silence
            watch.heard = (watch.heard + new).min(WINDOW);
            let before = WINDOW - watch.heard;
            input[..before].fill(0.0);
            output[..before].fill(0.0);

            let recent = (&input[WINDOW - new..], &output[WINDOW - new..]);
            if dyn_shown {
                watch.dynamics.frame(&app, &state, tap, recent, elapsed);
            }
            if scope.shown {
                let window = (new > 0).then_some((&input[..], &output[..]));
                watch.scope.frame(&app, recent, window, elapsed);
            }
            if !eq_shown && scope.right != "spectrum" {
                return;
            }
            let fall = RELEASE * elapsed;
            for (levels, samples) in [(&mut watch.input, &input), (&mut watch.output, &output)] {
                let now = match new > 0 {
                    true => analyzer.levels(samples, tap.rate(), &freqs),
                    false => vec![f32::NEG_INFINITY; freqs.len()],
                };
                for ((level, new), tilt) in levels.iter_mut().zip(now).zip(&tilt) {
                    *level = (new + tilt).max(*level - fall).max(BOTTOM);
                }
            }
            let spectrum = Spectrum {
                input: spectrum(&freqs, &watch.input).into(),
                output: spectrum(&freqs, &watch.output).into(),
            };
            if spectrum != app.get_selected_spectrum() {
                app.set_selected_spectrum(spectrum);
            }
        },
    );
    timer
}

/// Path of the area under spectrum `levels` (dB at `freqs`) across the wide
/// plot, empty when they are all at the bottom
fn spectrum(freqs: &[f32], levels: &[f32]) -> String {
    if levels.iter().all(|l| *l <= BOTTOM) {
        return String::new();
    }
    let width = VIEW * plugin_view::aspect();
    let points = freqs.iter().zip(levels).map(|(f, l)| {
        let y = ((TOP - l) / (TOP - BOTTOM)).clamp(0.0, 1.0);
        (freq::x(*f) * width, y * VIEW)
    });
    area(points, VIEW)
}
