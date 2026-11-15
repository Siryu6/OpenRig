//! Small-signal frequency response of a plugin at the values of its
//! controls, measured on an instance of its own: a quiet click goes through
//! it after a short silence (for its smoothed controls to settle), and the
//! spectrum of what comes out is its response. A drive shows its tone this
//! way (the hump of its mids, the lows its tight control takes off), below
//! the level where it clips.
//!
//! Instances are made and freed on the chain builder thread only (see
//! `chain_switcher`): LV2 forbids making or freeing two instances of a
//! plugin at once.

use anyhow::{bail, Result};
use realfft::RealFftPlanner;

use super::{scanner, Lv2Host};

/// Samples of the response taken (170 ms at 48 kHz, 5.9 Hz per bin)
const LENGTH: usize = 8192;
/// Samples run per call
const BLOCK: usize = 256;
/// Silence before the click, seconds
const SETTLE: f32 = 0.25;
/// Height of the click (-40 dBFS): quiet enough for a drive not to clip
const CLICK: f32 = 0.01;

/// A response to measure, and who to give it to
pub struct Probe {
    pub uri: String,
    /// Value of each parameter, by index (as `PluginInstance::set_parameter`)
    pub values: Vec<(usize, f32)>,
    /// Frequencies to read it at, rising
    pub freqs: Vec<f32>,
    /// Called on the builder thread with the gain at each of `freqs`, dB
    pub done: Box<dyn FnOnce(Option<Vec<f32>>) + Send>,
}

/// Whether the response of plugin `uri` can be measured: an LV2 plugin,
/// not a built-in block
pub fn measurable(uri: &str) -> bool {
    scanner::file_block(uri).is_none() && !scanner::is_routing(uri)
}

/// The gain of plugin `uri` with its parameters at `values`, in dB at each
/// of `freqs`
pub fn measure(
    host: &Lv2Host,
    uri: &str,
    values: &[(usize, f32)],
    freqs: &[f32],
    sample_rate: f32,
) -> Result<Vec<f32>> {
    if !measurable(uri) || host.is_demo() {
        bail!("{uri}: no response to measure");
    }
    let mut instance = host.instantiate(uri, usize::MAX, sample_rate, BLOCK, None)?;
    for &(index, value) in values {
        instance.set_parameter(index, value);
    }
    let mut block = [0.0f32; BLOCK];
    for _ in 0..((SETTLE * sample_rate) as usize).div_ceil(BLOCK) {
        block.fill(0.0);
        instance.process_in_place(&mut block);
    }
    let mut response = vec![0.0f32; LENGTH];
    for (i, chunk) in response.chunks_mut(BLOCK).enumerate() {
        block.fill(0.0);
        if i == 0 {
            block[0] = CLICK;
        }
        instance.process_in_place(&mut block);
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
    if response.iter().any(|s| !s.is_finite()) {
        bail!("{uri}: no response (not a number)");
    }
    Ok(gains(&mut response, sample_rate, freqs))
}

/// Gain in dB at each of `freqs` of the response to a click of height
/// [`CLICK`]: the mean power of the bins between the points around each
fn gains(response: &mut [f32], rate: f32, freqs: &[f32]) -> Vec<f32> {
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(response.len());
    let mut bins = fft.make_output_vec();
    if fft.process(response, &mut bins).is_err() {
        return vec![f32::NEG_INFINITY; freqs.len()];
    }
    let power: Vec<f32> = bins
        .iter()
        .map(|b| b.norm_sqr() / (CLICK * CLICK))
        .collect();
    let hz_per_bin = rate / response.len() as f32;
    let last = power.len().saturating_sub(1);
    let bin = |f: f32| (f / hz_per_bin).clamp(0.0, last as f32);
    freqs
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            let around = |j: Option<usize>| j.and_then(|j| freqs.get(j)).copied();
            let lo = around(i.checked_sub(1)).map_or(f, |p| (p * f).sqrt());
            let hi = around(Some(i + 1)).map_or(f, |n| (n * f).sqrt());
            let (a, b) = (bin(lo).ceil() as usize, bin(hi).floor() as usize);
            let mean = match power.get(a..=b) {
                Some(range) if !range.is_empty() => range.iter().sum::<f32>() / range.len() as f32,
                _ => {
                    let x = bin(f);
                    let (k, t) = (x.floor() as usize, x.fract());
                    let at = |k: usize| power.get(k.min(last)).copied().unwrap_or(0.0);
                    at(k) * (1.0 - t) + at(k + 1) * t
                }
            };
            10.0 * mean.max(1e-20).log10()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_through_a_wire_is_flat() {
        let mut response = vec![0.0; LENGTH];
        response[0] = CLICK;
        let gains = gains(&mut response, 48_000.0, &[50.0, 1000.0, 10_000.0]);
        assert!(gains.iter().all(|g| g.abs() < 0.01), "{gains:?}");
    }

    #[test]
    fn a_lowpass_falls_off() {
        // One pole at about 1 kHz: -3 dB there, about -20 dB a decade above
        let a = (-2.0 * std::f32::consts::PI * 1000.0 / 48_000.0).exp();
        let mut response: Vec<f32> = (0..LENGTH)
            .map(|n| CLICK * (1.0 - a) * a.powi(n as i32))
            .collect();
        // Points close together, as on a plot: each reads a narrow band
        let freqs = [
            50.0, 55.0, 950.0, 1000.0, 1050.0, 9500.0, 10_000.0, 10_500.0,
        ];
        let gains = gains(&mut response, 48_000.0, &freqs);
        assert!(gains[0].abs() < 0.5, "{gains:?}");
        assert!((gains[3] + 3.0).abs() < 0.5, "{gains:?}");
        assert!(gains[6] < -18.0, "{gains:?}");
    }

    #[test]
    fn built_in_blocks_are_not_measured() {
        assert!(!measurable(scanner::FILE_BLOCKS[0].uri));
        assert!(measurable("http://example.org/drive"));
    }
}
