//! Neural amp models (NAM `.nam`, RTNeural keras `.json` / `.aidax`) run
//! through NeuralAudio, built from `native/` by build.rs.

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;

use anyhow::{anyhow, Result};

extern "C" {
    fn pb_nam_load(
        path: *const c_char,
        sample_rate: c_int,
        max_block: c_int,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn pb_nam_free(model: *mut c_void);
    fn pb_nam_process(model: *mut c_void, input: *const f32, output: *mut f32, frames: usize);
    fn pb_nam_sample_rate(model: *mut c_void) -> f32;
    fn pb_nam_input_adjustment(model: *mut c_void) -> f32;
    fn pb_nam_output_adjustment(model: *mut c_void) -> f32;
    fn pb_nam_has_quality(model: *mut c_void) -> bool;
    fn pb_nam_set_quality(model: *mut c_void, quality: f32);
}

/// Quality values of the block's Quality control
pub const QUALITY_FULL: f32 = 1.0;
pub const QUALITY_LIGHT: f32 = 0.0;

/// A loaded model, ready to process audio
pub struct NamModel {
    handle: *mut c_void,
    max_block: usize,
    has_quality: bool,
}

// The model is only used from one thread at a time.
unsafe impl Send for NamModel {}

impl NamModel {
    /// Load and prewarm a model. Not real-time safe.
    pub fn load(path: &Path, sample_rate: f32, max_block: usize) -> Result<Self> {
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| anyhow!("Invalid model path {}", path.display()))?;
        let mut error = [0 as c_char; 512];
        let handle = unsafe {
            pb_nam_load(
                c_path.as_ptr(),
                sample_rate.round() as c_int,
                max_block as c_int,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if handle.is_null() {
            let message = unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) };
            return Err(anyhow!(
                "Cannot load model {}: {}",
                path.display(),
                message.to_string_lossy()
            ));
        }
        let has_quality = unsafe { pb_nam_has_quality(handle) };
        Ok(Self {
            handle,
            max_block,
            has_quality,
        })
    }

    /// Process `input` into `output` (same length, at most `max_block`)
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let frames = input.len().min(output.len()).min(self.max_block);
        unsafe { pb_nam_process(self.handle, input.as_ptr(), output.as_mut_ptr(), frames) }
    }

    /// True if the model holds several sizes to switch between
    pub fn has_quality(&self) -> bool {
        self.has_quality
    }

    /// Use the full model (1) or a lighter one (down to 0). Real-time safe;
    /// does nothing on a model without sizes.
    pub fn set_quality(&mut self, quality: f32) {
        if self.has_quality {
            unsafe { pb_nam_set_quality(self.handle, quality.clamp(0.0, 1.0)) }
        }
    }

    /// Sample rate the model was trained at, if known
    pub fn sample_rate(&self) -> Option<f32> {
        let rate = unsafe { pb_nam_sample_rate(self.handle) };
        (rate > 0.0).then_some(rate)
    }

    /// Input gain (dB) bringing the signal to the level the model expects
    pub fn input_adjustment_db(&self) -> f32 {
        unsafe { pb_nam_input_adjustment(self.handle) }
    }

    /// Output gain (dB) bringing the model to a standard loudness
    pub fn output_adjustment_db(&self) -> f32 {
        unsafe { pb_nam_output_adjustment(self.handle) }
    }
}

/// True if the model file holds several sizes of its capture (a packed A2
/// file from TONE3000), so its block can offer a lighter one. Reads the
/// whole file: not for the audio thread.
pub fn file_has_quality(path: &Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Header {
        architecture: String,
    }
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<Header>(&data).ok())
        .is_some_and(|h| h.architecture == "SlimmableContainer")
}

impl Drop for NamModel {
    fn drop(&mut self) {
        unsafe { pb_nam_free(self.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn example(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("third_party/NeuralAudio/deps/NeuralAmpModelerCore/example_models")
            .join(name)
    }

    fn sine(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| 0.3 * (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin())
            .collect()
    }

    fn check_model(name: &str) {
        let mut model = NamModel::load(&example(name), 48000.0, 128).unwrap();
        let input = sine(128);
        let mut output = vec![0.0; 128];
        // Let the receptive field fill up
        for _ in 0..20 {
            model.process(&input, &mut output);
        }
        assert!(output.iter().all(|s| s.is_finite()));
        assert!(
            output.iter().any(|s| s.abs() > 1e-4),
            "{name} output is silent"
        );
    }

    #[test]
    fn processes_wavenet_model() {
        check_model("wavenet.nam");
    }

    #[test]
    fn processes_lstm_model() {
        check_model("lstm.nam");
    }

    #[test]
    fn switches_the_size_of_a_packed_model() {
        let path = example("slimmable_container.nam");
        assert!(file_has_quality(&path));
        assert!(!file_has_quality(&example("wavenet.nam")));
        assert!(!file_has_quality(Path::new("/nonexistent/amp.nam")));

        let mut model = NamModel::load(&path, 48000.0, 128).unwrap();
        assert!(model.has_quality());
        let input = sine(128);
        let mut output = vec![0.0; 128];
        for quality in [QUALITY_LIGHT, QUALITY_FULL, QUALITY_LIGHT] {
            model.set_quality(quality);
            for _ in 0..20 {
                model.process(&input, &mut output);
            }
            assert!(output.iter().all(|s| s.is_finite()));
            assert!(output.iter().any(|s| s.abs() > 1e-4));
        }

        // Without sizes, the quality is ignored
        let mut plain = NamModel::load(&example("wavenet.nam"), 48000.0, 128).unwrap();
        assert!(!plain.has_quality());
        plain.set_quality(QUALITY_LIGHT);
    }

    #[test]
    fn reports_load_errors() {
        let missing = NamModel::load(Path::new("/nonexistent/amp.nam"), 48000.0, 128);
        assert!(missing.is_err());

        let dir = std::env::temp_dir().join("pedalboard-nam-test");
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("broken.nam");
        std::fs::write(&bad, "{ not json").unwrap();
        let error = NamModel::load(&bad, 48000.0, 128).err().unwrap();
        assert!(error.to_string().contains("broken.nam"));
    }
}
