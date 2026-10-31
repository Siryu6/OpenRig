//! The rack's knob strip driven by external encoders (I2C boards, or the
//! keyboard): encoder k turns the k-th knob of the strip's page shown, a
//! 6th encoder turns the pages.
//!
//! - knob: turning moves it, pressing swaps it with its value before the
//!   turns (A/B), holding puts it back to its default;
//! - switch: turning or pressing flips it, holding puts it back;
//! - list: pressing opens it, turning then moves through it and pressing
//!   picks; closed, turning steps through it and holding puts it back;
//! - factory presets and model, after the parameters: a press or a turn
//!   opens their list, a press picks, holding closes it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::App;
use pedalboard::ui::encoder;

/// Knobs on a page of the strip (the rack's `per-page`)
const PER_PAGE: i32 = 5;
const TURN: i32 = 0;
const PRESS: i32 = 1;
const LONG_PRESS: i32 = 2;
/// `strip-list-row` of the list fields (the rack's)
const FACTORY_ROW: i32 = -2;
const MODEL_ROW: i32 = -3;

/// A strip control and what an encoder event does to it
struct Control<'a> {
    kind: &'a str,
    min: f32,
    max: f32,
    default: f32,
    value: f32,
    options: &'a [f32],
}

#[derive(Debug, Default, PartialEq)]
struct Response {
    value: Option<f32>,
    list: List,
    /// The value before the turns: kept, set or forgotten
    before: Before,
}

#[derive(Debug, Default, PartialEq)]
enum List {
    #[default]
    Keep,
    /// Opens it on, or moves to, an option
    Focus(i32),
    Close,
}

#[derive(Debug, Default, PartialEq)]
enum Before {
    #[default]
    Keep,
    Set(f32),
    Forget,
}

impl Control<'_> {
    /// The option the value is on
    fn current(&self) -> i32 {
        self.options
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1 - self.value)
                    .abs()
                    .total_cmp(&(b.1 - self.value).abs())
            })
            .map_or(0, |(i, _)| i as i32)
    }

    /// `focus`: the option the open list is on (-1: the current one), None
    /// when the list is closed; `before`: the value before the turns
    fn respond(
        &self,
        action: i32,
        detents: i32,
        focus: Option<i32>,
        before: Option<f32>,
    ) -> Response {
        let step = |detents| {
            encoder::step(
                self.kind,
                self.min,
                self.max,
                self.value,
                self.options,
                detents,
            )
        };
        let set = |value: f32| Response {
            value: Some(value),
            ..Response::default()
        };
        let list = self.kind == "enum" && !self.options.is_empty();
        let last = self.options.len() as i32 - 1;
        let focus = focus.map(|f| if f < 0 { self.current() } else { f.min(last) });
        match (action, focus) {
            (TURN, Some(focus)) if list => Response {
                list: List::Focus((focus + detents).clamp(0, last)),
                ..Response::default()
            },
            (PRESS, Some(focus)) if list => Response {
                value: self.options.get(focus as usize).copied(),
                list: List::Close,
                ..Response::default()
            },
            (PRESS, None) if list => Response {
                list: List::Focus(self.current()),
                ..Response::default()
            },
            (LONG_PRESS, Some(_)) => Response {
                list: List::Close,
                ..Response::default()
            },
            (TURN, _) => Response {
                value: Some(step(detents)),
                list: List::Keep,
                before: if before.is_none() && self.kind != "toggle" {
                    Before::Set(self.value)
                } else {
                    Before::Keep
                },
            },
            (PRESS, _) if self.kind == "toggle" => set(step(1)),
            (PRESS, _) => match before {
                Some(before) => Response {
                    value: Some(before),
                    list: List::Keep,
                    before: Before::Set(self.value),
                },
                None => Response::default(),
            },
            (LONG_PRESS, _) => Response {
                value: Some(self.default),
                list: List::Keep,
                before: Before::Forget,
            },
            _ => Response::default(),
        }
    }
}

