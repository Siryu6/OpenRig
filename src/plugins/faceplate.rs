//! How an OpenRig FX plugin wants to look: its `openrig:` faceplate terms
//! (`docs/OPENRIG-VIEWS.md`, "Faceplates", in the openrig-fx repository).
//!
//! Three tiers: a plugin without a family is drawn like any LV2 plugin; a
//! basic one gets the OpenRig FX panel; a signature one gets a faceplate in
//! its own colour, finish, wear and knob style. The terms are read only
//! from plugins published under [`TRUSTED_PREFIX`]: a third-party plugin
//! that copies them keeps the generic look, so the OpenRig FX look stays a
//! mark of origin.

use serde::{Deserialize, Serialize};

/// Namespace of the `openrig:` vocabulary
pub const NS: &str = "https://openrig.eu/ns/view#";

/// URIs of the OpenRig FX plugins, the only ones whose faceplate terms count
pub const TRUSTED_PREFIX: &str = "https://openrig.eu/plugins/";

/// Longest tagline and plate name, in characters
pub const TAGLINE_MAX: usize = 48;
pub const PLATE_NAME_MAX: usize = 12;

/// True if the faceplate terms of plugin `uri` are read
pub fn trusted(uri: &str) -> bool {
    uri.starts_with(TRUSTED_PREFIX)
}

/// The view a plugin states (`openrig:view`, with `openrig:dynamics` for
/// a level curve), among those the app draws. A view the app does not draw
/// leaves the view to inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum View {
    Eq,
    Compressor,
    Limiter,
    Gate,
    EchoTimeline,
    /// One bar in steps, from the plugin's `openrig:StepGrid` stream
    StepGrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Family {
    Basic,
    Signature,
}

/// `openrig:knobStyle`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnobStyle {
    #[default]
    Plain,
    Skirted,
    Chickenhead,
    Domed,
}

impl KnobStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Skirted => "skirted",
            Self::Chickenhead => "chickenhead",
            Self::Domed => "domed",
        }
    }
}

/// `openrig:finish`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Finish {
    #[default]
    Painted,
    Brushed,
}

/// `openrig:plateStyle`: how the name is printed on a signature plate
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlateStyle {
    /// Plain silk-screened letters
    #[default]
    Silkscreen,
    /// The letters cut in two thin bands, one shifted right in the accent
    /// colour, one shifted left: a fixed print, never animated
    Glitch,
}

impl PlateStyle {
    /// Name of the style as the UI takes it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silkscreen => "silkscreen",
            Self::Glitch => "glitch",
        }
    }
}

/// The material of a signature plate, from which its picture is drawn
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Plate {
    /// Base colour 0xRRGGBB, already made readable (see [`readable_color`])
    pub color: u32,
    pub knob_style: KnobStyle,
    pub finish: Finish,
    /// Scratches and rubbed edges, 0 (new) to 1
    pub wear: f32,
    /// Seed of the grain and wear, from the URI: the same plate every time
    pub seed: u64,
    /// How its name is printed
    #[serde(default)]
    pub style: PlateStyle,
}

impl Plate {
    /// Knob caps stand out from the plate: a steel cap on a very dark
    /// plate, a black one otherwise
    pub fn steel_caps(&self) -> bool {
        luminance(self.color) < 0.03
    }

    /// The block's colour where the plate is not drawn (value arcs, the
    /// knob strip, rack lines): the plate's colour, lifted toward a light
    /// grey until it shows on the dark stage
    pub fn tint(&self) -> u32 {
        const GREY: u32 = 0xC8C8CC;
        const LEAST: f32 = 0.08;
        let mix = |a: u32, t: f32| -> u32 {
            let channel = |shift: u32| {
                let (x, y) = (
                    ((a >> shift) & 0xff) as f32,
                    ((GREY >> shift) & 0xff) as f32,
                );
                ((x + (y - x) * t).round() as u32) << shift
            };
            channel(16) | channel(8) | channel(0)
        };
        (0..=10)
            .map(|step| mix(self.color, step as f32 / 10.0))
            .find(|&c| luminance(c) >= LEAST)
            .unwrap_or(GREY)
    }
}

