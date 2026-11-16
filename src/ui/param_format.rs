//! A parameter's value as shown on knobs and sliders ("12.5 dB", "40%",
//! "1 beat", "2 beats")

/// Units with a singular and a plural form: (singular, plural)
const PLURALS: &[(&str, &str)] = &[
    ("beat", "beats"),
    ("bar", "bars"),
    ("frame", "frames"),
    ("sample", "samples"),
];

/// `value` with its `unit`, rounded for display: whole numbers for `int`
/// parameters and from 100 up, one decimal from 10, two below; kHz from
/// 1000 Hz; the singular unit for exactly 1 ("1 beat", "0.5 beats")
pub fn format(value: f32, kind: &str, unit: &str) -> String {
    if unit == "Hz" && value >= 1000.0 {
        return format!("{} kHz", number((value / 100.0).round() / 10.0));
    }
    let rounded = if kind == "int" || value.abs() >= 100.0 {
        value.round()
    } else if value.abs() >= 10.0 {
        (value * 10.0).round() / 10.0
    } else {
        (value * 100.0).round() / 100.0
    };
    let text = number(rounded);
    match unit {
        "" => text,
        "%" => format!("{text}%"),
        unit => format!("{text} {}", counted(unit, rounded.abs() == 1.0)),
    }
}

/// The singular or plural form of `unit`; other units as they are
fn counted(unit: &str, one: bool) -> &str {
    PLURALS
        .iter()
        .find(|(singular, plural)| unit == *singular || unit == *plural)
        .map_or(
            unit,
            |(singular, plural)| if one { singular } else { plural },
        )
}

/// A rounded number without trailing zeros ("1", "0.5"), never "-0"
fn number(value: f32) -> String {
    format!("{}", value + 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_are_singular_only_for_one() {
        assert_eq!(format(1.0, "float", "beats"), "1 beat");
        assert_eq!(format(1.001, "float", "beats"), "1 beat");
        assert_eq!(format(0.5, "float", "beats"), "0.5 beats");
        assert_eq!(format(2.0, "float", "beats"), "2 beats");
        assert_eq!(format(0.0, "float", "beats"), "0 beats");
        assert_eq!(format(1.0, "int", "beat"), "1 beat");
        assert_eq!(format(4.0, "int", "beat"), "4 beats");
    }

    #[test]
    fn bars_and_frames_follow_the_count() {
        assert_eq!(format(1.0, "int", "bars"), "1 bar");
        assert_eq!(format(8.0, "int", "bars"), "8 bars");
        assert_eq!(format(1.0, "int", "frames"), "1 frame");
        assert_eq!(format(256.0, "int", "frames"), "256 frames");
    }

    #[test]
    fn other_units_keep_their_symbol() {
        assert_eq!(format(1.0, "float", "dB"), "1 dB");
        assert_eq!(format(-12.345, "float", "dB"), "-12.3 dB");
        assert_eq!(format(40.0, "float", "%"), "40%");
        assert_eq!(format(1500.0, "float", "Hz"), "1.5 kHz");
        assert_eq!(format(440.0, "float", "Hz"), "440 Hz");
        assert_eq!(format(0.254, "float", ""), "0.25");
        assert_eq!(format(-0.001, "float", "ms"), "0 ms");
    }
}
