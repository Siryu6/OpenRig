//! Guitar tuner: pitch detection by autocorrelation

/// Below the lowest string of an 8-string in drop E or of a bass (E1, 41 Hz),
/// so that 7-strings in drop A (A1, 55 Hz) are covered too
const MIN_FREQ: f32 = 38.0;
const MAX_FREQ: f32 = 1200.0; // High harmonics
/// Detection works on the signal decimated by this factor: plenty for
/// frequencies below MAX_FREQ, and four times cheaper
const DECIMATION: usize = 2;

// Note names for chromatic scale
const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

#[derive(Debug, Clone)]
pub struct TunerResult {
    pub frequency: f32,
    pub note_name: &'static str,
    pub octave: i32,
    pub cents: f32,
    pub is_tuned: bool,
}

impl Default for TunerResult {
    fn default() -> Self {
        Self {
            frequency: 440.0,
            note_name: "A",
            octave: 4,
            cents: 0.0,
            is_tuned: false,
        }
    }
}

pub struct Tuner {
    sample_rate: f32,
    /// Last samples at full rate (ring buffer), long enough for two periods
    /// of the lowest note whatever the JACK buffer size
    input_buffer: Vec<f32>,
    buffer_index: usize,
    /// Decimated window in time order and correlation per period, both
    /// pre-allocated (detection runs in the audio callback)
    window: Vec<f32>,
    correlation: Vec<f32>,
    // Tuning reference (A4 frequency)
    reference_pitch: f32,
    // Mute output while tuning
    mute_enabled: bool,
    // Pitch detection only runs while the tuner is shown (it is costly)
    active: bool,
    samples_since_detect: usize,
}

impl Tuner {
    pub fn new(sample_rate: f32) -> Self {
        let max_period = (sample_rate / MIN_FREQ) as usize;
        let length = (2 * max_period).next_power_of_two();

        Self {
            sample_rate,
            input_buffer: vec![0.0; length],
            buffer_index: 0,
            window: vec![0.0; length / DECIMATION],
            correlation: vec![0.0; max_period / DECIMATION + 2],
            reference_pitch: 440.0,
            mute_enabled: false,
            active: false,
            samples_since_detect: 0,
        }
    }

    /// Enable/disable pitch detection (enable only while the tuner is visible)
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Process audio with optional mute
    pub fn process_with_mute(&mut self, input: &[f32], output: &mut [f32]) -> Option<TunerResult> {
        let result = self.process(input);

        if self.mute_enabled {
            // Mute output while tuning
            output.fill(0.0);
        } else {
            // Pass through
            output.copy_from_slice(input);
        }

        result
    }

    /// Set mute on/off
    pub fn set_mute(&mut self, enabled: bool) {
        self.mute_enabled = enabled;
    }

    /// Get mute status
    pub fn is_muted(&self) -> bool {
        self.mute_enabled
    }

    /// Set reference pitch (default 440 Hz for A4)
    pub fn set_reference_pitch(&mut self, frequency: f32) {
        self.reference_pitch = frequency.clamp(430.0, 450.0);
    }

    /// Get reference pitch
    pub fn reference_pitch(&self) -> f32 {
        self.reference_pitch
    }

    /// Process audio samples and detect pitch
    pub fn process(&mut self, samples: &[f32]) -> Option<TunerResult> {
        // Accumulate samples
        for &sample in samples {
            self.input_buffer[self.buffer_index] = sample;
            self.buffer_index += 1;

            if self.buffer_index >= self.input_buffer.len() {
                self.buffer_index = 0;
            }
        }

        // Detect pitch about twenty times a second, whatever the buffer size
        if !self.active {
            return None;
        }
        self.samples_since_detect += samples.len();
        if self.samples_since_detect < (self.sample_rate / 20.0) as usize {
            return None;
        }
        self.samples_since_detect = 0;
        if let Some(frequency) = self.detect_pitch() {
            if (MIN_FREQ..=MAX_FREQ).contains(&frequency) {
                let (note_name, octave, cents) = self.frequency_to_note(frequency);
                let is_tuned = cents.abs() < 5.0;

                return Some(TunerResult {
                    frequency,
                    note_name,
                    octave,
                    cents,
                    is_tuned,
                });
            }
        }

        None
    }