/// A trusted plugin's look
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Faceplate {
    /// The plate of a signature plugin; `None` for a basic one
    pub plate: Option<Plate>,
    /// The name on the plate: `openrig:plateName`, else the plugin's name
    /// without its "OR " prefix
    pub name: String,
    /// The name on a rack block: `openrig:plateName`, else the first word
    /// of `name` for a signature plate (it is a short silk name), the whole
    /// of it for a basic block
    pub short_name: String,
    /// `openrig:tagline`, at most [`TAGLINE_MAX`] characters
    pub tagline: String,
    /// `openrig:view`: the view drawn above the controls; `None`: inferred
    #[serde(default)]
    pub view: Option<View>,
}

impl Faceplate {
    pub fn family(&self) -> Family {
        if self.plate.is_some() {
            Family::Signature
        } else {
            Family::Basic
        }
    }

    /// Name of the look its block is drawn with, as the UI takes it
    /// ("basic" or "signature"; a plugin without a face has the generic
    /// look, "")
    pub fn look(&self) -> &'static str {
        match self.family() {
            Family::Basic => "basic",
            Family::Signature => "signature",
        }
    }
}

/// The faceplate terms of a plugin as written, before any check: local
/// names of the `openrig:` resources ("Signature", "Skirted"…), literals
/// as they are
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Terms {
    pub family: Option<String>,
    pub face_color: Option<String>,
    pub knob_style: Option<String>,
    pub finish: Option<String>,
    pub wear: Option<f32>,
    pub plate_name: Option<String>,
    pub plate_style: Option<String>,
    pub tagline: Option<String>,
    pub view: Option<String>,
    pub dynamics: Option<String>,
}

impl Terms {
    /// The look of plugin `uri` named `name`. `None` (the generic look)
    /// for an untrusted URI or without a known family. A signature plugin
    /// without a usable colour falls back to the basic look; unknown values
    /// fall back to their default.
    pub fn faceplate(&self, uri: &str, name: &str) -> Option<Faceplate> {
        if !trusted(uri) {
            return None;
        }
        let signature = match self.family.as_deref()? {
            "Basic" => false,
            "Signature" => true,
            _ => return None,
        };
        let plate = signature
            .then(|| self.face_color.as_deref().and_then(parse_color))
            .flatten()
            .map(|color| Plate {
                color: readable_color(color),
                knob_style: match self.knob_style.as_deref() {
                    Some("Skirted") => KnobStyle::Skirted,
                    Some("Chickenhead") => KnobStyle::Chickenhead,
                    Some("Domed") => KnobStyle::Domed,
                    _ => KnobStyle::Plain,
                },
                finish: match self.finish.as_deref() {
                    Some("Brushed") => Finish::Brushed,
                    _ => Finish::Painted,
                },
                wear: self
                    .wear
                    .filter(|w| w.is_finite())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                seed: seed(uri),
                style: match self.plate_style.as_deref() {
                    Some("Glitch") => PlateStyle::Glitch,
                    _ => PlateStyle::Silkscreen,
                },
            });
        let plate_name = self
            .plate_name
            .as_deref()
            .map(|n| clip(n.trim(), PLATE_NAME_MAX))
            .filter(|n| !n.is_empty());
        let stripped = name.strip_prefix("OR ").unwrap_or(name).trim().to_string();
        let full = plate_name.clone().unwrap_or_else(|| stripped.clone());
        let short_name = match (&plate_name, plate.is_some()) {
            (Some(n), _) => n.clone(),
            (None, true) => stripped
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string(),
            (None, false) => stripped,
        };
        let tagline = self
            .tagline
            .as_deref()
            .map(|t| clip(t.trim().trim_end_matches('.'), TAGLINE_MAX))
            .unwrap_or_default();
        let view = match (self.view.as_deref(), self.dynamics.as_deref()) {
            (Some("EqView"), _) => Some(View::Eq),
            (Some("DynamicsView"), Some("Compressor")) => Some(View::Compressor),
            (Some("DynamicsView"), Some("Limiter")) => Some(View::Limiter),
            (Some("DynamicsView"), Some("Gate" | "Expander")) => Some(View::Gate),
            (Some("EchoTimelineView"), _) => Some(View::EchoTimeline),
            (Some("StepGridView"), _) => Some(View::StepGrid),
            _ => None,
        };
        Some(Faceplate {
            plate,
            name: full,
            short_name,
            tagline,
            view,
        })
    }
}

