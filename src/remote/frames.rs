//! Pictures of the full editor: the rectangles of the window that changed,
//! encoded for the browser (see protocol::frame_header).
//!
//! JPEG (quality 80) for large areas, PNG (lossless, fast compression) for
//! small ones: a knob or a meter is a few hundred bytes of PNG, while a
//! page change is ten times smaller in JPEG than in PNG and quicker to
//! encode. `PEDALBOARD_REMOTE_FRAMES=png|jpeg` forces one.

use super::protocol::{frame_header, FrameFormat, FRAME_HEADER};

/// Rectangles up to this many pixels are sent as PNG
const PNG_MAX_PIXELS: usize = 128 * 128;
const JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Auto,
    Png,
    Jpeg,
}

impl Encoding {
    pub fn from_env() -> Encoding {
        match std::env::var("PEDALBOARD_REMOTE_FRAMES").as_deref() {
            Ok("png") => Encoding::Png,
            Ok("jpeg") => Encoding::Jpeg,
            _ => Encoding::Auto,
        }
    }

    fn format(self, pixels: usize) -> FrameFormat {
        match self {
            Encoding::Png => FrameFormat::Png,
            Encoding::Jpeg => FrameFormat::Jpeg,
            Encoding::Auto if pixels <= PNG_MAX_PIXELS => FrameFormat::Png,
            Encoding::Auto => FrameFormat::Jpeg,
        }
    }
}

/// A rectangle of the window, in pixels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    /// The smallest rectangle holding both
    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);
        Rect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        }
    }
}

impl Rect {
    fn area(self) -> usize {
        self.width * self.height
    }

    /// Whether they overlap or share an edge
    fn touches(self, other: Rect) -> bool {
        self.x <= other.x + other.width
            && other.x <= self.x + self.width
            && self.y <= other.y + other.height
            && other.y <= self.y + self.height
    }
}

/// Areas held at most, before the closest ones merge
const MAX_AREAS: usize = 4;

/// Areas of the window to send again: a few rectangles. Areas that touch
/// merge; distant ones (a meter and a knob) stay apart instead of becoming
/// the large rectangle between them, unless there are too many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Areas(Vec<Rect>);

impl Areas {
    pub fn add(&mut self, mut rect: Rect) {
        if rect.area() == 0 {
            return;
        }
        while let Some(i) = self.0.iter().position(|r| r.touches(rect)) {
            rect = rect.union(self.0.swap_remove(i));
        }
        self.0.push(rect);
        while self.0.len() > MAX_AREAS {
            // Merge the two that add the least area
            let mut best = (usize::MAX, 0, 1);
            for i in 0..self.0.len() {
                for j in i + 1..self.0.len() {
                    let (a, b) = (self.0[i], self.0[j]);
                    let cost = a.union(b).area().saturating_sub(a.area() + b.area());
                    if cost < best.0 {
                        best = (cost, i, j);
                    }
                }
            }
            let (_, i, j) = best;
            let b = self.0.swap_remove(j);
            self.0[i] = self.0[i].union(b);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.0
    }
}

/// Merging two areas may add this many pixels: below, one picture costs
/// less than two (each has a header, a PNG or JPEG header, and is
/// acknowledged)
const MERGE_SLACK: usize = 4096;

/// Merge the areas that are close: the same pixels, in fewer pictures
pub fn merge_close(rects: &mut Vec<Rect>) {
    loop {
        let mut best = (usize::MAX, 0, 0);
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let (a, b) = (rects[i], rects[j]);
                let cost = a.union(b).area().saturating_sub(a.area() + b.area());
                if cost < best.0 {
                    best = (cost, i, j);
                }
            }
        }
        let (cost, i, j) = best;
        if cost > MERGE_SLACK {
            return;
        }
        let b = rects.swap_remove(j);
        rects[i] = rects[i].union(b);
    }
}

/// The part of `rect` where `now` differs from `before` (both pictures of
/// the window, `width` pixels a row), None when nothing changed
pub fn changed<P: PartialEq>(rect: Rect, now: &[P], before: &[P], width: usize) -> Option<Rect> {
    let row = |y: usize| y * width + rect.x..y * width + rect.x + rect.width;
    let differs = |y: &usize| now[row(*y)] != before[row(*y)];
    let mut rows = rect.y..rect.y + rect.height;
    let top = rows.find(differs)?;
    let bottom = rows.rev().find(differs).unwrap_or(top);
    let (mut left, mut right) = (rect.width, 0);
    for y in top..=bottom {
        let (a, b) = (&now[row(y)], &before[row(y)]);
        if let Some(l) = (0..left).find(|&x| a[x] != b[x]) {
            left = l;
        }
        if let Some(r) = (right..rect.width).rev().find(|&x| a[x] != b[x]) {
            right = r + 1;
        }
    }
    Some(Rect {
        x: rect.x + left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom + 1 - top,
    })
}

