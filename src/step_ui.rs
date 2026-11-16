//! The bar of a stutter, slicer or rhythmic gate (openrig:StepGridView),
//! above its parameters: drawn from what its plugin streams
//! (view_infer::step_grid), never from its controls. The cells are drawn
//! again only when what they play changes; the playhead moves on its own,
//! by whole pixels, so a frame redraws a thin strip and not the bar.

use pedalboard::audio::spectrum::{BlockTap, StreamFrame};
use pedalboard::audio::transport::BEATS_PER_BAR;
use pedalboard::plugins::scanner::Stream;
use pedalboard::plugins::view_infer::{self, STEP_ROWS};
use pedalboard::state::AppState;

use crate::plugin_view::{self, Cache, Controls};
use crate::{App, StepBox, StepLive, StepView};

/// Narrowest cell, logical pixels: neighbours are merged above that
const NARROWEST: f32 = 4.0;
/// Room above the cells for the caption, logical pixels
const HEADROOM: f32 = 30.0;
/// Space each side of a run of cells, logical pixels
const GAP: f32 = 1.0;

thread_local! {
    static VIEWS: Cache<Stream> = Cache::default();
}

/// Show the bar of the selected block, if its plugin streams one
pub(crate) fn refresh(app: &App, state: &AppState) {
    let view = view(app, state).unwrap_or_default();
    if view != app.get_selected_step() {
        app.set_selected_step(view);
    }
}

fn view(app: &App, state: &AppState) -> Option<StepView> {
    let controls = Controls::of_selection(app, state)?;
    controls.inferred(&VIEWS, view_infer::step_grid)?;
    Some(StepView {
        shown: true,
        beats: BEATS_PER_BAR as i32,
        summary: format!("1 bar · {:.0} BPM", state.tempo()).into(),
    })
}

/// One cell as drawn
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cell {
    /// 0 to 1
    level: f32,
    /// 0 live, 1 captured, 2 repeat
    source: u8,
    speed: f32,
}