pub fn setup(app: &App) {
    // Value before the turns, by instance and parameter index
    let before: Rc<RefCell<HashMap<(i32, i32), f32>>> = Rc::default();
    let weak = app.as_weak();
    app.on_strip_encoder(move |encoder, action, detents| {
        if let Some(app) = weak.upgrade() {
            event(&app, &mut before.borrow_mut(), encoder, action, detents);
        }
    });
}

fn event(
    app: &App,
    before: &mut HashMap<(i32, i32), f32>,
    encoder: i32,
    action: i32,
    detents: i32,
) {
    let instance = app.get_selected_instance_id();
    if app.get_current_view() != 0 || instance < 0 {
        return;
    }
    // The parameters in the strip, then the list fields
    let strip_count = app.get_selected_strip_count();
    let factory = app.get_selected_strip_factory().row_count() as i32;
    let models = app.get_selected_strip_models().row_count() as i32;
    let factory_pos = (factory > 0).then_some(strip_count);
    let model_pos = app
        .get_selected_uses_file()
        .then_some(strip_count + i32::from(factory > 0));
    let fields = strip_count + i32::from(factory_pos.is_some()) + i32::from(model_pos.is_some());
    let pages = ((fields + PER_PAGE - 1) / PER_PAGE).max(1);
    let page = app.get_strip_page().clamp(0, pages - 1);
    if encoder == PER_PAGE {
        let next = (page + detents).clamp(0, pages - 1);
        if action == TURN && next != page {
            app.set_strip_list_row(-1);
            app.set_strip_page(next);
        }
        return;
    }
    if !(0..PER_PAGE).contains(&encoder) {
        return;
    }
    let slot = page * PER_PAGE + encoder;
    let list_row = app.get_strip_list_row();
    if Some(slot) == factory_pos || Some(slot) == model_pos {
        let row = if Some(slot) == factory_pos {
            FACTORY_ROW
        } else {
            MODEL_ROW
        };
        if list_row != -1 && list_row != row {
            app.set_strip_list_row(-1);
        }
        if row == MODEL_ROW && models <= 1 {
            // Nothing to list: a press opens the models
            if action == PRESS {
                app.invoke_strip_model_picked(0);
            }
            return;
        }
        let count = if row == FACTORY_ROW { factory } else { models };
        let current = if row == MODEL_ROW {
            app.get_selected_strip_model_current()
        } else {
            -1
        };
        let open = (list_row == row).then(|| app.get_strip_list_focus());
        match list_field(count, current, action, detents, open) {
            List::Keep => {}
            List::Focus(option) => {
                app.set_strip_list_focus(option);
                app.set_strip_list_row(row);
            }
            List::Close => app.set_strip_list_row(-1),
        }
        if let (Some(focus), PRESS) = (open, action) {
            let picked = if focus < 0 { current.max(0) } else { focus };
            if row == FACTORY_ROW {
                app.invoke_strip_factory_picked(picked);
            } else {
                app.invoke_strip_model_picked(picked);
            }
        }
        return;
    }
    let params = app.get_selected_parameters();
    let Some((row, p)) = params.iter().enumerate().find(|(_, p)| p.strip_pos == slot) else {
        return;
    };
    let row = row as i32;
    // Another control closes the list
    if list_row != -1 && list_row != row {
        app.set_strip_list_row(-1);
    }
    let options: Vec<f32> = p.option_values.iter().collect();
    let control = Control {
        kind: &p.kind,
        min: p.min,
        max: p.max,
        default: p.default,
        value: p.value,
        options: &options,
    };
    let key = (instance, p.index);
    let open = (list_row == row).then(|| app.get_strip_list_focus());
    let response = control.respond(action, detents, open, before.get(&key).copied());
    match response.before {
        Before::Keep => {}
        Before::Set(value) => {
            before.insert(key, value);
        }
        Before::Forget => {
            before.remove(&key);
        }
    }
    match response.list {
        List::Keep => {}
        List::Focus(option) => {
            app.set_strip_list_focus(option);
            app.set_strip_list_row(row);
        }
        List::Close => app.set_strip_list_row(-1),
    }
    if let Some(value) = response.value {
        app.invoke_set_parameter(instance, p.index, value);
    }
}