/// The rectangle a frame message shows (see protocol::frame_header)
pub fn rect_of(message: &[u8]) -> Option<Rect> {
    let header = message.get(..FRAME_HEADER)?;
    let field = |i: usize| usize::from(u16::from_le_bytes([header[2 + i * 2], header[3 + i * 2]]));
    Some(Rect {
        x: field(0),
        y: field(1),
        width: field(2),
        height: field(3),
    })
}

/// The part of `rect` inside a window of `window` (width, height), None
/// when nothing is left
pub fn clip(rect: Rect, window: (usize, usize)) -> Option<Rect> {
    let x2 = (rect.x + rect.width).min(window.0);
    let y2 = (rect.y + rect.height).min(window.1);
    (rect.x < x2 && rect.y < y2).then(|| Rect {
        x: rect.x,
        y: rect.y,
        width: x2 - rect.x,
        height: y2 - rect.y,
    })
}

/// The message for rectangle `rect` of an RGB window (`rgb`, 3 bytes per
/// pixel, `window` = width and height): header then picture. None when the
/// rectangle is empty or outside the window.
pub fn encode(
    rgb: &[u8],
    window: (usize, usize),
    rect: Rect,
    encoding: Encoding,
) -> Option<Vec<u8>> {
    let (win_w, win_h) = window;
    let rect = clip(rect, window)?;
    if rgb.len() < win_w * win_h * 3 {
        return None;
    }
    let mut pixels = Vec::with_capacity(rect.width * rect.height * 3);
    for row in rect.y..rect.y + rect.height {
        let start = (row * win_w + rect.x) * 3;
        pixels.extend_from_slice(&rgb[start..start + rect.width * 3]);
    }
    encode_packed(&pixels, window, rect, encoding)
}

