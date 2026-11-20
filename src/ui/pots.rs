//! Potentiometers on the knob strip (M5Stack 8Angle): a pot is absolute,
//! with end stops, so its position maps onto the whole range of the field
//! it drives, and it only takes the field over once it reaches the field's
//! value (pickup, or soft takeover): a pot never makes the value jump.
//!
//! - continuous: across the range, on a log scale for frequencies (as the
//!   encoders step it); integer: rounded;
//! - list: the option at that place, first to last;
//! - switch: off below the middle, on above, with some play around it.
//!
//! A pot is picked up when it crosses the field's value, comes within
//! [`WINDOW`] of it, or already selects it (same option, same side of a
//! switch). Another field under the pot (page, block), or a value changed
//! from elsewhere (touch, MIDI, preset, scene, remote), lets go of it.

/// How close a pot must come to the value to pick it up (a share of its
/// travel)
pub const WINDOW: f32 = 0.02;
/// Positions this far apart are the same value (float rounding)
const SAME: f32 = 0.001;
/// A switch flips this far past the middle of the travel
const SWITCH_PLAY: f32 = 0.05;
/// The page pot must be this far into a page's share of the travel to
/// turn to it
const PAGE_PLAY: f32 = 0.02;

/// The field a pot drives: kind, range and options as the strip has them
/// (`"float"`, `"int"`, `"enum"`, `"toggle"`)
#[derive(Debug, Clone, Copy)]
pub struct Field<'a> {
    pub kind: &'a str,
    pub min: f32,
    pub max: f32,
    pub options: &'a [f32],
}

impl Field<'_> {
    fn list(&self) -> bool {
        self.kind == "enum" && !self.options.is_empty()
    }

    fn log(&self) -> bool {
        self.min > 0.0 && self.max / self.min >= 100.0
    }

    fn middle(&self) -> f32 {
        (self.min + self.max) / 2.0
    }

    /// Where `value` is on a pot's travel, 0 to 1
    pub fn position(&self, value: f32) -> f32 {
        let position = if self.kind == "toggle" {
            if value > self.middle() {
                1.0
            } else {
                0.0
            }
        } else if self.list() {
            let last = self.options.len().saturating_sub(1);
            let nearest = self
                .options
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1 - value).abs().total_cmp(&(b.1 - value).abs()))
                .map_or(0, |(i, _)| i);
            if last == 0 {
                0.0
            } else {
                nearest as f32 / last as f32
            }
        } else if self.log() {
            (value.max(self.min) / self.min).ln() / (self.max / self.min).ln()
        } else if self.max != self.min {
            (value - self.min) / (self.max - self.min)
        } else {
            0.0
        };
        position.clamp(0.0, 1.0)
    }

    /// The value at `position` (0 to 1); `current` is kept by a switch in
    /// the play around the middle
    pub fn value(&self, position: f32, current: f32) -> f32 {
        let position = position.clamp(0.0, 1.0);
        let (lo, hi) = (self.min.min(self.max), self.max.max(self.min));
        match self.kind {
            "toggle" if position > 0.5 + SWITCH_PLAY => self.max,
            "toggle" if position < 0.5 - SWITCH_PLAY => self.min,
            "toggle" => current,
            _ if self.list() => {
                let last = self.options.len() - 1;
                let at = (position * last as f32).round() as usize;
                self.options.get(at.min(last)).copied().unwrap_or(current)
            }
            "int" => (self.min + position * (self.max - self.min))
                .round()
                .clamp(lo, hi),
            _ if self.log() => (self.min * (self.max / self.min).powf(position)).clamp(lo, hi),
            _ => (self.min + position * (self.max - self.min)).clamp(lo, hi),
        }
    }

    /// The pot at `position` already selects `value`: same option, same
    /// side of a switch
    fn selects(&self, position: f32, value: f32) -> bool {
        match self.kind {
            "toggle" => (position > 0.5) == (value > self.middle()),
            "int" => self.value(position, value) == value.round(),
            _ if self.list() => self.position(self.value(position, value)) == self.position(value),
            _ => false,
        }
    }
}

/// The field under a pot: `key` tells fields apart (block, parameter),
/// `value` is its value now
#[derive(Debug, Clone, Copy)]
pub struct Target<'a, K> {
    pub key: K,
    pub field: Field<'a>,
    pub value: f32,
}