    /// Normalised autocorrelation on the decimated window
    fn detect_pitch(&mut self) -> Option<f32> {
        // Unwrap the ring buffer (oldest sample first) while decimating
        let length = self.input_buffer.len();
        for (j, out) in self.window.iter_mut().enumerate() {
            let i = (self.buffer_index + j * DECIMATION) % length;
            *out = 0.5 * (self.input_buffer[i] + self.input_buffer[(i + 1) % length]);
        }
        let x = &self.window;

        // Signal too weak
        let energy: f32 = x.iter().map(|&v| v * v).sum();
        if (energy / x.len() as f32).sqrt() < 0.01 {
            return None;
        }

        let rate = self.sample_rate / DECIMATION as f32;
        let min_period = ((rate / MAX_FREQ) as usize).max(2);
        let max_period = self.correlation.len() - 2;
        // Every period is compared over the same number of samples
        let n = x.len() - max_period - 1;
        let head: f32 = x[..n].iter().map(|&v| v * v).sum();
        let mut lagged: f32 = x[min_period - 1..n + min_period - 1]
            .iter()
            .map(|&v| v * v)
            .sum();
        let mut best = 0.0f32;
        for period in min_period - 1..=max_period + 1 {
            if period >= min_period {
                let (gone, new) = (x[period - 1], x[n + period - 1]);
                lagged += new * new - gone * gone;
            }
            let dot = dot(&x[..n], &x[period..period + n]);
            let r = dot / (head * lagged.max(0.0)).sqrt().max(f32::EPSILON);
            self.correlation[period + 1 - min_period] = r;
            if (min_period..=max_period).contains(&period) {
                best = best.max(r);
            }
        }
        if best < 0.5 {
            return None;
        }

        // The first peak close to the best one: its multiples correlate as
        // well, and would read an octave too low
        let c = &self.correlation;
        let period = (min_period..=max_period).find(|&p| {
            let i = p + 1 - min_period;
            c[i] >= 0.9 * best && c[i] >= c[i - 1] && c[i] >= c[i + 1]
        })?;

        // Parabolic interpolation between neighbouring periods
        let i = period + 1 - min_period;
        let (a, b, d) = (c[i - 1], c[i], c[i + 1]);
        let curve = a - 2.0 * b + d;
        let shift = if curve < 0.0 {
            (0.5 * (a - d) / curve).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        Some(rate / (period as f32 + shift))
    }

    /// Convert frequency to note name, octave, and cents offset
    fn frequency_to_note(&self, frequency: f32) -> (&'static str, i32, f32) {
        // Use configured reference pitch (default A4 = 440 Hz)
        const A4_MIDI: f32 = 69.0;

        // Convert to MIDI note number (fractional)
        let midi_note = 12.0 * (frequency / self.reference_pitch).log2() + A4_MIDI;

        // Round to nearest semitone
        let nearest_note = midi_note.round();
        let note_index = (nearest_note as i32) % 12;
        let octave = (nearest_note as i32) / 12 - 1;

        // Calculate cents offset (-50 to +50)
        let cents = (midi_note - nearest_note) * 100.0;

        let note_name = NOTE_NAMES[note_index as usize];

        (note_name, octave, cents)
    }
}

/// Dot product of two slices of the same length. Eight running sums let the
/// compiler use SIMD (a single float sum must be added in order); this is
/// most of the tuner's work, run on the audio thread.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut sums = [0.0f32; 8];
    let ((a_chunks, a_tail), (b_chunks, b_tail)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for ((sum, x), y) in sums.iter_mut().zip(x).zip(y) {
            *sum += x * y;
        }
    }
    sums.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frequency_to_note() {
        let tuner = Tuner::new(48000.0);
        // A4 = 440 Hz
        let (note, octave, cents) = tuner.frequency_to_note(440.0);
        assert_eq!(note, "A");
        assert_eq!(octave, 4);
        assert!(cents.abs() < 1.0);

        // E2 = 82.41 Hz (low E string)
        let (note, octave, _) = tuner.frequency_to_note(82.41);
        assert_eq!(note, "E");
        assert_eq!(octave, 2);

        // E4 = 329.63 Hz (high E string)
        let (note, octave, _) = tuner.frequency_to_note(329.63);
        assert_eq!(note, "E");
        assert_eq!(octave, 4);
    }

    #[test]
    fn detects_a_sine_only_when_active() {
        let mut tuner = Tuner::new(48000.0);
        let sine: Vec<f32> = (0..512 * 12)
            .map(|i| (2.0 * std::f32::consts::PI * 110.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();

        assert!(sine.chunks(512).all(|block| tuner.process(block).is_none()));

        tuner.set_active(true);
        let result = sine
            .chunks(512)
            .filter_map(|block| tuner.process(block))
            .last()
            .expect("pitch detected");
        assert_eq!(result.note_name, "A");
        assert_eq!(result.octave, 2);
    }

    fn detect(buffer_size: usize, frequency: f32) -> TunerResult {
        let mut tuner = Tuner::new(48000.0);
        tuner.set_active(true);
        let signal: Vec<f32> = (0..48000)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * frequency * i as f32 / 48000.0;
                // A string has strong harmonics
                0.4 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.2 * (3.0 * phase).sin()
            })
            .collect();
        signal
            .chunks(buffer_size)
            .filter_map(|block| tuner.process(block))
            .last()
            .expect("pitch detected")
    }

    #[test]
    fn detects_every_string_at_small_buffer_sizes() {
        for buffer_size in [64, 128, 1024] {
            for (frequency, name, octave) in [
                (41.2, "E", 1),
                (55.0, "A", 1),
                (61.74, "B", 1),
                (82.41, "E", 2),
                (110.0, "A", 2),
                (146.83, "D", 3),
                (196.0, "G", 3),
                (246.94, "B", 3),
                (329.63, "E", 4),
            ] {
                let result = detect(buffer_size, frequency);
                assert_eq!((result.note_name, result.octave), (name, octave));
                assert!(result.cents.abs() < 3.0, "{frequency} Hz: {}", result.cents);
            }
        }
    }
}