/// At most `max` characters of `text`
fn clip(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// "#RRGGBB" (the `#` optional) as 0xRRGGBB
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Seed of a plate's grain and wear: FNV-1a of the URI, stable across
/// builds and machines
pub fn seed(uri: &str) -> u64 {
    uri.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// sRGB channels of 0xRRGGBB, 0 to 1
fn channels(color: u32) -> [f32; 3] {
    [
        ((color >> 16) & 0xff) as f32 / 255.0,
        ((color >> 8) & 0xff) as f32 / 255.0,
        (color & 0xff) as f32 / 255.0,
    ]
}

fn pack(c: [f32; 3]) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
}

/// Relative luminance (WCAG) of 0xRRGGBB
pub fn luminance(color: u32) -> f32 {
    let linear = |v: f32| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = channels(color);
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// Luminance of the silk-screened labels (`Theme.plate-silk`, #ECE5D0)
const SILK_LUMINANCE: f32 = 0.78;
/// Contrast kept between the silk and the plate (WCAG AA for text)
const MIN_CONTRAST: f32 = 4.5;

/// The plate colour the host draws: an amber-like saturated colour is
/// toned down (amber means value, focus and selection in the app), and a
/// light one is darkened until the silk labels read on it
pub fn readable_color(color: u32) -> u32 {
    let [r, g, b] = channels(color);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let mut c = [r, g, b];
    if max - min > 0.0 {
        let hue = if max == r {
            60.0 * ((g - b) / (max - min)).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / (max - min) + 2.0)
        } else {
            60.0 * ((r - g) / (max - min) + 4.0)
        };
        let saturation = (max - min) / max;
        if (20.0..=55.0).contains(&hue) && saturation > 0.45 && max > 0.35 {
            // Halfway to grey of the same value
            let grey = (r + g + b) / 3.0;
            c = c.map(|v| v + (grey - v) * 0.6);
        }
    }
    let mut out = pack(c);
    for _ in 0..40 {
        if (SILK_LUMINANCE + 0.05) / (luminance(out) + 0.05) >= MIN_CONTRAST {
            break;
        }
        c = c.map(|v| v * 0.94);
        out = pack(c);
    }
    out
}

/// A tiny drawing of what a plugin does, for its basic rack block: SVG path
/// commands in a 50 x 14 box (echo taps, a reverb tail, a level curve…)
pub fn glyph(category: &super::scanner::PluginCategory) -> &'static str {
    use super::scanner::PluginCategory as C;
    match category {
        C::Delay => {
            "M 2 14 L 2 1 M 14 14 L 14 5 M 26 14 L 26 8 M 38 14 L 38 10.5 M 50 14 L 50 12.5"
        }
        C::Reverb => "M 0 1 C 10 1 14 10 24 12 C 34 13.5 42 13.8 50 14",
        C::Dynamics => "M 0 14 L 24 4 Q 30 1.8 50 0",
        C::Filter => "M 0 9 C 12 9 16 2 25 2 C 34 2 38 9 50 9",
        C::Distortion => "M 0 13 C 10 13 14 2 25 2 L 50 2",
        C::Modulation => {
            "M 0 7 C 6 0 10 0 12.5 7 C 15 14 19 14 25 7 C 31 0 35 0 37.5 7 C 40 14 44 14 50 7"
        }
        // A note held, then a step up
        C::Pitch => "M 0 11 L 16 11 L 22 3 L 50 3",
        _ => "M 0 7 L 50 7",
    }
}

/// xorshift64*: the grain and wear of a plate, the same for a seed
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 to 1
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