/// The cells of a frame's three rows (`length` values each), merged down
/// to `most`: the lowest level, the highest source, the speed of the first
fn cells(values: &[f32], most: usize) -> Vec<Cell> {
    let length = values.len() / STEP_ROWS as usize;
    let (levels, rest) = values.split_at(length);
    let (sources, speeds) = rest.split_at(length);
    let count = length.min(most.max(1));
    (0..count)
        .filter_map(|k| {
            let range = k * length / count..((k + 1) * length / count).max(k * length / count + 1);
            let level = levels
                .get(range.clone())?
                .iter()
                .map(|l| {
                    if l.is_finite() {
                        l.clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                })
                .fold(1.0, f32::min);
            let source = sources
                .get(range.clone())?
                .iter()
                .map(|s| {
                    if s.is_finite() {
                        s.round().clamp(0.0, 2.0) as u8
                    } else {
                        0
                    }
                })
                .max()?;
            let speed = speeds
                .get(range.start)
                .copied()
                .filter(|s| s.is_finite())
                .unwrap_or(1.0);
            Some(Cell {
                level,
                source,
                speed,
            })
        })
        .collect()
}

/// `cells` as drawn across a plot `width` × `height` logical pixels:
/// boxes for runs of the same source and level, the speed where it is not 1
/// as level and upright strokes, and a mark on each run played backward
/// (all as fractions of the plot from its top left). Rectangles, not
/// paths: the software renderer redraws only what the playhead crosses,
/// where a path would be drawn whole again.
fn drawn(cells: &[Cell], width: f32, height: f32) -> Drawn {
    let mut drawn = Drawn::default();
    if cells.is_empty() || width <= 0.0 || height <= 0.0 {
        return drawn;
    }
    let step = 1.0 / cells.len() as f32;
    let gap = GAP / width;
    let top = (HEADROOM / height).min(0.4);
    let y = |level: f32| 1.0 - level * (1.0 - top);
    let off = |c: &Cell| (c.speed - 1.0).abs() > 1e-3;
    let mut start = 0;
    let mut last_speed: Option<f32> = None;
    for (i, cell) in cells.iter().enumerate() {
        let x = i as f32 * step;
        let next = cells.get(i + 1);
        // A run of the same source and level is one box
        if next.is_none_or(|n| n.source != cell.source || n.level != cell.level) {
            let (x0, x1) = (start as f32 * step + gap, (i + 1) as f32 * step - gap);
            if cell.level > 1e-3 && x1 > x0 {
                drawn.cells.push(StepBox {
                    x: x0,
                    y: y(cell.level),
                    width: x1 - x0,
                    height: 1.0 - y(cell.level),
                    source: i32::from(cell.source),
                });
            }
            start = i + 1;
        }
        // The speed where it is not 1: level strokes, joined upright
        if off(cell) {
            let at = y(cell.speed.abs().min(1.0));
            if let Some(before) = last_speed.filter(|b| *b != at) {
                drawn.speed.push(StepBox {
                    x,
                    y: before.min(at),
                    width: 0.0,
                    height: (before - at).abs(),
                    source: 0,
                });
            }
            match drawn.speed.last_mut() {
                Some(line) if last_speed == Some(at) && line.height == 0.0 => {
                    line.width = x + step - line.x;
                }
                _ => drawn.speed.push(StepBox {
                    x,
                    y: at,
                    width: step,
                    height: 0.0,
                    source: 0,
                }),
            }
            last_speed = Some(at);
        } else {
            last_speed = None;
        }
        // Backward: a mark in the middle of the run
        let backward = |c: &Cell| c.speed < 0.0;
        if backward(cell) && (i == 0 || cells.get(i - 1).is_none_or(|c| !backward(c))) {
            let run = cells.iter().skip(i).take_while(|c| backward(c)).count() as f32;
            drawn.reverse.push(StepBox {
                x: x + run * step / 2.0,
                y: (1.0 + top) / 2.0,
                width: 0.0,
                height: 0.0,
                source: 0,
            });
        }
    }
    drawn
}

/// What the bar plays, as drawn
#[derive(Debug, Default, PartialEq)]
struct Drawn {
    cells: Vec<StepBox>,
    speed: Vec<StepBox>,
    reverse: Vec<StepBox>,
}

/// What the bar plays, frame after frame
#[derive(Default)]
pub(crate) struct Live {
    frame: StreamFrame,
    /// The rows and plot size the cells were drawn for
    rows: Vec<f32>,
    size: (f32, f32),
    /// The playhead, whole pixels across
    playhead: Option<f32>,
}

impl Live {
    /// Show the last frame the block streamed, if it is new or the plot was
    /// laid out again
    pub fn frame(&mut self, app: &App, tap: &BlockTap) {
        let new = tap.read_stream(&mut self.frame);
        let size = plugin_view::size();
        if self.frame.count == 0 || self.frame.channels != STEP_ROWS || (!new && size == self.size)
        {
            return;
        }
        let (width, height) = size;
        if self.rows != self.frame.values || size != self.size {
            self.rows.clone_from(&self.frame.values);
            self.size = size;
            let most = (width / NARROWEST).floor() as usize;
            let drawn = drawn(&cells(&self.rows, most), width, height);
            let current = app.get_selected_step_live();
            let live = StepLive {
                cells: plugin_view::in_place(&current.cells, drawn.cells),
                speed: plugin_view::in_place(&current.speed, drawn.speed),
                reverse: plugin_view::in_place(&current.reverse, drawn.reverse),
            };
            if live != current {
                app.set_selected_step_live(live);
            }
        }
        if width > 0.0 {
            let x = (self.frame.position.clamp(0.0, 1.0) * width).round();
            if self.playhead != Some(x) {
                self.playhead = Some(x);
                app.set_selected_step_playhead(x / width);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(levels: &[f32], sources: &[f32], speeds: &[f32]) -> Vec<f32> {
        [levels, sources, speeds].concat()
    }

    #[test]
    fn neighbouring_cells_merge_to_fit() {
        let values = frame(
            &[1.0, 0.5, 1.0, 1.0],
            &[0.0, 1.0, 2.0, 2.0],
            &[1.0, 1.0, -1.0, 0.5],
        );
        let all = cells(&values, 8);
        assert_eq!(all.len(), 4);
        assert_eq!(all[2].source, 2);
        // Two by two: the lowest level, the highest source, the first speed
        let merged = cells(&values, 2);
        assert_eq!(
            merged,
            vec![
                Cell {
                    level: 0.5,
                    source: 1,
                    speed: 1.0
                },
                Cell {
                    level: 1.0,
                    source: 2,
                    speed: -1.0
                },
            ]
        );
        // Values out of range or not numbers stay in bounds
        let odd = cells(&frame(&[f32::NAN, 7.0], &[9.0, -3.0], &[f32::NAN, 1.0]), 8);
        assert_eq!(
            odd[0],
            Cell {
                level: 0.0,
                source: 2,
                speed: 1.0
            }
        );
        assert_eq!(odd[1].level, 1.0);
        assert!(cells(&[], 8).is_empty());
    }

    #[test]
    fn runs_are_drawn_as_boxes_by_source() {
        // Live, then a slice captured over two cells, then two repeats
        // backward, slowing down
        let values = frame(
            &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &[0.0, 1.0, 1.0, 2.0, 2.0, 0.0],
            &[1.0, 1.0, 1.0, -1.0, -0.5, 1.0],
        );
        let drawn = drawn(&cells(&values, 64), 600.0, 100.0);
        // One box per run: live, captured, the repeats, live
        let sources: Vec<i32> = drawn.cells.iter().map(|c| c.source).collect();
        assert_eq!(sources, [0, 1, 2, 0]);
        assert!(drawn.cells.windows(2).all(|w| w[0].x + w[0].width < w[1].x));
        // The speed only where it is not 1: two levels joined upright
        let level: Vec<bool> = drawn.speed.iter().map(|s| s.height == 0.0).collect();
        assert_eq!(level, [true, false, true]);
        // One mark backward, in the middle of its run
        assert_eq!(drawn.reverse.len(), 1);
        assert!((drawn.reverse[0].x - 4.0 / 6.0).abs() < 1e-5);
        // Silence draws no box
        let quiet = super::drawn(&cells(&frame(&[0.0], &[0.0], &[1.0]), 8), 600.0, 100.0);
        assert_eq!(quiet, Drawn::default());
    }
}
