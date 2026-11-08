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
    let x2 = (rect.x + rect.width).min(win_w);
    let y2 = (rect.y + rect.height).min(win_h);
    if rect.x >= x2 || rect.y >= y2 || rgb.len() < win_w * win_h * 3 {
        return None;
    }
    let (w, h) = (x2 - rect.x, y2 - rect.y);
    let mut pixels = Vec::with_capacity(w * h * 3);
    for row in rect.y..y2 {
        let start = (row * win_w + rect.x) * 3;
        pixels.extend_from_slice(&rgb[start..start + w * 3]);
    }
    let format = encoding.format(w * h);
    let mut out = Vec::with_capacity(FRAME_HEADER + w * h / 4);
    let fit = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    out.extend_from_slice(&frame_header(
        format,
        [fit(rect.x), fit(rect.y), fit(w), fit(h)],
        [fit(win_w), fit(win_h)],
    ));
    match format {
        FrameFormat::Png => {
            let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(&pixels).ok()?;
            writer.finish().ok()?;
        }
        FrameFormat::Jpeg => {
            let encoder = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
            encoder
                .encode(&pixels, fit(w), fit(h), jpeg_encoder::ColorType::Rgb)
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
