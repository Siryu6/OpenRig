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
//!
//! Pots (M5Stack 8Angle, [`pot`]) set the parameter at their place instead,
//! once they reach its value ([`pedalboard::ui::pots`]); the 6th pot picks
//! the page; the board's switch turns the block shown on or off. Its LEDs
//! say which pots drive their parameter ([`pot_leds`]). The factory presets
//! and model fields are left to touch and encoders: a pot sweeping through
//! them would load every file on its way.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::{App, View};
use pedalboard::ui::encoder;
#[cfg(feature = "gpio")]
use pedalboard::ui::pots;

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
            event(&app, &before, encoder, action, detents);
        }
    });
}

/// `before` is only borrowed for a moment: the callbacks invoked here may
/// come back to this one
/// The strip of the block shown on the rack page
struct Strip {
    instance: i32,
    page: i32,
    pages: i32,
    factory_pos: Option<i32>,
    model_pos: Option<i32>,
    factory: i32,
    models: i32,
}

/// None off the rack page, or without a block selected
fn strip(app: &App) -> Option<Strip> {
    let instance = app.get_selected_instance_id();
    if app.get_current_view() != View::Rack || instance < 0 {
        return None;
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
    Some(Strip {
        instance,
        page: app.get_strip_page().clamp(0, pages - 1),
        pages,
        factory_pos,
        model_pos,
        factory,
        models,
    })
}

fn event(
    app: &App,
    before: &RefCell<HashMap<(i32, i32), f32>>,
    encoder: i32,
    action: i32,
    detents: i32,
) {
    let Some(Strip {
        instance,
        page,
        pages,
        factory_pos,
        model_pos,
        factory,
        models,
    }) = strip(app)
    else {
        return;
    };
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
        show_list(app, row, list_field(count, current, action, detents, open));
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
    let held = before.borrow().get(&key).copied();
    let response = control.respond(action, detents, open, held);
    match response.before {
        Before::Keep => {}
        Before::Set(value) => {
            before.borrow_mut().insert(key, value);
        }
        Before::Forget => {
            before.borrow_mut().remove(&key);
        }
    }
    show_list(app, row, response.list);
    if let Some(value) = response.value {
        app.invoke_set_parameter(instance, p.index, value);
    }
}

/// Pickup of each pot of the strip's fields (UI thread only)
#[cfg(feature = "gpio")]
#[derive(Default)]
struct Pots {
    fields: [pots::Pickup<(i32, i32)>; PER_PAGE as usize],
}

#[cfg(feature = "gpio")]
thread_local! {
    static POTS: RefCell<Pots> = RefCell::default();
}

/// The parameter under pot `pot` (0–4) of the strip shown: its row, kind,
/// range, options and value
#[cfg(feature = "gpio")]
struct Under {
    row: i32,
    index: i32,
    kind: slint::SharedString,
    min: f32,
    max: f32,
    value: f32,
    options: Vec<f32>,
}

#[cfg(feature = "gpio")]
impl Under {
    fn find(app: &App, strip: &Strip, pot: i32) -> Option<Self> {
        let slot = strip.page * PER_PAGE + pot;
        app.get_selected_parameters()
            .iter()
            .enumerate()
            .find(|(_, p)| p.strip_pos == slot)
            .map(|(row, p)| Self {
                row: row as i32,
                index: p.index,
                options: p.option_values.iter().collect(),
                kind: p.kind,
                min: p.min,
                max: p.max,
                value: p.value,
            })
    }

    fn target(&self, instance: i32) -> pots::Target<'_, (i32, i32)> {
        pots::Target {
            key: (instance, self.index),
            field: pots::Field {
                kind: &self.kind,
                min: self.min,
                max: self.max,
                options: &self.options,
            },
            value: self.value,
        }
    }
}

/// Pot `pot` moved to `position` (0–1): pots 1–5 set the parameter at
/// their place on the page shown once picked up, the 6th picks the page
#[cfg(feature = "gpio")]
pub fn pot(app: &App, pot: i32, position: f32) {
    let strip = strip(app);
    if pot == PER_PAGE {
        let Some(strip) = strip else {
            return;
        };
        let next = pots::page_at(position, strip.page, strip.pages);
        if next != strip.page {
            app.set_strip_list_row(-1);
            app.set_strip_page(next);
        }
        return;
    }
    let Some(pickup) = usize::try_from(pot).ok().filter(|&p| p < PER_PAGE as usize) else {
        return;
    };
    let under = strip.as_ref().and_then(|s| Under::find(app, s, pot));
    let target = strip
        .as_ref()
        .zip(under.as_ref())
        .map(|(s, u)| u.target(s.instance));
    // Every move counts, even away from the rack: a crossing is only seen
    // from where the pot really was. Borrowed for a moment: setting the
    // parameter calls back into the UI
    let value = POTS.with_borrow_mut(|pots| {
        pots.fields
            .get_mut(pickup)
            .and_then(|p| p.moved(target.as_ref(), position))
    });
    let (Some(strip), Some(under), Some(value)) = (strip, under, value) else {
        return;
    };
    let list_row = app.get_strip_list_row();
    if list_row != -1 && list_row != under.row {
        app.set_strip_list_row(-1);
    }
    app.invoke_set_parameter(strip.instance, under.index, value);
}

/// The board's switch moved: it turns the block shown on or off
#[cfg(feature = "gpio")]
pub fn pot_switch(app: &App) {
    if let Some(strip) = strip(app) {
        app.invoke_toggle_node_bypass(strip.instance);
    }
}

/// The 8Angle's LEDs: one per pot, then the switch's. A pot driving its
/// parameter is amber, one waiting to reach the value dim white, one with
/// nothing to drive off; the page pot is lit when there are pages to
/// pick, the switch when the block shown is on. All off away from the rack
#[cfg(feature = "gpio")]
pub fn pot_leds(app: &App) -> Vec<pedalboard::i2c_encoders::Led> {
    use pedalboard::i2c_encoders::Led;
    let mut leds = vec![Led::OFF; 9];
    let Some(strip) = strip(app) else {
        return leds;
    };
    POTS.with_borrow_mut(|pots| {
        for (pot, (led, pickup)) in leds.iter_mut().zip(pots.fields.iter_mut()).enumerate() {
            let under = Under::find(app, &strip, pot as i32);
            let target = under.as_ref().map(|u| u.target(strip.instance));
            *led = match target {
                None => {
                    pickup.sync(None);
                    Led::OFF
                }
                Some(target) if pickup.sync(Some(&target)) => Led::LIVE,
                Some(_) => Led::WAITING,
            };
        }
    });
    if let Some(led) = leds.get_mut(PER_PAGE as usize) {
        *led = if strip.pages > 1 { Led::LIVE } else { Led::OFF };
    }
    if let Some(led) = leds.last_mut() {
        *led = if app.get_selected_plugin_bypassed() {
            Led::OFF
        } else {
            Led::LIVE
        };
    }
    leds
}

/// Open, move or close the list of row `row`
fn show_list(app: &App, row: i32, list: List) {
    match list {
        List::Keep => {}
        List::Focus(option) => {
            app.set_strip_list_focus(option);
            app.set_strip_list_row(row);
        }
        List::Close => app.set_strip_list_row(-1),
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
