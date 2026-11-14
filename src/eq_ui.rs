//! EQ curve above the parameters of a block read as an EQ (see
//! plugins::view_infer), over the spectrum of the sound going through it
//! (block_sound). The curve follows the editor's values, so it moves with
//! the knobs; dragging a point turns the band's knobs.

use pedalboard::plugins::eq_curve::{Band, BandState, Display, Shape};
use pedalboard::plugins::view_infer;
use pedalboard::state::AppState;

use crate::plugin_view::{self, area, band_handle, freq, line, Cache, Controls, VIEW};
use crate::{App, EqView, PlotHandle};

/// Gain axis of a block EQ: ± 24 dB
const PLOT: EqPlot = EqPlot { range: 24.0 };
/// Points of the curve
const STEPS: usize = 160;

thread_local! {
    static VIEWS: Cache<Display> = Cache::default();
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
    let enabled = display
        .enable
        .as_deref()
        .and_then(value)
        .is_none_or(|v| v >= 0.5);
    let bands: Vec<_> = display
        .bands
        .iter()
        .map(|b| (b.state(&value), b.gain.is_some()))
        .collect();
    Some(PLOT.draw(plugin_view::aspect(), enabled, &bands, |freqs| {
        display.response(&value, freqs)
    }))
}

/// The graph of an EQ: the block EQ's and the output EQ's (routing_ui)
#[derive(Debug, Clone, Copy)]
pub(crate) struct EqPlot {
    /// Gain axis: ± this many dB
    pub range: f32,
}

impl EqPlot {
    /// Height of a gain, 0 to 1 from the bottom
    pub fn up(&self, db: f32) -> f32 {
        (0.5 + db / (2.0 * self.range)).clamp(0.0, 1.0)
    }

    /// Gain at a height, the other way
    pub fn gain(&self, y: f32) -> f32 {
        (y - 0.5) * 2.0 * self.range
    }

