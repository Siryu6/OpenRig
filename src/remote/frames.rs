//! Pictures of the full editor: the rectangles of the window that changed,
//! encoded for the browser (see protocol::frame_header).
//!
//! One pipeline, from the window to the pages (the app's `headless` runs
//! it): the UI thread draws, `Sent::update` finds the areas the pages do
//! not have yet, `Areas::pack` copies their pixels, `encode_picture` makes
//! the frames of the picture on the encoder thread, and the server sends
//! it whole to each page that can take it (see flow). Areas a page did not
//! get come back in the `Requests` of the next picture.
//!
//! JPEG (quality 80) for large areas, PNG (lossless, fast compression) for
//! small ones: a knob or a meter is a few hundred bytes of PNG, while a
//! page change is ten times smaller in JPEG than in PNG and quicker to
//! encode. `PEDALBOARD_REMOTE_FRAMES=png|jpeg` forces one.

use axum::body::Bytes;
use slint::Rgb8Pixel;

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
    /// The whole window of `window` (width, height)
    pub fn whole(window: (usize, usize)) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: window.0,
            height: window.1,
        }
    }

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

    fn area(self) -> usize {
        self.width * self.height
    }

    /// The part inside a window of `window` (width, height), None when
    /// nothing is left
    pub fn clip(self, window: (usize, usize)) -> Option<Rect> {
        let x2 = (self.x + self.width).min(window.0);
        let y2 = (self.y + self.height).min(window.1);
        (self.x < x2 && self.y < y2).then(|| Rect {
            x: self.x,
            y: self.y,
            width: x2 - self.x,
            height: y2 - self.y,
        })
    }

    /// The index ranges of its rows in a window `width` pixels wide
    fn rows(self, width: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
        (self.y..self.y + self.height).map(move |y| {
            let start = y * width + self.x;
            start..start + self.width
        })
    }
}

/// Areas held at most: past it, the closest ones merge
const MAX_AREAS: usize = 4;
/// Merging two areas may add this many pixels: below, one picture costs
/// less than two (each has a header, a PNG or JPEG header, and is
/// acknowledged)
const MERGE_SLACK: usize = 4096;

/// Areas of the window: a few rectangles. Close areas (a knob and its
/// value) merge; distant ones (a meter and a knob) stay apart instead of
/// becoming the large rectangle between them, unless there are too many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Areas(Vec<Rect>);

impl Areas {
    pub fn add(&mut self, rect: Rect) {
        if rect.area() == 0 {
            return;
        }
        self.0.push(rect);
        // Merge the two that add the least, while that is little or there
        // are too many
        while let Some((cost, i, j)) = self.cheapest_merge() {
            if cost > MERGE_SLACK && self.0.len() <= MAX_AREAS {
                break;
            }
            let b = self.0.swap_remove(j);
            self.0[i] = self.0[i].union(b);
        }
    }

    /// The pixels the cheapest merge adds, and its two areas
    fn cheapest_merge(&self) -> Option<(usize, usize, usize)> {
        let mut best: Option<(usize, usize, usize)> = None;
        for i in 0..self.0.len() {
            for j in i + 1..self.0.len() {
                let (a, b) = (self.0[i], self.0[j]);
                let cost = a.union(b).area().saturating_sub(a.area() + b.area());
                if best.is_none_or(|(c, _, _)| cost < c) {
                    best = Some((cost, i, j));
                }
            }
        }
        best
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.0
    }

    /// The pixels of the areas of `window` (`width` pixels a row) into
    /// `out`, one area after the other, rows in order
    pub fn pack<P: Copy>(&self, window: &[P], width: usize, out: &mut Vec<P>) {
        out.clear();
        for rect in &self.0 {
            for row in rect.rows(width) {
                out.extend_from_slice(&window[row]);
            }
        }
    }
}

/// What the pages asked of the next picture
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Requests {
    /// Window size wanted by the last page that asked
    pub size: Option<(u32, u32)>,
    /// The whole window: a page started showing the Editor, or missed
    /// frames
    pub keyframe: bool,
    /// Areas a page did not get (it was behind): sent again, as they are
    pub repaint: Areas,
}