/// Whether a pot drives the field under it
#[derive(Debug, Clone)]
pub struct Pickup<K> {
    key: Option<K>,
    picked: bool,
    /// Where the pot was last (none until it moved once)
    last: Option<f32>,
    /// Where the value the pot set is on the travel
    sent: Option<f32>,
}

impl<K> Default for Pickup<K> {
    fn default() -> Self {
        Self {
            key: None,
            picked: false,
            last: None,
            sent: None,
        }
    }
}

impl<K: PartialEq + Copy> Pickup<K> {
    /// Follows the field under the pot (None: nothing there): another
    /// field, or a value changed from elsewhere, must be picked up again.
    /// Whether the pot drives it
    pub fn sync(&mut self, target: Option<&Target<'_, K>>) -> bool {
        let key = target.map(|t| t.key);
        if key != self.key {
            self.key = key;
            self.picked = false;
            self.sent = None;
        }
        if let (Some(target), Some(sent)) = (target, self.sent) {
            if (target.field.position(target.value) - sent).abs() > SAME {
                self.picked = false;
                self.sent = None;
            }
        }
        self.picked && target.is_some()
    }

    /// The pot moved to `position`: the value to set, once picked up
    pub fn moved(&mut self, target: Option<&Target<'_, K>>, position: f32) -> Option<f32> {
        self.sync(target);
        let last = self.last.replace(position);
        let target = target?;
        let field = target.field;
        if !self.picked {
            let at = field.position(target.value);
            let crossed = last.is_some_and(|last| (last - at) * (position - at) <= 0.0);
            let near = (position - at).abs() <= WINDOW;
            if !(crossed || near || field.selects(position, target.value)) {
                return None;
            }
            self.picked = true;
        }
        let value = field.value(position, target.value);
        self.sent = Some(field.position(value));
        (value != target.value).then_some(value)
    }
}