/// The message for rectangle `rect` (inside the window, see clip) whose
/// pixels are `rgb`: its rows one after the other, 3 bytes per pixel
pub fn encode_packed(
    rgb: &[u8],
    window: (usize, usize),
    rect: Rect,
    encoding: Encoding,
) -> Option<Vec<u8>> {
    let (w, h) = (rect.width, rect.height);
    if w == 0 || h == 0 || rgb.len() != w * h * 3 {
        return None;
    }
    let format = encoding.format(w * h);
    let mut out = Vec::with_capacity(FRAME_HEADER + w * h / 4);
    let fit = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    out.extend_from_slice(&frame_header(
        format,
        [fit(rect.x), fit(rect.y), fit(w), fit(h)],
        [fit(window.0), fit(window.1)],
    ));
    match format {
        FrameFormat::Png => {
            let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(rgb).ok()?;
            writer.finish().ok()?;
        }
        FrameFormat::Jpeg => {
            let encoder = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
            encoder
                .encode(rgb, fit(w), fit(h), jpeg_encoder::ColorType::Rgb)
                .ok()?;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window like the UI: flat colours, a gradient, some text-like noise
    fn window(w: usize, h: usize) -> Vec<u8> {
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                let v = if y < h / 4 {
                    30
                } else if (x / 7 + y / 9) % 5 == 0 {
                    220
                } else {
                    (x * 255 / w) as u8
                };
                rgb[i] = v;
                rgb[i + 1] = v / 2;
                rgb[i + 2] = 40;
            }
        }
        rgb
    }

    #[test]
    fn small_rects_png_large_jpeg() {
        let rgb = window(800, 480);
        let small = encode(
            &rgb,
            (800, 480),
            Rect {
                x: 10,
                y: 20,
                width: 40,
                height: 30,
            },
            Encoding::Auto,
        )
        .unwrap_or_default();
        assert_eq!(small[1], FrameFormat::Png as u8);
        assert_eq!(&small[FRAME_HEADER..FRAME_HEADER + 4], b"\x89PNG");
        let full = encode(
            &rgb,
            (800, 480),
            Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 480,
            },
            Encoding::Auto,
        )
        .unwrap_or_default();
        assert_eq!(full[1], FrameFormat::Jpeg as u8);
        assert_eq!(&full[FRAME_HEADER..FRAME_HEADER + 2], b"\xff\xd8");
    }

    #[test]
    fn clipped_to_the_window() {
        let rgb = window(100, 50);
        let clipped = encode(
            &rgb,
            (100, 50),
            Rect {
                x: 90,
                y: 40,
                width: 50,
                height: 50,
            },
            Encoding::Png,
        )
        .unwrap_or_default();
        assert_eq!(u16::from_le_bytes([clipped[6], clipped[7]]), 10);
        assert_eq!(u16::from_le_bytes([clipped[8], clipped[9]]), 10);
        assert!(encode(
            &rgb,
            (100, 50),
            Rect {
                x: 100,
                y: 0,
                width: 5,
                height: 5
            },
            Encoding::Png
        )
        .is_none());
    }

    #[test]
    fn rect_of_a_message_and_unions() {
        let rgb = window(100, 50);
        let rect = Rect {
            x: 10,
            y: 5,
            width: 20,
            height: 8,
        };
        let message = encode(&rgb, (100, 50), rect, Encoding::Png).unwrap_or_default();
        assert_eq!(rect_of(&message), Some(rect));
        assert_eq!(rect_of(&message[..4]), None);
        let other = Rect {
            x: 50,
            y: 0,
            width: 10,
            height: 2,
        };
        assert_eq!(
            rect.union(other),
            Rect {
                x: 10,
                y: 0,
                width: 50,
                height: 13
            }
        );
    }

    #[test]
    fn packed_pixels_encode_like_the_window() {
        let rgb = window(100, 50);
        let rect = Rect {
            x: 90,
            y: 40,
            width: 50,
            height: 50,
        };
        let clipped = clip(rect, (100, 50)).unwrap_or(rect);
        assert_eq!((clipped.width, clipped.height), (10, 10));
        let mut packed = Vec::new();
        for row in clipped.y..clipped.y + clipped.height {
            let start = (row * 100 + clipped.x) * 3;
            packed.extend_from_slice(&rgb[start..start + clipped.width * 3]);
        }
        assert_eq!(
            encode_packed(&packed, (100, 50), clipped, Encoding::Png),
            encode(&rgb, (100, 50), rect, Encoding::Png)
        );
        // Pixels that do not match the rectangle
        assert!(encode_packed(&packed[3..], (100, 50), clipped, Encoding::Png).is_none());
        assert!(clip(
            Rect {
                x: 100,
                y: 0,
                width: 5,
                height: 5
            },
            (100, 50)
        )
        .is_none());
    }

    #[test]
    fn areas_merge_when_they_touch_or_are_too_many() {
        let r = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        let mut areas = Areas::default();
        areas.add(r(0, 0, 10, 10));
        areas.add(r(500, 500, 10, 10));
        assert_eq!(areas.rects().len(), 2, "far apart");
        areas.add(r(10, 0, 10, 10));
        assert_eq!(areas.rects().len(), 2);
        assert!(areas.rects().contains(&r(0, 0, 20, 10)), "touching");
        areas.add(r(0, 0, 0, 5));
        assert_eq!(areas.rects().len(), 2, "empty");
        for i in 0..4 {
            areas.add(r(100 + i * 100, 300, 5, 5));
        }
        assert_eq!(areas.rects().len(), MAX_AREAS);
        // Everything is still covered
        for (x, y) in [(0, 0), (19, 9), (500, 500), (100, 300), (404, 304)] {
            assert!(areas
                .rects()
                .iter()
                .any(|a| a.x <= x && x < a.x + a.width && a.y <= y && y < a.y + a.height));
        }
    }

    #[test]
    fn close_areas_merge_far_ones_stay() {
        let r = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        // A knob and its value under it, and a meter far away
        let mut rects = vec![r(648, 701, 65, 64), r(660, 768, 40, 20), r(10, 10, 8, 100)];
        merge_close(&mut rects);
        assert_eq!(rects.len(), 2);
        assert!(rects.contains(&r(648, 701, 65, 87)));
        assert!(rects.contains(&r(10, 10, 8, 100)));
        let mut one = vec![r(0, 0, 5, 5)];
        merge_close(&mut one);
        assert_eq!(one, [r(0, 0, 5, 5)]);
    }

    #[test]
    fn only_what_changed_is_sent() {
        let width = 10;
        let before = vec![0u8; width * 8];
        let mut now = before.clone();
        let rect = Rect {
            x: 1,
            y: 1,
            width: 8,
            height: 6,
        };
        assert_eq!(changed(rect, &now, &before, width), None);
        now[2 * width + 3] = 1;
        now[4 * width + 6] = 1;
        // Outside the rectangle: not looked at
        now[7 * width + 9] = 1;
        assert_eq!(
            changed(rect, &now, &before, width),
            Some(Rect {
                x: 3,
                y: 2,
                width: 4,
                height: 3,
            })
        );
        now[width + 1] = 1;
        now[6 * width + 8] = 1;
        assert_eq!(changed(rect, &now, &before, width), Some(rect));
    }

    /// Sizes and times of both encodings: `cargo test --release frames -- --nocapture`
    #[test]
    fn compare_encodings() {
        let rgb = window(1280, 800);
        for encoding in [Encoding::Png, Encoding::Jpeg] {
            let start = std::time::Instant::now();
            let n = 5;
            let mut size = 0;
            for _ in 0..n {
                size = encode(
                    &rgb,
                    (1280, 800),
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1280,
                        height: 800,
                    },
                    encoding,
                )
                .map_or(0, |f| f.len());
            }
            println!(
                "{encoding:?}: {} KiB, {:.1} ms per 1280x800 frame",
                size / 1024,
                start.elapsed().as_secs_f64() * 1000.0 / n as f64
            );
            assert!(size > 0);
        }
    }
}
