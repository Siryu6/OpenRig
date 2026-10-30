//! A parameter's next value when the encoder turns while it is adjusted

/// Detents for the whole range of a continuous parameter
const DETENTS: f32 = 100.0;

/// The value `steps` detents away (negative: down) from `value`:
/// - `float`: a hundredth of the range per detent, on a log scale for
///   frequencies (equal steps per octave);
/// - `int`: one per detent;
/// - `enum`: the next option of `options` (their values, in order);
/// - `toggle`: the other state, whatever the direction.
pub fn step(kind: &str, min: f32, max: f32, value: f32, options: &[f32], steps: i32) -> f32 {
    let clamp = |v: f32| v.clamp(min.min(max), max.max(min));
    match kind {
        "toggle" => {
            if value > (min + max) / 2.0 {
                min
            } else {
                max
            }
        }
        "int" => clamp(value.round() + steps as f32),
        "enum" => {
            let Some(current) = options
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1 - value).abs().total_cmp(&(b.1 - value).abs()))
                .map(|(i, _)| i as i32)
            else {
                return value;
            };
            let next = (current + steps).clamp(0, options.len() as i32 - 1);
            options[next as usize]
        }
        _ if min > 0.0 && max / min >= 100.0 => {
            // Hz and the like: a ratio per detent
            clamp(value.max(min) * (max / min).powf(steps as f32 / DETENTS))
        }
        _ => clamp(value + (max - min) * steps as f32 / DETENTS),
    }
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn continuous_values_move_by_a_hundredth_and_stay_in_range() {
        assert_eq!(step("float", -20.0, 20.0, 0.0, &[], 1), 0.4);
        assert_eq!(step("float", -20.0, 20.0, 0.0, &[], -3), -1.2);
        assert_eq!(step("float", 0.0, 1.0, 0.995, &[], 2), 1.0);
        assert_eq!(step("", 0.0, 1.0, 0.0, &[], -1), 0.0);
    }

    #[test]
    fn frequencies_move_by_a_ratio() {
        // 20 Hz to 20 kHz: 100 detents, ~7 % each
        let up = step("float", 20.0, 20000.0, 1000.0, &[], 1);
        assert!((up / 1000.0 - 1000f32.powf(0.01)).abs() < 1e-4);
        let back = step("float", 20.0, 20000.0, up, &[], -1);
        assert!((back - 1000.0).abs() < 0.01);
    }

    #[test]
    fn integers_enums_and_toggles() {
        assert_eq!(step("int", 0.0, 10.0, 3.0, &[], 2), 5.0);
        assert_eq!(step("int", 0.0, 10.0, 10.0, &[], 1), 10.0);
        let options = [0.0, 2.0, 5.0];
        assert_eq!(step("enum", 0.0, 5.0, 2.0, &options, 1), 5.0);
        assert_eq!(step("enum", 0.0, 5.0, 5.0, &options, 1), 5.0);
        assert_eq!(step("enum", 0.0, 5.0, 2.0, &options, -1), 0.0);
        assert_eq!(step("enum", 0.0, 5.0, 1.0, &[], 1), 1.0);
        assert_eq!(step("toggle", 0.0, 1.0, 0.0, &[], -1), 1.0);
        assert_eq!(step("toggle", 0.0, 1.0, 1.0, &[], 1), 0.0);
    }
}