impl Plate {
    /// The plate's picture at `w` x `h` pixels, opaque RGBA rows: its
    /// colour lit from above, grain, brushed streaks, scratches and rubbed
    /// edges in one image, so the UI draws a single picture under its
    /// knobs. Wear is drawn at the real size: scratches keep their length
    /// in pixels whatever the size. Small plates (rack blocks, seen from
    /// further away) get finer detail.
    pub fn render(&self, w: u32, h: u32) -> Vec<u8> {
        let (w, h) = (w.max(1) as usize, h.max(1) as usize);
        let small = w.min(h) < 200;
        let brushed = self.finish == Finish::Brushed;
        let grain = if brushed { 0.8 } else { 1.0 } * if small { 0.45 } else { 1.0 };
        let mut rng = Rng(self.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        // Lightness added to the base colour, per pixel
        let mut light = vec![0f32; w * h];

        // Light from above: +18 % at the top, -30 % at the bottom
        for (y, row) in light.chunks_exact_mut(w).enumerate() {
            let t = y as f32 / h as f32;
            let f = if t < 0.4 {
                0.18 * (1.0 - t / 0.4)
            } else {
                -0.30 * (t - 0.4) / 0.6
            };
            row.fill(f * 60.0);
        }
        // Grain: sparse light and dark specks
        for v in light.iter_mut() {
            let r = rng.unit();
            if r < 0.10 {
                *v += 14.0 * grain * rng.unit();
            } else if r < 0.32 {
                *v -= 16.0 * grain * rng.unit();
            }
        }
        // Brushed: noise smeared along x, a different strength per row
        if brushed {
            const RUN: usize = 24;
            let mut noise = vec![0f32; w + RUN];
            for row in light.chunks_exact_mut(w) {
                let gain = 0.5 + rng.unit();
                noise.iter_mut().for_each(|n| *n = rng.unit() - 0.5);
                let mut sum: f32 = noise.iter().take(RUN).sum();
                for (x, v) in row.iter_mut().enumerate() {
                    *v += sum / RUN as f32 * 70.0 * gain;
                    sum += noise.get(x + RUN).copied().unwrap_or(0.0)
                        - noise.get(x).copied().unwrap_or(0.0);
                }
            }
        }
        // Wear: thin light scratches, a density per area
        let scratches = ((w * h) as f32 / 2600.0 * self.wear) as usize;
        for _ in 0..scratches {
            let (x0, y0) = (rng.unit() * w as f32, rng.unit() * h as f32);
            let angle = rng.unit() * std::f32::consts::PI;
            let len = (6.0 + rng.unit() * 40.0).min(w.min(h) as f32 * 0.35);
            let strength = (20.0 + rng.unit() * 45.0) * if small { 0.6 } else { 1.0 };
            let (dx, dy) = (angle.cos(), angle.sin());
            let mut t = 0.0;
            while t < len {
                let (x, y) = (x0 + t * dx, y0 + t * dy);
                if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
                    let fade = 1.0 - (t / len - 0.5).abs() * 2.0;
                    if let Some(v) = light.get_mut(y as usize * w + x as usize) {
                        *v = v.max(strength * fade);
                    }
                }
                t += 0.7;
            }
        }
        // Rubbed rim: the paint worn through near the edges
        let rim = (w.min(h) as f32 * 0.03).clamp(3.0, 10.0);
        for (y, row) in light.chunks_exact_mut(w).enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                let d = x.min(y).min(w - 1 - x).min(h - 1 - y) as f32;
                if d < rim && rng.unit() < (rim - d) / rim * 0.6 * self.wear {
                    *v = v.max(30.0 + rng.unit() * 50.0);
                }
            }
        }

        let base = [
            (self.color >> 16) & 0xff,
            (self.color >> 8) & 0xff,
            self.color & 0xff,
        ]
        .map(|c| c as f32);
        let mut pixels = Vec::with_capacity(w * h * 4);
        for l in light {
            pixels.extend(base.map(|b| (b + l).clamp(0.0, 255.0) as u8));
            pixels.push(255);
        }
        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREEN: &str = "https://openrig.eu/plugins/green-drive";

    fn green_terms() -> Terms {
        Terms {
            family: Some("Signature".into()),
            face_color: Some("#2E5A3A".into()),
            knob_style: Some("Skirted".into()),
            tagline: Some("The mid-hump overdrive, in circuit".into()),
            ..Terms::default()
        }
    }

    #[test]
    fn green_drive_is_a_skirted_green_plate() {
        let face = green_terms().faceplate(GREEN, "OR Green Drive").unwrap();
        let plate = face.plate.unwrap();
        assert_eq!(face.family(), Family::Signature);
        assert_eq!(plate.color, 0x2E5A3A);
        assert_eq!(plate.knob_style, KnobStyle::Skirted);
        assert_eq!(plate.finish, Finish::Painted);
        assert_eq!(plate.wear, 0.0);
        assert!(!plate.steel_caps());
        assert_eq!(face.name, "Green Drive");
        assert_eq!(face.short_name, "Green");
        assert_eq!(face.tagline, "The mid-hump overdrive, in circuit");
    }

    #[test]
    fn a_plate_prints_its_name_in_its_style() {
        let style = |style: Option<&str>| {
            let terms = Terms {
                plate_style: style.map(str::to_string),
                ..green_terms()
            };
            terms
                .faceplate(GREEN, "OR Green Drive")
                .and_then(|f| f.plate)
                .map(|p| p.style)
        };
        assert_eq!(style(Some("Glitch")), Some(PlateStyle::Glitch));
        // Silk-screened unless stated, or of a style the app does not know
        assert_eq!(style(None), Some(PlateStyle::Silkscreen));
        assert_eq!(style(Some("Silkscreen")), Some(PlateStyle::Silkscreen));
        assert_eq!(style(Some("Neon")), Some(PlateStyle::Silkscreen));
        assert_eq!(PlateStyle::Glitch.as_str(), "glitch");
    }

    #[test]
    fn the_stated_view_is_kept_when_the_app_draws_it() {
        let view = |view: &str, dynamics: Option<&str>| {
            let terms = Terms {
                view: Some(view.into()),
                dynamics: dynamics.map(str::to_string),
                ..green_terms()
            };
            terms
                .faceplate(GREEN, "OR Green Drive")
                .and_then(|f| f.view)
        };
        assert_eq!(view("EqView", None), Some(View::Eq));
        assert_eq!(view("DynamicsView", Some("Gate")), Some(View::Gate));
        assert_eq!(view("EchoTimelineView", None), Some(View::EchoTimeline));
        assert_eq!(view("StepGridView", None), Some(View::StepGrid));
        // Not drawn by the app, or a level curve of no known kind
        assert_eq!(view("LfoView", None), None);
        assert_eq!(view("DynamicsView", None), None);
        assert_eq!(
            green_terms()
                .faceplate(GREEN, "OR Green Drive")
                .and_then(|f| f.view),
            None
        );
    }

    #[test]
    fn untrusted_uris_keep_the_generic_look() {
        assert_eq!(green_terms().faceplate("urn:copy:green", "Green"), None);
        assert_eq!(
            green_terms().faceplate("http://openrig.eu/plugins/green-drive", "Green"),
            None
        );
    }

    #[test]
    fn missing_or_unknown_family_is_generic() {
        let mut terms = green_terms();
        terms.family = None;
        assert_eq!(terms.faceplate(GREEN, "OR Green Drive"), None);
        terms.family = Some("Deluxe".into());
        assert_eq!(terms.faceplate(GREEN, "OR Green Drive"), None);
    }

    #[test]
    fn signature_without_colour_falls_back_to_basic() {
        let mut terms = green_terms();
        terms.face_color = Some("green".into());
        let face = terms.faceplate(GREEN, "OR Green Drive").unwrap();
        assert_eq!(face.family(), Family::Basic);
        assert_eq!(face.short_name, "Green Drive");
    }

    #[test]
    fn proposed_terms_are_read_and_bounded() {
        let mut terms = green_terms();
        terms.finish = Some("Brushed".into());
        terms.wear = Some(3.0);
        terms.plate_name = Some("SPLINTER GLITCH UNIT".into());
        terms.tagline = Some(format!("{}.", "x".repeat(60)));
        terms.knob_style = Some("Mystery".into());
        let face = terms.faceplate(GREEN, "OR Splinter").unwrap();
        let plate = face.plate.unwrap();
        assert_eq!(plate.finish, Finish::Brushed);
        assert_eq!(plate.wear, 1.0);
        assert_eq!(plate.knob_style, KnobStyle::Plain);
        assert_eq!(face.name, "SPLINTER GLI");
        assert_eq!(face.short_name, "SPLINTER GLI");
        assert_eq!(face.tagline.chars().count(), TAGLINE_MAX);
    }

    #[test]
    fn seeds_are_stable_and_differ_per_plugin() {
        assert_eq!(seed(GREEN), seed(GREEN));
        assert_ne!(seed(GREEN), seed("https://openrig.eu/plugins/black-dist"));
        // FNV-1a test vector
        assert_eq!(seed("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn colours_are_parsed() {
        assert_eq!(parse_color("#2E5A3A"), Some(0x2E5A3A));
        assert_eq!(parse_color("1e1e21"), Some(0x1E1E21));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#GG0000"), None);
    }

    #[test]
    fn plates_stay_readable_and_away_from_amber() {
        // Dark plates are kept as they are
        assert_eq!(readable_color(0x2E5A3A), 0x2E5A3A);
        assert_eq!(readable_color(0x1E1E21), 0x1E1E21);
        // A white plate is darkened until the silk reads on it
        let white = readable_color(0xFFFFFF);
        assert!((SILK_LUMINANCE + 0.05) / (luminance(white) + 0.05) >= MIN_CONTRAST);
        // The accent itself is no plate colour
        let amber = readable_color(0xF0A43C);
        let [r, g, b] = channels(amber);
        assert!((r.max(g).max(b) - r.min(g).min(b)) / r.max(g).max(b) < 0.45);
        // A dark steel plate gets steel caps, green keeps black ones
        let steel = Plate {
            color: 0x1E1E21,
            knob_style: KnobStyle::Domed,
            finish: Finish::Painted,
            wear: 0.0,
            seed: 0,
            style: PlateStyle::Silkscreen,
        };
        assert!(steel.steel_caps());
    }

    #[test]
    fn plates_render_the_same_every_time() {
        let plate = Plate {
            color: 0x2E5A3A,
            knob_style: KnobStyle::Skirted,
            finish: Finish::Painted,
            wear: 0.55,
            seed: seed("https://openrig.eu/plugins/green-drive"),
            style: PlateStyle::Silkscreen,
        };
        let a = plate.render(120, 80);
        assert_eq!(a.len(), 120 * 80 * 4);
        assert_eq!(a, plate.render(120, 80));
        assert!(a.iter().skip(3).step_by(4).all(|&alpha| alpha == 255));
        // Lit from above
        let row = |y: usize| -> u32 { a[y * 480..(y + 1) * 480].iter().map(|&c| c as u32).sum() };
        assert!(row(10) > row(70));
        // Brushed and worn plates differ from painted and new ones
        let brushed = Plate {
            finish: Finish::Brushed,
            ..plate
        };
        assert_ne!(brushed.render(120, 80), a);
        let new = Plate { wear: 0.0, ..plate };
        assert_ne!(new.render(120, 80), a);
        // Degenerate sizes do not panic
        assert_eq!(plate.render(0, 0).len(), 4);
        assert_eq!(plate.render(1, 300).len(), 300 * 4);
    }

    #[test]
    fn tints_show_on_the_stage() {
        let plate = |color| Plate {
            color,
            knob_style: KnobStyle::Plain,
            finish: Finish::Painted,
            wear: 0.0,
            seed: 1,
            style: PlateStyle::Silkscreen,
        };
        // A mid colour stays as it is, a near-black one turns grey
        assert_eq!(plate(0x2E7A4A).tint(), 0x2E7A4A);
        let black = plate(0x1E1E21).tint();
        assert_ne!(black, 0x1E1E21);
        assert!(luminance(black) >= 0.08);
    }
}
