//! EQ curve above the parameters of a block read as an EQ (see
//! plugins::view_infer), over the spectrum of the sound going through it
//! (block_sound). The curve follows the editor's values, so it moves with
//! the knobs; dragging a point turns the band's knobs.

use pedalboard::plugins::eq_curve::{Band, Display, Shape};
use pedalboard::plugins::view_infer;
use pedalboard::state::AppState;

use crate::plugin_view::{self, area, band_handle, freq, line, Cache, Controls, VIEW};
use crate::{App, EqView, PlotHandle};

/// Gain axis: ± this many dB
const RANGE: f32 = 24.0;
/// Points of the curve
const STEPS: usize = 160;

thread_local! {
    static VIEWS: Cache<Display> = Cache::default();
}

/// Height of a gain, 0 to 1 from the bottom
fn up(db: f32) -> f32 {
    (0.5 + db / (2.0 * RANGE)).clamp(0.0, 1.0)
}

/// Show the curve of the selected block, or hide it if it has none
pub(crate) fn refresh(app: &App, state: &AppState) {
    let (eq, points) = view(app, state).unwrap_or_default();
    app.set_selected_eq(eq);
    let current = app.get_selected_eq_points();
    let points = plugin_view::in_place(&current, points);
    if points != current {
        app.set_selected_eq_points(points);
    }
}

fn view(app: &App, state: &AppState) -> Option<(EqView, Vec<PlotHandle>)> {
    let controls = Controls::of_selection(app, state)?;
    let display = controls.inferred(&VIEWS, view_infer::eq)?;
    let value = |symbol: &str| controls.value(symbol);

    let freqs = freq::steps(STEPS);
    let response = display.response(&value, &freqs);
    let width = VIEW * plugin_view::aspect();
    let xy: Vec<(f32, f32)> = freqs
        .iter()
        .zip(&response)
        .map(|(f, db)| (freq::x(*f) * width, (1.0 - up(*db)) * VIEW))
        .collect();
    // Flat runs as one segment: the renderer strokes long runs of aligned
    // points unevenly
    let flat = |a: f32, b: f32| (a - b).abs() < 0.05;
    let kept = xy.iter().enumerate().filter(|&(i, (_, y))| {
        let inner = i > 0 && i + 1 < xy.len();
        !(inner && flat(xy[i - 1].1, *y) && flat(*y, xy[i + 1].1))
    });
    let curve = line(kept.map(|(_, p)| *p));
    // Nothing to fill under a flat curve (an empty area is drawn as a seam)
    let fill = match xy.iter().all(|(_, y)| flat(*y, VIEW / 2.0)) {
        true => String::new(),
        false => area(xy, VIEW / 2.0),
    };

    let mut peaks = 0;
    let points = display
        .bands
        .iter()
        .enumerate()
        .map(|(i, band)| {
            let s = band.state(&value);
            let label = match s.shape {
                // Named for what they cut, as on a mixing desk
                Shape::Highpass => "LC".to_string(),
                Shape::Lowpass => "HC".to_string(),
                Shape::Notch => "N".to_string(),
                Shape::Bandpass => "BP".to_string(),
                Shape::Lowshelf => "LS".to_string(),
                Shape::Highshelf => "HS".to_string(),
                Shape::Peak => {
                    peaks += 1;
                    peaks.to_string()
                }
            };
            // A cut has no gain: its point sits on the curve
            let y = match band.gain {
                Some(_) => up(s.gain),
                None => up(s.gain_at(s.freq).min(0.0)),
            };
            PlotHandle {
                shown: true,
                name: band_handle(i).into(),
                label: label.into(),
                on: s.on,
                x: freq::x(s.freq),
                y,
            }
        })
        .collect::<Vec<_>>();
    let enabled = display
        .enable
        .as_deref()
        .and_then(value)
        .is_none_or(|v| v >= 0.5);
    let eq = EqView {
        shown: true,
        enabled,
        curve: curve.into(),
        fill: fill.into(),
        range: RANGE,
    };
    Some((eq, points))
}

/// The selected block's controls, and its band `band`
fn band(app: &App, state: &AppState, band: usize) -> Option<(Controls, Band)> {
    let controls = Controls::of_selection(app, state)?;
    let band = controls
        .inferred(&VIEWS, view_infer::eq)?
        .bands
        .get(band)?
        .clone();
    Some((controls, band))
}

/// The point of band `n` dragged to (`x`, `y`): its frequency and gain (a
/// fixed frequency, as on a graphic EQ, does not move); a band that is off
/// is switched on
pub(crate) fn drag(app: &App, state: &AppState, n: usize, x: f32, y: f32) {
    let Some((controls, band)) = band(app, state, n) else {
        return;
    };
    if !band.state(&|s: &str| controls.value(s)).on {
        controls.set(app, band.switch.as_deref(), 1.0);
    }
    controls.set(app, band.freq.control(), freq::at(x) / band.freq_scale);
    controls.set(app, band.gain.as_deref(), (y - 0.5) * 2.0 * RANGE);
}

/// Switch band `n` on or off
pub(crate) fn toggle(app: &App, state: &AppState, n: usize) {
    let Some((controls, band)) = band(app, state, n) else {
        return;
    };
    let on = band.state(&|s: &str| controls.value(s)).on;
    controls.set(app, band.switch.as_deref(), if on { 0.0 } else { 1.0 });
}
