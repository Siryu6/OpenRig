//! The user's TONE3000 library: tone lists, the models of a tone, and model
//! downloads into `models/Tone3000/`.
//!
//! Only what TONE3000 allows OpenRig is used: the bounded list endpoints
//! (favorited, created, downloaded, trending, latest), the search, sent
//! when the user submits it, and the downloads of the models the user
//! picked, one after the other at the pace of `limiter`. Nothing is
//! prefetched or cached; the creator, license and link of every downloaded
//! tone are kept in a `tone.json` next to its files.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::auth::{download_agent, get_json, AuthError};
use crate::plugins::scanner::{TONE_INFO_FILE, TONE_PICTURE_FILE};

/// Folder of the models dir holding the TONE3000 downloads
pub const FOLDER: &str = "Tone3000";
/// Items asked per page (the API maximum); also the models shown and ticked
/// at once in a tone, as TONE3000 asks
pub const PAGE_SIZE: u32 = 25;

/// A list of tones
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Favorites,
    Created,
    Downloaded,
    Trending,
    Latest,
}

impl List {
    pub const ALL: [List; 5] = [
        List::Favorites,
        List::Created,
        List::Downloaded,
        List::Trending,
        List::Latest,
    ];

    fn path(self) -> &'static str {
        match self {
            List::Favorites => "/tones/favorited",
            List::Created => "/tones/created",
            List::Downloaded => "/tones/downloaded",
            List::Trending => "/tones/trending",
            List::Latest => "/tones/latest",
        }
    }

    /// Trending and latest are a single short list; latest ignores the gear
    /// filter, so it is applied here
    fn paginated(self) -> bool {
        matches!(self, List::Favorites | List::Created | List::Downloaded)
    }
}

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

fn gear_folder(gear: &str) -> &str {
    GEARS
        .iter()
        .find(|(id, ..)| *id == gear)
        .map(|(.., folder)| *folder)
        .unwrap_or("Other")
}

/// File extension of a model format, `None` when no block can load it yet
pub fn file_extension(format: &str) -> Option<&'static str> {
    match format {
        "nam" if cfg!(feature = "nam") => Some("nam"),
        "ir" => Some("wav"),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Creator {
    pub username: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// A tone (a capture pack: one or more models of a piece of gear)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tone {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub gear: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub user: Option<Creator>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub models_count: u32,
    #[serde(default)]
    pub a1_models_count: u32,
    #[serde(default)]
    pub a2_models_count: u32,
    #[serde(default)]
    pub custom_models_count: u32,
    #[serde(default)]
    pub irs_count: u32,
    #[serde(default)]
    pub sizes: Option<Vec<String>>,
    /// Pictures of the gear, the first one is the tone's
    #[serde(default)]
    pub images: Option<Vec<String>>,
}

impl Tone {
    pub fn creator(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.username.as_str())
            .unwrap_or("unknown")
    }

    /// The tone's picture, if any
    pub fn image(&self) -> Option<&str> {
        self.images
            .as_ref()?
            .first()
            .map(String::as_str)
            .filter(|u| !u.is_empty())
    }

    /// The creator's avatar, if any
    pub fn avatar(&self) -> Option<&str> {
        self.user
            .as_ref()?
            .avatar_url
            .as_deref()
            .filter(|u| !u.is_empty())
    }

    pub fn supported(&self) -> bool {
        file_extension(&self.format).is_some()
    }

    /// Link to the tone page
    pub fn page_url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| format!("{}/tones/{}", super::WEBSITE, self.id))
    }

    /// Where this tone's files go, relative to the models dir
    pub fn folder(&self) -> PathBuf {
        Path::new(FOLDER)
            .join(gear_folder(&self.gear))
            .join(format!("{} [{}]", file_name(&self.title), self.id))
    }
}

/// One model file of a tone
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: u64,
    pub name: String,
    #[serde(default, skip_serializing)]
    pub model_url: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub architecture_version: Option<String>,
}

impl Model {
    /// File path relative to the models dir. The id keeps apart models of
    /// a tone with the same name (A1 and A2 captures, sizes)
    pub fn relative_path(&self, tone: &Tone) -> Option<PathBuf> {
        let extension = file_extension(&tone.format)?;
        Some(tone.folder().join(format!(
            "{} [{}].{}",
            file_name(&self.name),
            self.id,
            extension
        )))
    }