/// The page the page pot at `position` turns to, from `page` of `pages`:
/// each page has an equal share of the travel, and the pot must be a little
/// into the next share, so a pot resting on a border does not flicker
pub fn page_at(position: f32, page: i32, pages: i32) -> i32 {
    if pages <= 1 {
        return 0;
    }
    let share = 1.0 / pages as f32;
    let target = ((position / share).floor() as i32).clamp(0, pages - 1);
    if target == page {
        return page;
    }
    let into = if target > page {
        position - target as f32 * share
    } else {
        (target + 1) as f32 * share - position
    };
    if into >= PAGE_PLAY || position <= 0.0 || position >= 1.0 {
        target
    } else {
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOB: Field = Field {
        kind: "float",
        min: 0.0,
        max: 10.0,
        options: &[],
    };
    const OPTIONS: [f32; 4] = [0.0, 1.0, 2.0, 5.0];
    const LIST: Field = Field {
        kind: "enum",
        min: 0.0,
        max: 5.0,
        options: &OPTIONS,
    };
    const SWITCH: Field = Field {
        kind: "toggle",
        min: 0.0,
        max: 1.0,
        options: &[],
    };

    fn at(key: i32, field: Field<'_>, value: f32) -> Target<'_, i32> {
        Target { key, field, value }
    }

    #[test]
    fn positions_map_onto_the_range() {
        assert_eq!(KNOB.value(0.25, 0.0), 2.5);
        assert_eq!(KNOB.position(2.5), 0.25);
        let hz = Field {
            min: 20.0,
            max: 20000.0,
            ..KNOB
        };
        // Log scale: the middle of the travel is the middle in octaves
        assert!((hz.value(0.5, 0.0) - 632.46).abs() < 0.1);
        assert!((hz.position(hz.value(0.3, 0.0)) - 0.3).abs() < 1e-4);
        let int = Field {
            kind: "int",
            ..KNOB
        };
        assert_eq!(int.value(0.33, 0.0), 3.0);
        // Lists: the option at that place, first to last
        assert_eq!(LIST.value(0.0, 2.0), 0.0);
        assert_eq!(LIST.value(0.4, 2.0), 1.0);
        assert_eq!(LIST.value(1.0, 2.0), 5.0);
        assert_eq!(LIST.position(2.0), 2.0 / 3.0);
        // Switches: play around the middle
        assert_eq!(SWITCH.value(0.2, 1.0), 0.0);
        assert_eq!(SWITCH.value(0.52, 0.0), 0.0);
        assert_eq!(SWITCH.value(0.52, 1.0), 1.0);
        assert_eq!(SWITCH.value(0.6, 0.0), 1.0);
    }

    #[test]
    fn a_pot_does_nothing_until_it_crosses_the_value_then_follows() {
        let mut pot = Pickup::default();
        let knob = |value| at(1, KNOB, value);
        // Value 7, pot at 0.2 then 0.4: below, never crossed
        assert_eq!(pot.moved(Some(&knob(7.0)), 0.2), None);
        assert_eq!(pot.moved(Some(&knob(7.0)), 0.4), None);
        assert!(!pot.sync(Some(&knob(7.0))));
        // Across 0.7: picked up, it follows from there
        assert_eq!(pot.moved(Some(&knob(7.0)), 0.75), Some(7.5));
        assert!(pot.sync(Some(&knob(7.5))));
        assert_eq!(pot.moved(Some(&knob(7.5)), 0.25), Some(2.5));
        // Within the window counts too, even on the first move
        let mut pot = Pickup::default();
        assert_eq!(pot.moved(Some(&knob(5.0)), 0.515625), Some(5.15625));
    }

    #[test]
    fn another_field_or_a_value_from_elsewhere_lets_go() {
        let mut pot = Pickup::default();
        assert_eq!(pot.moved(Some(&at(1, KNOB, 5.0)), 0.5), None);
        assert!(pot.sync(Some(&at(1, KNOB, 5.0))));
        // The page turned: another field under the pot
        assert!(!pot.sync(Some(&at(2, KNOB, 5.0))));
        assert_eq!(pot.moved(Some(&at(2, KNOB, 9.0)), 0.55), None);
        // Back on the first field: picked up again only by reaching it
        assert_eq!(pot.moved(Some(&at(1, KNOB, 5.0)), 0.6), None);
        assert_eq!(pot.moved(Some(&at(1, KNOB, 5.0)), 0.375), Some(3.75));
        // Touch, MIDI or a preset changed it
        assert!(!pot.sync(Some(&at(1, KNOB, 8.0))));
        assert_eq!(pot.moved(Some(&at(1, KNOB, 8.0)), 0.5), None);
        // An empty slot
        assert!(!pot.sync(None));
        assert_eq!(pot.moved(None, 0.9), None);
    }

    #[test]
    fn lists_and_switches_are_picked_up_on_their_value() {
        // Option 2.0 (third of four): the pot on it already selects it
        let mut pot = Pickup::default();
        assert_eq!(pot.moved(Some(&at(1, LIST, 2.0)), 0.1), None);
        assert_eq!(pot.moved(Some(&at(1, LIST, 2.0)), 0.6), None);
        assert!(pot.sync(Some(&at(1, LIST, 2.0))));
        assert_eq!(pot.moved(Some(&at(1, LIST, 2.0)), 0.95), Some(5.0));
        // A switch that is on: a pot low does not turn it off
        let mut pot = Pickup::default();
        assert_eq!(pot.moved(Some(&at(1, SWITCH, 1.0)), 0.1), None);
        assert_eq!(pot.moved(Some(&at(1, SWITCH, 1.0)), 0.3), None);
        // Past the middle it agrees, then going down flips it
        assert_eq!(pot.moved(Some(&at(1, SWITCH, 1.0)), 0.6), None);
        assert!(pot.sync(Some(&at(1, SWITCH, 1.0))));
        assert_eq!(pot.moved(Some(&at(1, SWITCH, 1.0)), 0.5), None);
        assert_eq!(pot.moved(Some(&at(1, SWITCH, 1.0)), 0.4), Some(0.0));
    }

    #[test]
    fn the_page_pot_shares_the_travel_with_some_play() {
        assert_eq!(page_at(0.9, 0, 1), 0);
        assert_eq!(page_at(0.0, 2, 3), 0);
        assert_eq!(page_at(1.0, 0, 3), 2);
        assert_eq!(page_at(0.5, 0, 3), 1);
        // Just over the border: stays
        assert_eq!(page_at(0.34, 0, 3), 0);
        assert_eq!(page_at(0.32, 1, 3), 1);
        assert_eq!(page_at(0.36, 0, 3), 1);
    }
}
