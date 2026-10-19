//! Levels of the sound through a block over the last seconds, shared by the
//! dynamics and scope views: their dB scale and their history.

use std::collections::VecDeque;

use crate::plugin_view::{area, line, VIEW};

/// Bottom of the level scale, dB (the top is 0 dB); DbLabels in plot.slint
/// labels the same scale
pub(crate) const FLOOR: f32 = -60.0;
/// Points of the history, one per frame (6 s at 25 frames per second)
const HISTORY: usize = 150;

/// Height of a level, 0 (FLOOR) to 1 (0 dB)
pub(crate) fn up(db: f32) -> f32 {
    (1.0 - db / FLOOR).clamp(0.0, 1.0)
}

/// Level at a height, dB (the reverse of `up`)
pub(crate) fn down(height: f32) -> f32 {
    FLOOR * (1.0 - height.clamp(0.0, 1.0))
}

/// Height of an amplitude (1 for 0 dB)
pub(crate) fn amplitude_up(amplitude: f32) -> f32 {
    up(20.0 * amplitude.max(1e-6).log10())
}

/// Peak level of `samples`, dB
fn peak(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    20.0 * peak.max(1e-6).log10()
}

/// Levels in and out over the last seconds, one point per frame, with a
/// third value (a compressor's gain reduction)
#[derive(Default)]
pub(crate) struct History {
    input: f32,
    output: f32,
    /// Oldest first: level in, level out, dB, and the third value
    points: VecDeque<(f32, f32, f32)>,
}

impl History {
    /// Levels in and out, dB: the peaks of the samples since the last frame,
    /// or the last levels less `fall` dB (a rise shows at once)
    pub fn levels(&mut self, input: &[f32], output: &[f32], fall: f32) -> (f32, f32) {
        self.input = peak(input).max(self.input - fall).max(FLOOR - 1.0);
        self.output = peak(output).max(self.output - fall).max(FLOOR - 1.0);
        (self.input, self.output)
    }

    /// Keep the levels as the newest point
    pub fn push(&mut self, third: f32) {
        self.points.push_back((self.input, self.output, third));
        while self.points.len() > HISTORY {
            self.points.pop_front();
        }
    }

    /// Nothing went through for the whole history
    fn quiet(&self) -> bool {
        self.points
            .iter()
            .all(|(i, o, r)| *i <= FLOOR && *o <= FLOOR && *r > -0.1)
    }

    /// The points, the newest at the right edge of a plot `aspect` times
    /// wider than high, each at the height `y` gives
    fn across<'a>(
        &'a self,
        aspect: f32,
        y: impl Fn(&(f32, f32, f32)) -> f32 + 'a,
    ) -> impl Iterator<Item = (f32, f32)> + 'a {
        let width = VIEW * aspect;
        let newest = self.points.len().saturating_sub(1);
        self.points.iter().enumerate().map(move |(i, p)| {
            let x = width - (newest - i) as f32 * width / (HISTORY - 1) as f32;
            (x, y(p) * VIEW)
        })
    }

    /// Area under the level in (`output` false) or out
    pub fn area(&self, output: bool, aspect: f32) -> String {
        if self.quiet() {
            return String::new();
        }
        let level = |p: &(f32, f32, f32)| 1.0 - up(if output { p.1 } else { p.0 });
        area(self.across(aspect, level), VIEW)
    }

    /// Line of the third value, `y` giving its height (0 at the top)
    pub fn line(&self, aspect: f32, y: impl Fn(f32) -> f32) -> String {
        if self.quiet() {
            return String::new();
        }
        line(self.across(aspect, |p| y(p.2)))
    }
}
