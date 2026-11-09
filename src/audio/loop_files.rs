//! Saved loops: a WAV file each (32-bit float stereo, the layers summed),
//! with a small JSON file next to it for its name and its bar grid. They
//! live in `loops/` of the data directory, next to the presets. File names
//! come from the time of saving; the name shown is in the JSON file, so any
//! name can be given. A WAV file dropped in the folder without its JSON file
//! is listed too, under its file name, as a free-length loop.
//!
//! Reading and writing happen on a worker thread, never the audio one.

use super::looper::LoopAudio;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Longest name kept
const MAX_NAME: usize = 60;

/// A loop on disk, for the list
#[derive(Debug, Clone, PartialEq)]
pub struct SavedLoop {
    /// File name without its extension
    pub id: String,
    pub name: String,
    pub seconds: f32,
    /// Whole bars (0: free length), with the tempo and bar they were played at
    pub bars: usize,
    pub bpm: f32,
    pub beats_per_bar: u8,
    /// When it was saved (last modified), for sorting
    modified: std::time::SystemTime,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Meta {
    name: String,
    bpm: f32,
    beats_per_bar: u8,
    bars: usize,
}

/// Folder of the saved loops in the data directory
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("loops")
}

/// Name given when none is: the date and time
pub fn default_name(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("Loop %Y-%m-%d %H:%M").to_string()
}

/// Ids are file names made here: nothing that could leave the folder
fn checked(id: &str) -> Result<&str> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ')
    {
        bail!("Invalid loop name {id:?}");
    }
    Ok(id)
}

fn wav_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.wav"))
}

fn meta_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn read_meta(dir: &Path, id: &str) -> Option<Meta> {
    let text = fs::read_to_string(meta_path(dir, id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The saved loops, newest first
pub fn list(dir: &Path) -> Vec<SavedLoop> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut loops: Vec<SavedLoop> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "wav" {
                return None;
            }
            let id = checked(path.file_stem()?.to_str()?).ok()?.to_string();
            let reader = hound::WavReader::open(&path).ok()?;
            let spec = reader.spec();
            let seconds = reader.duration() as f32 / spec.sample_rate.max(1) as f32;
            let meta = read_meta(dir, &id);
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            Some(SavedLoop {
                name: meta.as_ref().map_or_else(|| id.clone(), |m| m.name.clone()),
                bars: meta.as_ref().map_or(0, |m| m.bars),
                bpm: meta.as_ref().map_or(crate::presets::DEFAULT_BPM, |m| m.bpm),
                beats_per_bar: meta.as_ref().map_or(4, |m| m.beats_per_bar),
                id,
                seconds,
                modified,
            })
        })
        .collect();
    loops.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
    loops
}

/// The WAV file's bytes: 32-bit float, stereo
fn encode(audio: &LoopAudio) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: audio.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut bytes = std::io::Cursor::new(Vec::with_capacity(4 * audio.frames.len() + 64));
    let mut writer = hound::WavWriter::new(&mut bytes, spec)?;
    for &sample in &audio.frames {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(bytes.into_inner())
}

/// Save `audio` as `name` (the date and time when empty)
pub fn save(
    dir: &Path,
    name: &str,
    audio: &LoopAudio,
    now: chrono::DateTime<chrono::Local>,
) -> Result<SavedLoop> {
    fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let name: String = name.trim().chars().take(MAX_NAME).collect();
    let name = if name.is_empty() {
        default_name(now)
    } else {
        name
    };
    let stem = now.format("loop-%Y%m%d-%H%M%S").to_string();
    let id = (1..)
        .map(|n| match n {
            1 => stem.clone(),
            n => format!("{stem}-{n}"),
        })
        .find(|id| !wav_path(dir, id).exists())
        .unwrap_or(stem);
    let meta = Meta {
        name: name.clone(),
        bpm: audio.bpm,
        beats_per_bar: audio.beats_per_bar,
        bars: audio.bars,
    };
    // The JSON file first: a WAV file alone would be listed as free length
    crate::persist::write_atomic(&meta_path(dir, &id), &serde_json::to_vec_pretty(&meta)?)?;
    crate::persist::write_atomic(&wav_path(dir, &id), &encode(audio)?)?;
    Ok(SavedLoop {
        id,
        name,
        seconds: (audio.frames.len() / 2) as f32 / audio.sample_rate.max(1) as f32,
        bars: audio.bars,
        bpm: audio.bpm,
        beats_per_bar: audio.beats_per_bar,
        modified: std::time::SystemTime::now(),
    })
}

