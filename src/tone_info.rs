//! Downloaded TONE3000 tones on disk: where their files go in the models
//! directory, and the `tone.json` kept next to them (creator, license,
//! link, TONE3000 ids). Available without the `tone3000` feature: rig
//! export reads it, rig import computes the same paths.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::plugins::scanner::TONE_INFO_FILE;

/// Folder of the models dir holding the TONE3000 downloads
pub const FOLDER: &str = "Tone3000";

/// Gear types, in display order: (API value, label, folder)
pub const GEARS: [(&str, &str, &str); 7] = [
    ("amp", "Amp", "Amps"),
    ("amp-cab", "Amp + cab", "Amps + cabs"),
    ("pedal", "Pedal", "Pedals"),
    ("outboard", "Outboard", "Outboard"),
    ("cab", "Cab", "Cabs"),
    ("space", "Space", "Spaces"),
    ("experimental", "Experimental", "Experimental"),
];

pub fn gear_label(gear: &str) -> &str {
    GEARS
        .iter()
        .find(|(id, ..)| *id == gear)
        .map(|(_, label, _)| *label)
        .unwrap_or(gear)
}

pub fn gear_folder(gear: &str) -> &str {
    GEARS
        .iter()
        .find(|(id, ..)| *id == gear)
        .map(|(.., folder)| *folder)
        .unwrap_or("Other")
}

/// File extension of a TONE3000 format, whether or not a block of this
/// build can load it
pub fn format_extension(format: &str) -> Option<&'static str> {
    match format {
        "nam" => Some("nam"),
        "ir" => Some("wav"),
        _ => None,
    }
}

/// Keep a name usable as a file name on every OS
pub fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let cleaned: String = cleaned.chars().take(80).collect();
    if cleaned.is_empty() {
        "untitled".into()
    } else {
        cleaned
    }
}

/// Folder of a tone, relative to the models dir
pub fn tone_folder(gear: &str, title: &str, tone_id: u64) -> PathBuf {
    Path::new(FOLDER)
        .join(gear_folder(gear))
        .join(format!("{} [{}]", file_name(title), tone_id))
}

/// File name of a model in its tone folder. The id keeps apart models of
/// a tone with the same name (A1 and A2 captures, sizes)
pub fn model_file(name: &str, model_id: u64, extension: &str) -> String {
    format!("{} [{}].{}", file_name(name), model_id, extension)
}

/// The id at the end of a file name made by `model_file`:
/// "Crunch [123].nam" -> 123
pub fn model_id_in(file: &str) -> Option<u64> {
    let stem = Path::new(file).file_stem()?.to_str()?;
    let (_, id) = stem.rsplit_once(" [")?;
    id.strip_suffix(']')?.parse().ok()
}

/// What is kept next to the files of a downloaded tone
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToneInfo {
    /// TONE3000 tone id
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub gear: String,
    #[serde(default)]
    pub format: String,
    /// Picture shown in the block that loads it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub source: String,
    /// The models downloaded into the folder, with their TONE3000 ids.
    /// Older files lack it: the model id is then read from the file name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelInfo>,
}

/// A downloaded model of a tone
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// TONE3000 model id
    pub id: u64,
    /// Name of the model on TONE3000
    pub name: String,
    /// File name in the tone folder
    pub file: String,
}

impl ToneInfo {
    /// The `tone.json` of folder `dir`, if there is a readable one
    pub fn read(dir: &Path) -> Option<Self> {
        let path = dir.join(TONE_INFO_FILE);
        let data = std::fs::read(&path).ok()?;
        match serde_json::from_slice(&data) {
            Ok(info) => Some(info),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Unreadable tone.json");
                None
            }
        }
    }

    /// Write it into `dir`
    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        crate::persist::write_atomic(
            &dir.join(TONE_INFO_FILE),
            serde_json::to_string_pretty(self)?.as_bytes(),
        )
    }

    /// Note a downloaded model (once per id)
    pub fn add_model(&mut self, model: ModelInfo) {
        self.models.retain(|m| m.id != model.id);
        self.models.push(model);
        self.models.sort_by_key(|m| m.id);
    }

    /// Id and name of the model in file `file` of the folder: from the
    /// list, else from the file name (`Name [id].ext`)
    pub fn model_of(&self, file: &str) -> Option<(u64, String)> {
        if let Some(model) = self.models.iter().find(|m| m.file == file) {
            return Some((model.id, model.name.clone()));
        }
        let id = model_id_in(file)?;
        let stem = Path::new(file).file_stem()?.to_str()?;
        let name = stem.rsplit_once(" [").map_or(stem, |(name, _)| name);
        Some((id, name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_from_file_names() {
        assert_eq!(model_id_in("Crunch [123].nam"), Some(123));
        assert_eq!(model_id_in("A [b] [7].wav"), Some(7));
        assert_eq!(model_id_in("Crunch.nam"), None);
        assert_eq!(model_id_in("Crunch [x].nam"), None);
    }

    #[test]
    fn old_tone_files_still_read() {
        let old = r#"{"id": 5, "title": "Plexi", "creator": "me", "license": null,
            "url": "https://www.tone3000.com/tones/5", "gear": "amp", "format": "nam",
            "source": "Downloaded from TONE3000"}"#;
        let info: ToneInfo = serde_json::from_str(old).unwrap();
        assert!(info.models.is_empty());
        assert_eq!(
            info.model_of("Clean [42].nam"),
            Some((42, "Clean".to_string()))
        );

        let mut info = info;
        info.add_model(ModelInfo {
            id: 9,
            name: "Edge: of breakup".into(),
            file: "Edge- of breakup [9].nam".into(),
        });
        assert_eq!(
            info.model_of("Edge- of breakup [9].nam"),
            Some((9, "Edge: of breakup".to_string()))
        );
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"models\""));
    }

    #[test]
    fn paths() {
        assert_eq!(
            tone_folder("amp", "Plexi/1959", 4),
            Path::new("Tone3000/Amps/Plexi-1959 [4]")
        );
        assert_eq!(model_file("Crunch?", 8, "nam"), "Crunch- [8].nam");
    }
}
