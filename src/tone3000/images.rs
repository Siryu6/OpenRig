//! Tone pictures and creator avatars, fetched when shown (the TONE3000
//! branding guidelines ask for both in the tone lists).
//!
//! They are public files of the TONE3000 storage: no token goes with them.
//! Each is asked as a thumbnail of the size it is drawn at, decoded and
//! cropped to a square here; the library keeps them in memory only. The
//! picture of a downloaded tone is saved with its models, to show in its
//! block without a network (on stage).

use std::io::{Cursor, Read};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

/// Where TONE3000 pictures are served from
const HOSTS: [&str; 2] = ["https://api.tone3000.com/", "https://www.tone3000.com/"];
/// Public file of the storage, and its resized rendition
const STORAGE_FILE: &str = "/storage/v1/object/public/";
const STORAGE_RENDER: &str = "/storage/v1/render/image/public/";
/// Largest picture accepted (originals are a few hundred KB)
const MAX_BYTES: u64 = 8 << 20;
/// Largest decoded side: bounds the memory of a hostile file
const MAX_SIDE: usize = 4096;

/// A decoded square picture, RGBA
pub struct Picture {
    pub side: u32,
    pub rgba: Vec<u8>,
}

/// Whether `url` points at TONE3000
pub fn is_tone3000(url: &str) -> bool {
    HOSTS.iter().any(|host| url.starts_with(host))
}

/// The storage's rendition of `url` at `side` pixels, if it has one
pub fn thumbnail_url(url: &str, side: u32) -> Option<String> {
    if !url.starts_with(HOSTS[0]) || !url.contains(STORAGE_FILE) || url.contains('?') {
        return None;
    }
    Some(format!(
        "{}?width={side}&height={side}&resize=cover",
        url.replacen(STORAGE_FILE, STORAGE_RENDER, 1)
    ))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("pedalboard/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(url: &str) -> Result<Vec<u8>> {
    let response = agent()
        .get(url)
        // JPEG and PNG are the formats decoded here
        .header("Accept", "image/jpeg, image/png")
        .call()
        .map_err(|e| anyhow!("Cannot reach TONE3000: {e}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("HTTP {status}");
    }
    let mut data = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        bail!("picture too large");
    }
    Ok(data)
}

/// Fetch `url` as a `side` × `side` square
pub fn fetch(url: &str, side: u32) -> Result<Picture> {
    if !is_tone3000(url) {
        bail!("not a TONE3000 picture: {url}");
    }
    // The rendition is small; the original stays the fallback
    let data = match thumbnail_url(url, side).map(|thumb| get(&thumb)) {
        Some(Ok(data)) => data,
        _ => get(url)?,
    };
    let (width, height, rgba) = decode(&data)?;
    Ok(cover(width, height, &rgba, side))
}

/// PNG file of a picture
pub fn encode_png(picture: &Picture) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut encoder = png::Encoder::new(&mut data, picture.side, picture.side);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&picture.rgba)?;
    writer.finish()?;
    Ok(data)
}

/// A picture file saved on the pedalboard, as a square
pub fn load(path: &std::path::Path) -> Result<Picture> {
    let file = std::fs::File::open(path)?;
    let mut data = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        bail!("picture too large");
    }
    let (width, height, rgba) = decode(&data)?;
    let side = width.min(height).clamp(1, MAX_SIDE) as u32;
    Ok(cover(width, height, &rgba, side))
}

/// Decode a JPEG or PNG file to RGBA
pub fn decode(data: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    if data.starts_with(&[0xff, 0xd8]) {
        use zune_jpeg::zune_core::colorspace::ColorSpace;
        use zune_jpeg::zune_core::options::DecoderOptions;
        let options = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(MAX_SIDE)
            .set_max_height(MAX_SIDE);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(data), options);
        let rgba = decoder.decode().map_err(|e| anyhow!("JPEG: {e:?}"))?;
        let info = decoder.info().context("JPEG without size")?;
        let (width, height) = (info.width as usize, info.height as usize);
        if rgba.len() != width * height * 4 {
            bail!("JPEG: unexpected colors");
        }
        return Ok((width, height, rgba));
    }
    if data.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(Cursor::new(data));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info()?;
        let (width, height) = reader.info().size();
        if width as usize > MAX_SIDE || height as usize > MAX_SIDE {
            bail!("PNG too large");
        }
        let size = reader.output_buffer_size().context("PNG too large")?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf)?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .chunks(3)
                .flat_map(|p| [p[0], p[1], p[2], 0xff])
                .collect(),
            png::ColorType::GrayscaleAlpha => buf
                .chunks(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 0xff]).collect(),
            png::ColorType::Indexed => bail!("PNG: unexpected indexed colors"),
        };
        return Ok((info.width as usize, info.height as usize, rgba));
    }
    bail!("unsupported picture format")
}