/// Read the loop `id`: any PCM or float WAV, mono or stereo (the first two
/// channels of more)
pub fn load(dir: &Path, id: &str) -> Result<LoopAudio> {
    let path = wav_path(dir, checked(id)?);
    let reader =
        hound::WavReader::open(&path).with_context(|| format!("Cannot read {}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels.max(1) as usize;
    let frames = samples
        .chunks_exact(channels)
        .flat_map(|frame| [frame[0], frame[if channels > 1 { 1 } else { 0 }]])
        .collect();
    let meta = read_meta(dir, id);
    Ok(LoopAudio {
        frames,
        sample_rate: spec.sample_rate,
        bpm: meta.as_ref().map_or(crate::presets::DEFAULT_BPM, |m| m.bpm),
        beats_per_bar: meta.as_ref().map_or(4, |m| m.beats_per_bar),
        bars: meta.as_ref().map_or(0, |m| m.bars),
    })
}

/// Delete the loop `id` (its JSON file too)
pub fn delete(dir: &Path, id: &str) -> Result<()> {
    let path = wav_path(dir, checked(id)?);
    fs::remove_file(&path).with_context(|| format!("Cannot delete {}", path.display()))?;
    let _ = fs::remove_file(meta_path(dir, id));
    crate::persist::sync_dir(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 11, 9, h, m, s)
            .single()
            .unwrap()
    }

    /// A fresh folder, removed when dropped
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("openrig-loops-{}", uuid::Uuid::new_v4())))
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn audio() -> LoopAudio {
        LoopAudio {
            frames: (0..2000).map(|i| (i as f32 / 2000.0) - 0.5).collect(),
            sample_rate: 48000,
            bpm: 96.0,
            beats_per_bar: 3,
            bars: 2,
        }
    }

    #[test]
    fn a_loop_survives_a_round_trip() {
        let tmp = TempDir::new();
        let dir = dir(tmp.path());
        let saved = save(&dir, "  Groove in A  ", &audio(), at(18, 42, 5)).unwrap();
        assert_eq!(saved.id, "loop-20261109-184205");
        assert_eq!(saved.name, "Groove in A");
        let loaded = load(&dir, &saved.id).unwrap();
        assert_eq!(loaded, audio());
        let listed = list(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Groove in A");
        assert_eq!((listed[0].bars, listed[0].beats_per_bar), (2, 3));
        assert!((listed[0].seconds - 1000.0 / 48000.0).abs() < 1e-6);
    }

    #[test]
    fn names_default_to_the_date_and_files_never_clash() {
        let tmp = TempDir::new();
        let first = save(tmp.path(), "", &audio(), at(9, 5, 0)).unwrap();
        assert_eq!(first.name, "Loop 2026-11-09 09:05");
        let second = save(tmp.path(), "", &audio(), at(9, 5, 0)).unwrap();
        assert_eq!(second.id, "loop-20261109-090500-2");
        assert_eq!(list(tmp.path()).len(), 2);
        delete(tmp.path(), &first.id).unwrap();
        let left = list(tmp.path());
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, second.id);
        assert!(!tmp.path().join(format!("{}.json", first.id)).exists());
    }

    #[test]
    fn a_plain_wav_file_is_a_free_loop() {
        let tmp = TempDir::new();
        fs::create_dir_all(tmp.path()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(tmp.path().join("my take.wav"), spec).unwrap();
        for s in [0i16, 16384, -16384] {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
        let listed = list(tmp.path());
        assert_eq!(listed[0].name, "my take");
        assert_eq!(listed[0].bars, 0);
        let loaded = load(tmp.path(), "my take").unwrap();
        assert_eq!(loaded.frames, vec![0.0, 0.0, 0.5, 0.5, -0.5, -0.5]);
        assert_eq!(loaded.sample_rate, 44100);
    }

    #[test]
    fn ids_cannot_leave_the_folder() {
        let tmp = TempDir::new();
        assert!(load(tmp.path(), "../presets/x").is_err());
        assert!(delete(tmp.path(), "a/b").is_err());
        assert!(delete(tmp.path(), "").is_err());
    }
}