    /// "A2", "A1 · lite", "IR"…
    pub fn description(&self, tone: &Tone) -> String {
        let mut parts = Vec::new();
        if tone.format == "ir" {
            parts.push("IR".to_string());
        } else {
            match self.architecture_version.as_deref() {
                Some("2") => parts.push("A2".into()),
                Some("custom") => parts.push("Custom".into()),
                _ => parts.push("A1".into()),
            }
        }
        if let Some(size) = self.size.as_deref().filter(|s| !s.is_empty()) {
            parts.push(size.to_string());
        }
        parts.join(" · ")
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

#[derive(Deserialize)]
struct ListResponse<T> {
    data: Vec<T>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    total_pages: Option<u32>,
}

/// One page of a list
pub struct Page {
    pub tones: Vec<Tone>,
    pub page: u32,
    pub has_more: bool,
}

/// Fetch one page of `list`, filtered by gear (API value, `None` for all)
pub fn list(token: &str, list: List, gear: Option<&str>, page: u32) -> Result<Page, AuthError> {
    let mut path = format!("{}?page={page}", list.path());
    if list.paginated() {
        path += &format!("&page_size={PAGE_SIZE}");
    }
    if let Some(gear) = gear {
        path += &format!("&gear={gear}");
    }
    let response: ListResponse<Tone> = get_json(token, &path)?;
    let mut tones = response.data;
    if let Some(gear) = gear {
        tones.retain(|t| t.gear == gear);
    }
    let page = response.page.unwrap_or(page);
    let has_more = list.paginated() && page < response.total_pages.unwrap_or(0);
    Ok(Page {
        tones,
        page,
        has_more,
    })
}

/// One page of the tones matching `query`, filtered by gear (API value,
/// `None` for all)
pub fn search(token: &str, query: &str, gear: Option<&str>, page: u32) -> Result<Page, AuthError> {
    let mut path = format!(
        "/tones/search?query={}&page={page}&page_size={PAGE_SIZE}",
        encode(query.trim())
    );
    if let Some(gear) = gear {
        path += &format!("&gears={}", encode(gear));
    }
    let response: ListResponse<Tone> = get_json(token, &path)?;
    let mut tones = response.data;
    if let Some(gear) = gear {
        tones.retain(|t| t.gear == gear);
    }
    let page = response.page.unwrap_or(page);
    Ok(Page {
        tones,
        page,
        has_more: page < response.total_pages.unwrap_or(0),
    })
}

/// Percent-encode a query parameter
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Every model of a tone. The API leaves A2 models out unless asked, so
/// both sets are requested.
pub fn models(token: &str, tone: &Tone) -> Result<Vec<Model>, AuthError> {
    let mut queries = vec![""];
    if tone.format != "ir" && tone.a2_models_count > 0 {
        queries.push("&architecture=2");
        if tone.a1_models_count + tone.custom_models_count == 0 {
            queries.remove(0);
        }
    }
    let mut models = Vec::new();
    for query in queries {
        let mut page = 1;
        loop {
            let path = format!(
                "/models?tone_id={}&page={page}&page_size={PAGE_SIZE}{query}",
                tone.id
            );
            let response: ListResponse<Model> = get_json(token, &path)?;
            models.extend(response.data);
            if page >= response.total_pages.unwrap_or(0) {
                break;
            }
            page += 1;
        }
    }
    models.sort_by_key(|m| m.name.to_lowercase());
    models.dedup_by_key(|m| m.id);
    Ok(models)
}

/// What is kept next to the files of a downloaded tone
#[derive(Serialize, Deserialize)]
struct ToneInfo {
    id: u64,
    title: String,
    creator: String,
    license: Option<String>,
    url: String,
    gear: String,
    format: String,
    /// Picture shown in the block that loads it
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    source: String,
}

/// Largest model or IR file accepted (NAM models are a few MB at most)
const MAX_MODEL_BYTES: u64 = 64 << 20;

/// Download one model into `models_dir`; returns its path relative to it.
/// An existing file is kept. The caller waits for its turn in `limiter`
/// first.
pub fn download(token: &str, models_dir: &Path, tone: &Tone, model: &Model) -> Result<PathBuf> {
    let relative = model
        .relative_path(tone)
        .ok_or_else(|| anyhow!("The {} format is not supported yet", tone.format))?;
    let path = models_dir.join(&relative);
    if path.is_file() {
        return Ok(relative);
    }
    // The token goes with the request: only ever send it to TONE3000
    if !model.model_url.starts_with(&format!("{}/", super::WEBSITE)) {
        bail!("Unexpected download address for {}", model.name);
    }
    let dir = path.parent().unwrap_or(models_dir);
    std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;

    let response = download_agent()
        .get(&model.model_url)
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .map_err(|e| anyhow!("Cannot reach TONE3000: {e}"))?;
    match response.status().as_u16() {
        200..=299 => {}
        401 => bail!(AuthError::Revoked),
        status => bail!("Download of {} failed (HTTP {status})", model.name),
    }
    let part = path.with_extension("part");
    let mut file =
        std::fs::File::create(&part).with_context(|| format!("Cannot write {}", part.display()))?;
    // Bounded: /data also holds the system's state (network, clock)
    let mut body = response.into_body().into_reader().take(MAX_MODEL_BYTES + 1);
    let written = std::io::copy(&mut body, &mut file);
    let result = written
        .map_err(anyhow::Error::from)
        .and_then(|n| {
            if n == 0 {
                bail!("{} is empty", model.name);
            }
            if n > MAX_MODEL_BYTES {
                bail!("{} is too large", model.name);
            }
            // On the card before it gets its name: a power cut must not
            // leave a truncated model that looks downloaded
            file.sync_all()?;
            Ok(())
        })
        .and_then(|()| std::fs::rename(&part, &path).map_err(Into::into));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e.context(format!("Download of {} failed", model.name)));
    }