    /// The curve `response` gives (dB at each frequency asked), for a plot
    /// `aspect` times wider than high, and a point per band; each band says
    /// whether it has a gain to drag (else its point sits on the curve)
    pub fn draw(
        &self,
        aspect: f32,
        enabled: bool,
        bands: &[(BandState, bool)],
        response: impl FnOnce(&[f32]) -> Vec<f32>,
    ) -> (EqView, Vec<PlotHandle>) {
        let freqs = freq::steps(STEPS);
        let width = VIEW * aspect;
        let xy: Vec<(f32, f32)> = freqs
            .iter()
            .zip(response(&freqs))
            .map(|(f, db)| (freq::x(*f) * width, (1.0 - self.up(db)) * VIEW))
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
        let points = bands
            .iter()
            .enumerate()
            .map(|(i, (s, has_gain))| {
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
                let y = match has_gain {
                    true => self.up(s.gain),
                    false => self.up(s.gain_at(s.freq).min(0.0)),
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
            .collect();
        let eq = EqView {
            shown: true,
            enabled,
            curve: curve.into(),
            fill: fill.into(),
            range: self.range,
        };
        (eq, points)
    }
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
    controls.set(app, band.gain.as_deref(), PLOT.gain(y));
}

/// Switch band `n` on or off
pub(crate) fn toggle(app: &App, state: &AppState, n: usize) {
    let Some((controls, band)) = band(app, state, n) else {
        return;
    };
    let on = band.state(&|s: &str| controls.value(s)).on;
    controls.set(app, band.switch.as_deref(), if on { 0.0 } else { 1.0 });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pedalboard::plugins::eq_curve::QUnit;

    fn band(shape: Shape, freq: f32, gain: f32) -> BandState {
        BandState {
            shape,
            on: true,
            freq,
            gain,
            q: std::f32::consts::FRAC_1_SQRT_2,
            q_unit: QUnit::Q,
        }
    }

    /// The path's points, as (x, y)
    fn coords(path: &str) -> Vec<(f32, f32)> {
        let numbers: Vec<f32> = path
            .split_whitespace()
            .filter_map(|w| w.parse().ok())
            .collect();
        numbers.chunks(2).map(|p| (p[0], p[1])).collect()
    }

    #[test]
    fn gains_map_to_heights_and_back() {
        let plot = EqPlot { range: 18.0 };
        assert_eq!(plot.up(0.0), 0.5);
        assert_eq!(plot.up(18.0), 1.0);
        assert_eq!(plot.up(-40.0), 0.0);
        assert!((plot.gain(plot.up(6.0)) - 6.0).abs() < 1e-5);
        assert_eq!(EqPlot { range: 24.0 }.gain(1.0), 24.0);
    }

    #[test]
    fn a_flat_curve_is_one_segment_and_not_filled() {
        let (eq, _) = PLOT.draw(2.0, true, &[], |f| vec![0.0; f.len()]);
        assert_eq!(coords(&eq.curve), [(0.0, 500.0), (2000.0, 500.0)]);
        assert_eq!(eq.fill, "");
        assert_eq!(eq.range, 24.0);
        assert!(eq.enabled);
    }

    #[test]
    fn a_curve_keeps_its_bends_and_is_filled() {
        let peak = band(Shape::Peak, 1000.0, 12.0);
        let (eq, _) = PLOT.draw(3.0, false, &[(peak, true)], |freqs| {
            freqs.iter().map(|&f| peak.gain_at(f)).collect()
        });
        let curve = coords(&eq.curve);
        // From end to end, near 0 dB at both
        let (first, last) = (curve[0], curve[curve.len() - 1]);
        assert_eq!((first.0, last.0), (0.0, 3000.0));
        assert!((first.1 - 500.0).abs() < 5.0 && (last.1 - 500.0).abs() < 5.0);
        assert!(curve.len() > 10 && curve.len() < STEPS + 1);
        // At 1 kHz, 12 dB up on a ± 24 dB axis: a quarter of the way up
        let top = curve.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        assert!((top - 250.0).abs() < 2.0, "{top}");
        assert!(eq.fill.starts_with("M 0.0 500.0 ") && eq.fill.ends_with(" Z"));
        assert!(!eq.enabled);
    }

    #[test]
    fn points_are_named_for_their_band_and_shape() {
        let shapes = [
            Shape::Highpass,
            Shape::Lowshelf,
            Shape::Peak,
            Shape::Peak,
            Shape::Notch,
            Shape::Bandpass,
            Shape::Highshelf,
            Shape::Lowpass,
        ];
        let bands: Vec<_> = shapes
            .iter()
            .map(|&s| (band(s, 500.0, 6.0), true))
            .collect();
        let (_, points) = PLOT.draw(3.0, true, &bands, |f| vec![0.0; f.len()]);
        let labels: Vec<_> = points.iter().map(|p| p.label.to_string()).collect();
        assert_eq!(labels, ["LC", "LS", "1", "2", "N", "BP", "HS", "HC"]);
        assert_eq!(points[3].name, band_handle(3));
        assert!((points[0].x - freq::x(500.0)).abs() < 1e-6);
    }

    #[test]
    fn a_point_without_gain_sits_on_the_curve() {
        let mut cut = band(Shape::Highpass, 100.0, 0.0);
        let (_, points) = PLOT.draw(3.0, true, &[(cut, false)], |f| vec![0.0; f.len()]);
        // A cookbook high-pass is 3 dB down at its frequency
        assert!(
            (points[0].y - PLOT.up(-3.0)).abs() < 0.01,
            "{}",
            points[0].y
        );
        // A band with a gain shows its gain, and a band switched off is hollow
        let shelf = band(Shape::Lowshelf, 100.0, 12.0);
        cut.on = false;
        let (_, points) = PLOT.draw(3.0, true, &[(shelf, true), (cut, false)], |f| {
            vec![0.0; f.len()]
        });
        assert_eq!(points[0].y, 0.75);
        assert_eq!(points[1].y, 0.5);
        assert!(points[0].on && !points[1].on);
    }
}
