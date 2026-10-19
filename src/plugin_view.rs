//! Shared by the views drawn above a block's parameters (eq_ui, dynamics_ui,
//! scope_ui): the block's controls as the editor shows them, each view read
//! once per plugin, the plot the views are drawn in and their paths. Rust
//! draws, and reads the handles back: the UI only gives positions
//! (PlotEvents), as fractions of the plot from its bottom left.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::LocalKey;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use pedalboard::plugins::scanner::{PluginInfo, PortInfo};
use pedalboard::state::AppState;

use crate::{block_uri, dynamics_ui, eq_ui, id, model, scope_ui, App, PlotEvents};

/// Path coordinates: a plot is VIEW high and VIEW × its aspect wide (a Path
/// keeps the proportions of its box)
pub(crate) const VIEW: f32 = 1000.0;

thread_local! {
    /// Width over height of the wide plot of the view shown (one at a time),
    /// as last laid out
    static ASPECT: Cell<f32> = const { Cell::new(3.0) };
}

/// Width over height of the wide plot
pub(crate) fn aspect() -> f32 {
    ASPECT.get()
}

/// Views read from plugins' ports, by plugin URI
pub(crate) type Cache<T> = RefCell<HashMap<String, Option<T>>>;

/// The selected block's plugin, and the editor's values of its controls
pub(crate) struct Controls {
    pub uri: String,
    pub info: PluginInfo,
    /// Editor row and port of each control input, by symbol: rows are
    /// numbered like the plugin's control inputs
    ports: HashMap<String, (usize, PortInfo)>,
    values: HashMap<usize, f32>,
}

impl Controls {
    pub fn of_selection(app: &App, state: &AppState) -> Option<Self> {
        let instance_id = id(app.get_selected_instance_id())?;
        let uri = block_uri(&state.chain(), instance_id)?;
        let info = state.lv2_host.scanner().find_by_uri(&uri)?.clone();
        let ports = info
            .control_inputs()
            .enumerate()
            .map(|(i, p)| (p.symbol.clone(), (i, p.clone())))
            .collect();
        let values = app
            .get_selected_parameters()
            .iter()
            .map(|p| (p.index as usize, p.value))
            .collect();
        Some(Self {
            uri,
            info,
            ports,
            values,
        })
    }

    /// The plugin's view, read from its ports by `infer` the first time
    pub fn inferred<T: Clone + 'static>(
        &self,
        cache: &'static LocalKey<Cache<T>>,
        infer: fn(&PluginInfo) -> Option<T>,
    ) -> Option<T> {
        cache.with_borrow_mut(|views| {
            views
                .entry(self.uri.clone())
                .or_insert_with(|| infer(&self.info))
                .clone()
        })
    }

    pub fn value(&self, symbol: &str) -> Option<f32> {
        let (i, port) = self.ports.get(symbol)?;
        Some(self.values.get(i).copied().unwrap_or(port.default))
    }

    /// The control exists (a handle on it can be dragged)
    pub fn has(&self, symbol: Option<&str>) -> bool {
        symbol.is_some_and(|s| self.ports.contains_key(s))
    }

    pub fn range(&self, symbol: Option<&str>) -> (f32, f32) {
        symbol
            .and_then(|s| self.ports.get(s))
            .map_or((0.0, 0.0), |(_, p)| (p.min, p.max))
    }

    /// Turn a control to `value`, within its range, as its knob would
    pub fn set(&self, app: &App, symbol: Option<&str>, value: f32) {
        let Some((row, port)) = symbol.and_then(|s| self.ports.get(s)) else {
            return;
        };
        let value = value.clamp(port.min.min(port.max), port.max.max(port.min));
        app.invoke_set_parameter(app.get_selected_instance_id(), *row as i32, value);
    }
}

/// Answer the views' plots and handles
pub(crate) fn setup(app: &App, state: &Arc<AppState>) {
    let events = app.global::<PlotEvents>();
    macro_rules! answer {
        ($setter:ident, |$app:ident, $state:ident $(, $arg:ident)*| $body:expr) => {{
            let (weak, state) = (app.as_weak(), state.clone());
            events.$setter(move |$($arg),*| {
                if let Some($app) = weak.upgrade() {
                    let $state: &AppState = &state;
                    $body;
                }
            });
        }};
    }
    answer!(on_resized, |app, state, w, h| resized(&app, state, w, h));
    answer!(on_drag, |app, state, handle, x, y| drag(
        &app, state, &handle, x, y
    ));
    answer!(on_double_tapped, |app, state, handle| double_tapped(
        &app, state, &handle
    ));
    events.on_released(released);
}