    // Written once a model is there: it marks the tone as downloaded
    let info = ToneInfo {
        id: tone.id,
        title: tone.title.clone(),
        creator: tone.creator().to_string(),
        license: tone.license.clone(),
        url: tone.page_url(),
        gear: tone.gear.clone(),
        format: tone.format.clone(),
        image: tone.image().map(str::to_string),
        source: "Downloaded from TONE3000 (https://www.tone3000.com)".into(),
    };
    crate::presets::store::write_atomic(
        &dir.join(TONE_INFO_FILE),
        serde_json::to_string_pretty(&info)?.as_bytes(),
    )?;
    save_picture(models_dir, tone);
    Ok(relative)
}

/// Side of the picture kept with a downloaded tone
const PICTURE_SIDE: u32 = 96;

/// Keep the tone's picture with its models, for its block on stage (no
/// network there); returns whether it was saved now. Optional: the models
/// work without it.
pub fn save_picture(models_dir: &Path, tone: &Tone) -> bool {
    let dir = models_dir.join(tone.folder());
    let path = dir.join(TONE_PICTURE_FILE);
    let Some(url) = tone.image() else {
        return false;
    };
    if path.is_file() || !dir.join(TONE_INFO_FILE).is_file() {
        return false;
    }
    let saved = super::images::fetch(url, PICTURE_SIDE)
        .and_then(|picture| super::images::encode_png(&picture))
        .and_then(|data| crate::presets::store::write_atomic(&path, &data));
    match saved {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("Picture of {} not saved: {:#}", tone.title, e);
            false
        }
    }
}

/// Whether some model of this tone is on the pedalboard
pub fn is_downloaded(models_dir: &Path, tone: &Tone) -> bool {
    models_dir
        .join(tone.folder())
        .join(TONE_INFO_FILE)
        .is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(format: &str) -> Tone {
        Tone {
            id: 42,
            title: "Fender: Twin/Reverb?".into(),
            gear: "amp".into(),
            format: format.into(),
            ..Default::default()
        }
    }

    #[test]
    fn builds_portable_paths() {
        let model = Model {
            id: 1,
            name: "Clean <bright>".into(),
            model_url: String::new(),
            size: Some("lite".into()),
            architecture_version: Some("2".into()),
        };
        let path = model.relative_path(&tone("ir")).unwrap();
        assert_eq!(
            path,
            Path::new("Tone3000/Amps/Fender- Twin-Reverb- [42]/Clean -bright- [1].wav")
        );
        assert_eq!(model.description(&tone("nam")), "A2 · lite");
        assert!(model.relative_path(&tone("proteus")).is_none());
        assert_eq!(file_name(" ..  "), "untitled");
    }

    #[test]
    fn parses_api_lists() {
        let page = r#"{"data":[{"id":7,"title":"Cab","gear":"cab","format":"ir",
            "user":{"username":"bob","avatar_url":"https://api.tone3000.com/a.jpg"},
            "license":"t3k","irs_count":4,"sizes":null,"images":["https://api.tone3000.com/i.jpg"],
            "makes":[{"id":1,"name":"Orange"}]}],"page":1,"page_size":25,"total":1,"total_pages":1}"#;
        let response: ListResponse<Tone> = serde_json::from_str(page).unwrap();
        let tone = &response.data[0];
        assert_eq!((tone.creator(), tone.gear.as_str()), ("bob", "cab"));
        assert!(tone.supported());
        assert_eq!(tone.image(), Some("https://api.tone3000.com/i.jpg"));
        assert_eq!(tone.avatar(), Some("https://api.tone3000.com/a.jpg"));
        // Trending / latest answer without pagination fields
        let short: ListResponse<Tone> = serde_json::from_str(r#"{"data":[]}"#).unwrap();
        assert_eq!(short.total_pages, None);
    }

    #[test]
    fn encodes_search_queries() {
        assert_eq!(encode("5150 red"), "5150%20red");
        assert_eq!(encode("AC/30 & co"), "AC%2F30%20%26%20co");
        assert_eq!(encode("Écho"), "%C3%89cho");
    }

    #[test]
    fn spots_downloaded_tones() {
        let dir = std::env::temp_dir().join(format!("pedalboard-t3k-{}", std::process::id()));
        let tone = tone("ir");
        assert!(!is_downloaded(&dir, &tone));
        std::fs::create_dir_all(dir.join(tone.folder())).unwrap();
        std::fs::write(dir.join(tone.folder()).join("tone.json"), "{}").unwrap();
        assert!(is_downloaded(&dir, &tone));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