/// Crop the centre square of a picture and scale it to `side` (box filter
/// when shrinking, nearest pixel when growing)
pub fn cover(width: usize, height: usize, rgba: &[u8], side: u32) -> Picture {
    let side = side.max(1) as usize;
    let crop = width.min(height).max(1);
    let (x0, y0) = (
        (width - crop.min(width)) / 2,
        (height - crop.min(height)) / 2,
    );
    let mut out = vec![0u8; side * side * 4];
    if width == 0 || height == 0 {
        return Picture {
            side: side as u32,
            rgba: out,
        };
    }
    for oy in 0..side {
        let sy0 = y0 + oy * crop / side;
        let sy1 = (y0 + ((oy + 1) * crop).div_ceil(side))
            .max(sy0 + 1)
            .min(height);
        for ox in 0..side {
            let sx0 = x0 + ox * crop / side;
            let sx1 = (x0 + ((ox + 1) * crop).div_ceil(side))
                .max(sx0 + 1)
                .min(width);
            let mut sum = [0u32; 4];
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = (sy * width + sx) * 4;
                    for c in 0..4 {
                        sum[c] += rgba[p + c] as u32;
                    }
                }
            }
            let n = ((sy1 - sy0) * (sx1 - sx0)) as u32;
            let o = (oy * side + ox) * 4;
            for c in 0..4 {
                out[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    Picture {
        side: side as u32,
        rgba: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_storage_renditions() {
        let url = "https://api.tone3000.com/storage/v1/object/public/images/abc.jpg";
        assert_eq!(
            thumbnail_url(url, 96).unwrap(),
            "https://api.tone3000.com/storage/v1/render/image/public/images/abc.jpg?width=96&height=96&resize=cover"
        );
        assert!(thumbnail_url("https://www.tone3000.com/x.png", 96).is_none());
        assert!(is_tone3000(url));
        assert!(!is_tone3000(
            "https://example.com/storage/v1/object/public/a.jpg"
        ));
        assert!(!is_tone3000("https://api.tone3000.com.evil.net/a.jpg"));
    }

    #[test]
    fn crops_to_a_square() {
        // 4 × 2: left half red, right half blue; the centre square is 2 × 2
        let mut rgba = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                rgba.extend(if x < 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                });
            }
        }
        let picture = cover(4, 2, &rgba, 2);
        assert_eq!(picture.side, 2);
        assert_eq!(&picture.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&picture.rgba[4..8], &[0, 0, 255, 255]);
        // Shrinking averages
        let one = cover(4, 2, &rgba, 1);
        assert_eq!(&one.rgba, &[127, 0, 127, 255]);
        // Growing repeats pixels
        assert_eq!(cover(1, 1, &[9, 8, 7, 6], 3).rgba.len(), 36);
    }

    /// `cargo test -- --ignored`: fetches a picture from TONE3000
    #[test]
    #[ignore]
    fn fetches_live_pictures() {
        let url = "https://api.tone3000.com/storage/v1/object/public/images/dr16x6nddma.jpg";
        let picture = fetch(url, 96).unwrap();
        assert_eq!((picture.side, picture.rgba.len()), (96, 96 * 96 * 4));
    }

    #[test]
    fn saves_pictures() {
        let picture = cover(1, 1, &[200, 100, 50, 255], 4);
        let path = std::env::temp_dir().join(format!("pedalboard-pic-{}.png", std::process::id()));
        std::fs::write(&path, encode_png(&picture).unwrap()).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((loaded.side, loaded.rgba), (4, picture.rgba));
    }

    #[test]
    fn rejects_other_files() {
        assert!(decode(b"GIF89a").is_err());
        assert!(decode(&[0xff, 0xd8, 0, 1]).is_err());
    }
}