/// The window as the pages have it: the pictures sent so far. The UI
/// redraws whole items (a label whose model row was set again): only the
/// pixels that changed go.
#[derive(Debug, Default)]
pub struct Sent<P> {
    pixels: Vec<P>,
    size: (usize, usize),
}

impl<P: Copy + PartialEq> Sent<P> {
    /// The areas of `now` (the window, `size` = width and height) to send:
    /// the parts of the areas the renderer redrew (`drawn`) that differ
    /// from what was sent, and the areas pages asked for again. The whole
    /// window for a keyframe or a new size. They count as sent.
    pub fn update(
        &mut self,
        now: &[P],
        size: (usize, usize),
        drawn: &[Rect],
        requests: &Requests,
    ) -> Areas {
        let mut areas = Areas::default();
        if requests.keyframe || size != self.size || self.pixels.len() != now.len() {
            self.pixels.clear();
            self.pixels.extend_from_slice(now);
            self.size = size;
            areas.add(Rect::whole(size));
            return areas;
        }
        for rect in drawn.iter().filter_map(|r| r.clip(size)) {
            if let Some(rect) = changed(rect, now, &self.pixels, size.0) {
                areas.add(rect);
            }
        }
        for rect in requests.repaint.rects().iter().filter_map(|r| r.clip(size)) {
            areas.add(rect);
        }
        for rect in areas.rects() {
            for row in rect.rows(size.0) {
                self.pixels[row.clone()].copy_from_slice(&now[row]);
            }
        }
        areas
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

/// One area of a picture, as sent: header then PNG or JPEG
#[derive(Debug, Clone)]
pub struct Frame {
    pub rect: Rect,
    pub message: Bytes,
}

/// The frames of a picture: `pixels` are those of `areas` (see
/// Areas::pack) in a window of `window` (width, height). `rgb` is the
/// caller's buffer, reused.
pub fn encode_picture(
    pixels: &[Rgb8Pixel],
    areas: &Areas,
    window: (usize, usize),
    encoding: Encoding,
    rgb: &mut Vec<u8>,
) -> Vec<Frame> {
    let mut picture = Vec::with_capacity(areas.rects().len());
    let mut offset = 0;
    for &rect in areas.rects() {
        let Some(area) = pixels.get(offset..offset + rect.area()) else {
            break;
        };
        offset += rect.area();
        rgb.clear();
        rgb.extend(area.iter().flat_map(|p| [p.r, p.g, p.b]));
        picture.extend(encode(rgb, window, rect, encoding));
    }
    picture
}

/// The frame of rectangle `rect` (inside the window, see Rect::clip)
/// whose pixels are `rgb`: its rows one after the other, 3 bytes a pixel
pub fn encode(rgb: &[u8], window: (usize, usize), rect: Rect, encoding: Encoding) -> Option<Frame> {
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
    Some(Frame {
        rect,
        message: out.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: usize, y: usize, width: usize, height: usize) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// A window like the UI: flat colours, a gradient, some text-like noise
    fn window(w: usize, h: usize) -> Vec<Rgb8Pixel> {
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let v = if y < h / 4 {
                    30
                } else if (x / 7 + y / 9) % 5 == 0 {
                    220
                } else {
                    (x * 255 / w) as u8
                };
                pixels.push(Rgb8Pixel::new(v, v / 2, 40));
            }
        }
        pixels
    }

    /// The frame of one area of a window
    fn frame_of(
        pixels: &[Rgb8Pixel],
        window: (usize, usize),
        rect: Rect,
        encoding: Encoding,
    ) -> Option<Frame> {
        let mut areas = Areas::default();
        areas.add(rect.clip(window)?);
        let mut packed = Vec::new();
        areas.pack(pixels, window.0, &mut packed);
        encode_picture(&packed, &areas, window, encoding, &mut Vec::new())
            .into_iter()
            .next()
    }

    #[test]
    fn small_rects_png_large_jpeg() {
        let pixels = window(800, 480);
        let small = frame_of(&pixels, (800, 480), r(10, 20, 40, 30), Encoding::Auto)
            .map(|f| f.message)
            .unwrap_or_default();
        assert_eq!(small[1], FrameFormat::Png as u8);
        assert_eq!(&small[FRAME_HEADER..FRAME_HEADER + 4], b"\x89PNG");
        let full = frame_of(&pixels, (800, 480), r(0, 0, 800, 480), Encoding::Auto)
            .map(|f| f.message)
            .unwrap_or_default();
        assert_eq!(full[1], FrameFormat::Jpeg as u8);
        assert_eq!(&full[FRAME_HEADER..FRAME_HEADER + 2], b"\xff\xd8");
    }

    #[test]
    fn frames_show_their_rectangle() {
        let pixels = window(100, 50);
        let rect = r(10, 5, 20, 8);
        let Some(frame) = frame_of(&pixels, (100, 50), rect, Encoding::Png) else {
            panic!("no frame");
        };
        assert_eq!(frame.rect, rect);
        let field = |i: usize| u16::from_le_bytes([frame.message[i], frame.message[i + 1]]);
        assert_eq!([field(2), field(4), field(6), field(8)], [10, 5, 20, 8]);
        assert_eq!([field(10), field(12)], [100, 50]);
        // Pixels that do not match the rectangle
        assert!(encode(&[0; 3], (100, 50), rect, Encoding::Png).is_none());
    }

    #[test]
    fn clipped_to_the_window() {
        assert_eq!(r(90, 40, 50, 50).clip((100, 50)), Some(r(90, 40, 10, 10)));
        assert_eq!(r(100, 0, 5, 5).clip((100, 50)), None);
        assert_eq!(r(0, 0, 0, 5).clip((100, 50)), None);
        assert_eq!(r(10, 5, 20, 8).union(r(50, 0, 10, 2)), r(10, 0, 50, 13));
    }

    #[test]
    fn a_picture_encodes_each_area_from_its_packed_pixels() {
        let pixels = window(100, 50);
        let mut areas = Areas::default();
        areas.add(r(0, 0, 10, 10));
        areas.add(r(80, 30, 20, 20));
        let mut packed = Vec::new();
        areas.pack(&pixels, 100, &mut packed);
        assert_eq!(packed.len(), 100 + 400);
        assert_eq!(packed[100], pixels[30 * 100 + 80]);
        let picture = encode_picture(&packed, &areas, (100, 50), Encoding::Png, &mut Vec::new());
        assert_eq!(picture.len(), 2);
        for (frame, &rect) in picture.iter().zip(areas.rects()) {
            let alone = frame_of(&pixels, (100, 50), rect, Encoding::Png);
            assert_eq!(Some(&frame.message), alone.as_ref().map(|f| &f.message));
        }
        // Too few pixels: the areas they cover
        assert_eq!(
            encode_picture(
                &packed[..150],
                &areas,
                (100, 50),
                Encoding::Png,
                &mut Vec::new()
            )
            .len(),
            1
        );
    }

    #[test]
    fn close_areas_merge_far_ones_stay() {
        let mut areas = Areas::default();
        // A knob and its value under it, and a meter far away
        areas.add(r(648, 701, 65, 64));
        areas.add(r(10, 10, 8, 100));
        areas.add(r(660, 768, 40, 20));
        assert_eq!(areas.rects().len(), 2);
        assert!(areas.rects().contains(&r(648, 701, 65, 87)));
        assert!(areas.rects().contains(&r(10, 10, 8, 100)));
        // Touching or inside: one area
        let mut areas = Areas::default();
        areas.add(r(0, 0, 10, 10));
        areas.add(r(10, 0, 10, 10));
        areas.add(r(2, 2, 3, 3));
        areas.add(r(0, 0, 0, 5));
        assert_eq!(areas.rects(), [r(0, 0, 20, 10)]);
    }

    #[test]
    fn too_many_areas_merge() {
        let mut areas = Areas::default();
        for i in 0..8 {
            areas.add(r(i * 1000, (i % 2) * 400, 5, 5));
        }
        assert_eq!(areas.rects().len(), MAX_AREAS);
        // Everything is still covered
        for i in 0..8 {
            let (x, y) = (i * 1000 + 4, (i % 2) * 400 + 4);
            assert!(areas
                .rects()
                .iter()
                .any(|a| a.x <= x && x < a.x + a.width && a.y <= y && y < a.y + a.height));
        }
    }

    #[test]
    fn only_what_changed_is_sent() {
        let width = 10;
        let before = vec![0u8; width * 8];
        let mut now = before.clone();
        let rect = r(1, 1, 8, 6);
        assert_eq!(changed(rect, &now, &before, width), None);
        now[2 * width + 3] = 1;
        now[4 * width + 6] = 1;
        // Outside the rectangle: not looked at
        now[7 * width + 9] = 1;
        assert_eq!(changed(rect, &now, &before, width), Some(r(3, 2, 4, 3)));
        now[width + 1] = 1;
        now[6 * width + 8] = 1;
        assert_eq!(changed(rect, &now, &before, width), Some(rect));
    }

    #[test]
    fn sent_tracks_what_the_pages_have() {
        let size = (20, 10);
        let mut now = vec![0u8; 200];
        let mut sent = Sent::default();
        let nothing = Requests::default();
        // The first picture is the whole window
        let areas = sent.update(&now, size, &[r(5, 5, 2, 2)], &nothing);
        assert_eq!(areas.rects(), [Rect::whole(size)]);
        // Redrawn as it was: nothing goes
        assert!(sent
            .update(&now, size, &[r(0, 0, 20, 10)], &nothing)
            .is_empty());
        // A change goes once, trimmed, and clipped to the window
        now[3 * 20 + 18] = 7;
        let drawn = [r(10, 0, 30, 30)];
        assert_eq!(
            sent.update(&now, size, &drawn, &nothing).rects(),
            [r(18, 3, 1, 1)]
        );
        assert!(sent.update(&now, size, &drawn, &nothing).is_empty());
        // A change outside the areas drawn is not looked at
        now[0] = 1;
        assert!(sent.update(&now, size, &drawn, &nothing).is_empty());
        // Areas asked again go as they are, unchanged or not
        let mut repaint = Areas::default();
        repaint.add(r(15, 8, 10, 10));
        let asked = Requests {
            repaint,
            ..Requests::default()
        };
        assert_eq!(
            sent.update(&now, size, &[], &asked).rects(),
            [r(15, 8, 5, 2)]
        );
        // A keyframe is the whole window
        let keyframe = Requests {
            keyframe: true,
            ..Requests::default()
        };
        assert_eq!(
            sent.update(&now, size, &[], &keyframe).rects(),
            [Rect::whole(size)]
        );
    }

    #[test]
    fn a_new_size_sends_the_whole_window() {
        // As many pixels, rows of another width: no comparing with the
        // old rows, even without a keyframe
        let mut sent = Sent::default();
        let nothing = Requests::default();
        let _ = sent.update(&[0u8; 200], (20, 10), &[], &nothing);
        let mut now = vec![0u8; 200];
        now[5] = 1;
        let areas = sent.update(&now, (10, 20), &[r(0, 0, 1, 1)], &nothing);
        assert_eq!(areas.rects(), [Rect::whole((10, 20))]);
        assert!(sent
            .update(&now, (10, 20), &[r(0, 0, 10, 20)], &nothing)
            .is_empty());
    }

    /// Sizes and times of both encodings: `cargo test --release frames -- --nocapture`
    #[test]
    fn compare_encodings() {
        let pixels = window(1280, 800);
        for encoding in [Encoding::Png, Encoding::Jpeg] {
            let start = std::time::Instant::now();
            let n = 5;
            let mut size = 0;
            for _ in 0..n {
                size = frame_of(&pixels, (1280, 800), Rect::whole((1280, 800)), encoding)
                    .map_or(0, |f| f.message.len());
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
