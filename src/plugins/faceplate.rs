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
}

impl Plate {
    /// Knob caps stand out from the plate: a steel cap on a very dark
    /// plate, a black one otherwise
    pub fn steel_caps(&self) -> bool {
        luminance(self.color) < 0.03
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
}

/// Name of the look a block is drawn with: "" (generic), "basic" or
/// "signature", as the UI takes it
pub fn look(face: Option<&Faceplate>) -> &'static str {
    match face.map(Faceplate::family) {
        None => "",
        Some(Family::Basic) => "basic",
        Some(Family::Signature) => "signature",
    }
}

impl Faceplate {
    pub fn family(&self) -> Family {
        if self.plate.is_some() {
            Family::Signature
        } else {
            Family::Basic
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
    pub tagline: Option<String>,
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
        Some(Faceplate {
            plate,
            name: full,
            short_name,
            tagline,
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
        _ => "M 0 7 L 50 7",
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
        terms.plate_name = Some("FRACTURE GLITCH UNIT".into());
        terms.tagline = Some(format!("{}.", "x".repeat(60)));
        terms.knob_style = Some("Mystery".into());
        let face = terms.faceplate(GREEN, "OR Fracture").unwrap();
        let plate = face.plate.unwrap();
        assert_eq!(plate.finish, Finish::Brushed);
        assert_eq!(plate.wear, 1.0);
        assert_eq!(plate.knob_style, KnobStyle::Plain);
        assert_eq!(face.name, "FRACTURE GLI");
        assert_eq!(face.short_name, "FRACTURE GLI");
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
        };
        assert!(steel.steel_caps());
    }
}