/// What an encoder does to a list field (factory presets, model) of
/// `count` rows, `current` the one loaded (-1: none): a press or a turn
/// opens it, then the turns move and a press picks (the caller loads it),
/// holding closes it. `open`: the focused row (-1: none yet), None when
/// closed
fn list_field(count: i32, current: i32, action: i32, detents: i32, open: Option<i32>) -> List {
    let last = (count - 1).max(0);
    match (action, open) {
        (TURN, Some(focus)) => {
            let from = if focus < 0 { current.max(0) } else { focus };
            List::Focus((from + detents).clamp(0, last))
        }
        (PRESS | LONG_PRESS, Some(_)) => List::Close,
        (TURN | PRESS, None) => List::Focus(current.clamp(0, last)),
        _ => List::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: [f32; 3] = [0.0, 1.0, 2.0];

    fn knob(value: f32) -> Control<'static> {
        Control {
            kind: "float",
            min: 0.0,
            max: 10.0,
            default: 5.0,
            value,
            options: &[],
        }
    }

    fn list(value: f32) -> Control<'static> {
        Control {
            kind: "enum",
            min: 0.0,
            max: 2.0,
            default: 0.0,
            value,
            options: &OPTIONS,
        }
    }

    #[test]
    fn a_knob_turns_swaps_with_the_value_before_and_resets() {
        let turned = knob(5.0).respond(TURN, 2, None, None);
        assert_eq!(turned.value, Some(5.2));
        assert_eq!(turned.before, Before::Set(5.0));
        // Turned again: the first value before stays
        assert_eq!(
            knob(5.2).respond(TURN, 1, None, Some(5.0)).before,
            Before::Keep
        );
        let swapped = knob(6.0).respond(PRESS, 0, None, Some(5.0));
        assert_eq!(
            (swapped.value, swapped.before),
            (Some(5.0), Before::Set(6.0))
        );
        // Never turned: nothing to swap with
        assert_eq!(knob(6.0).respond(PRESS, 0, None, None), Response::default());
        let reset = knob(6.0).respond(LONG_PRESS, 0, None, Some(5.0));
        assert_eq!((reset.value, reset.before), (Some(5.0), Before::Forget));
    }

    #[test]
    fn a_switch_flips() {
        let switch = Control {
            kind: "toggle",
            max: 1.0,
            default: 0.0,
            ..knob(0.0)
        };
        assert_eq!(switch.respond(PRESS, 0, None, None).value, Some(1.0));
        assert_eq!(switch.respond(TURN, -1, None, None).value, Some(1.0));
        assert_eq!(switch.respond(TURN, -1, None, None).before, Before::Keep);
    }

    #[test]
    fn a_list_opens_moves_and_picks() {
        let opened = list(1.0).respond(PRESS, 0, None, None);
        assert_eq!((opened.value, opened.list), (None, List::Focus(1)));
        // Open, from the current option or the focused one
        assert_eq!(
            list(1.0).respond(TURN, 1, Some(-1), None).list,
            List::Focus(2)
        );
        assert_eq!(
            list(1.0).respond(TURN, 5, Some(1), None).list,
            List::Focus(2)
        );
        let picked = list(1.0).respond(PRESS, 0, Some(0), None);
        assert_eq!((picked.value, picked.list), (Some(0.0), List::Close));
        assert_eq!(
            list(1.0).respond(LONG_PRESS, 0, Some(2), None).list,
            List::Close
        );
        // Closed: turning steps through it, holding resets it
        assert_eq!(list(1.0).respond(TURN, -1, None, None).value, Some(0.0));
        assert_eq!(
            list(2.0).respond(LONG_PRESS, 0, None, None).value,
            Some(0.0)
        );
    }
}