/// Show the view of the selected block: its EQ curve, its level curve, or
/// else what it does to the sound. In that order: the scope is shown only
/// when the plugin has neither curve.
pub(crate) fn refresh(app: &App, state: &AppState) {
    eq_ui::refresh(app, state);
    dynamics_ui::refresh(app, state);
    scope_ui::refresh(app, state);
}

/// The wide plot was laid out at `width` × `height`
pub(crate) fn resized(app: &App, state: &AppState, width: f32, height: f32) {
    if width > 0.0 && height > 0.0 && width / height != ASPECT.get() {
        ASPECT.set(width / height);
        refresh(app, state);
    }
}

/// A handle was dragged to (`x`, `y`)
pub(crate) fn drag(app: &App, state: &AppState, handle: &str, x: f32, y: f32) {
    match handle {
        "threshold" | "ratio" => dynamics_ui::drag(app, state, handle, x, y),
        "time" | "feedback" => scope_ui::drag(app, state, handle, x, y),
        _ => {
            if let Some(band) = band(handle) {
                eq_ui::drag(app, state, band, x, y);
            }
        }
    }
}

/// The handle dragged was let go
pub(crate) fn released() {
    scope_ui::released();
}

/// A handle was double-tapped
pub(crate) fn double_tapped(app: &App, state: &AppState, handle: &str) {
    if let Some(band) = band(handle) {
        eq_ui::toggle(app, state, band);
    }
}

/// `rows` in the model `current`, changed in place when it has as many: a
/// new model re-creates the elements shown from it, and drops a handle being
/// dragged. The same model back means nothing to set.
pub(crate) fn in_place<T: Clone + PartialEq + 'static>(
    current: &ModelRc<T>,
    rows: Vec<T>,
) -> ModelRc<T> {
    match current.as_any().downcast_ref::<VecModel<T>>() {
        Some(shown) if shown.row_count() == rows.len() => {
            for (i, row) in rows.into_iter().enumerate() {
                if shown.row_data(i).as_ref() != Some(&row) {
                    shown.set_row_data(i, row);
                }
            }
            current.clone()
        }
        _ => model(rows),
    }
}

/// Name of the handle of an EQ band, and back
pub(crate) fn band_handle(band: usize) -> String {
    format!("band {band}")
}

fn band(handle: &str) -> Option<usize> {
    handle.strip_prefix("band ")?.parse().ok()
}

/// Line through `points`, in path coordinates
pub(crate) fn line(points: impl IntoIterator<Item = (f32, f32)>) -> String {
    let mut path = String::new();
    for (i, (x, y)) in points.into_iter().enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        path += &format!("{cmd} {x:.1} {y:.1} ");
    }
    path
}

/// Area between the line through `points` and the height `base` (empty
/// without points)
pub(crate) fn area(points: impl IntoIterator<Item = (f32, f32)>, base: f32) -> String {
    let mut path = String::new();
    let mut last = None;
    for (x, y) in points {
        if last.is_none() {
            path += &format!("M {x:.1} {base:.1} ");
        }
        path += &format!("L {x:.1} {y:.1} ");
        last = Some(x);
    }
    match last {
        Some(x) => path + &format!("L {x:.1} {base:.1} Z"),
        None => path,
    }
}

/// Frequency axis of the EQ curve and the spectra, log scale; FreqAxis in
/// plot.slint draws its lines
pub(crate) mod freq {
    const LOW: f32 = 20.0;
    const HIGH: f32 = 20_000.0;

    /// Where `freq` sits across, 0 to 1
    pub fn x(freq: f32) -> f32 {
        ((freq / LOW).ln() / (HIGH / LOW).ln()).clamp(0.0, 1.0)
    }

    /// The frequency at `x` across
    pub fn at(x: f32) -> f32 {
        LOW * (HIGH / LOW).powf(x)
    }

    /// `steps` + 1 frequencies, evenly spread across
    pub fn steps(steps: usize) -> Vec<f32> {
        (0..=steps).map(|i| at(i as f32 / steps as f32)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_join_their_points() {
        assert_eq!(line([(0.0, 1.0), (2.0, 3.0)]), "M 0.0 1.0 L 2.0 3.0 ");
        assert_eq!(
            area([(0.0, 1.0), (2.0, 3.0)], 10.0),
            "M 0.0 10.0 L 0.0 1.0 L 2.0 3.0 L 2.0 10.0 Z"
        );
        assert_eq!(area([], 10.0), "");
    }

    #[test]
    fn the_frequency_axis_goes_both_ways() {
        assert!((freq::at(freq::x(1000.0)) - 1000.0).abs() < 0.1);
        assert_eq!(freq::steps(4).len(), 5);
        assert_eq!(band(&band_handle(3)), Some(3));
    }
}
