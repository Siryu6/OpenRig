//! The order of a plugin's parameters in the editor and the rack's knob
//! strip, and those left out of the strip, as arranged in the editor. Kept
//! per plugin URI (every instance of the plugin), by port symbol, so a new
//! version of the plugin keeps it; parameters it does not name come after,
//! in the plugin's order.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "param-layout.json";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Symbols, first shown first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// Symbols left out of the knob strip
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
}

impl Layout {
    /// Sort key of a parameter: its place if it is named, else after
    /// them (a stable sort keeps the plugin's order for those)
    pub fn rank(&self, symbol: &str) -> usize {
        self.order
            .iter()
            .position(|s| s == symbol)
            .unwrap_or(usize::MAX)
    }

    pub fn in_strip(&self, symbol: &str) -> bool {
        !self.hidden.iter().any(|s| s == symbol)
    }

    /// The layout with `symbol` moved `steps` places in `shown` (the
    /// symbols in the order shown)
    pub fn moved<S: AsRef<str>>(&self, shown: &[S], symbol: &str, steps: i32) -> Self {
        let mut order: Vec<String> = shown.iter().map(|s| s.as_ref().to_string()).collect();
        if let Some(from) = order.iter().position(|s| s == symbol) {
            let to = (from as i64 + i64::from(steps)).clamp(0, order.len() as i64 - 1) as usize;
            let symbol = order.remove(from);
            order.insert(to, symbol);
        }
        Self {
            order,
            hidden: self.hidden.clone(),
        }
    }

    /// The layout with `symbol` put in or left out of the strip
    pub fn toggled(&self, symbol: &str) -> Self {
        let mut hidden = self.hidden.clone();
        match hidden.iter().position(|s| s == symbol) {
            Some(i) => {
                hidden.remove(i);
            }
            None => hidden.push(symbol.to_string()),
        }
        Self {
            order: self.order.clone(),
            hidden,
        }
    }
}

/// Every plugin's layout, by URI
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Layouts(BTreeMap<String, Layout>);

impl Layouts {
    /// The saved layouts; none if the file is missing or unreadable
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        crate::persist::load_json(&path)
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "{}: {e:#}; parameters in the plugins' order",
                    path.display()
                );
                None
            })
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        crate::persist::save_json(&data_dir.join(FILE), self)
    }

    pub fn get(&self, uri: &str) -> Layout {
        self.0.get(uri).cloned().unwrap_or_default()
    }

    /// Sets the layout of `uri`; the default one is not kept
    pub fn set(&mut self, uri: &str, layout: Layout) {
        if layout == Layout::default() {
            self.0.remove(uri);
        } else {
            self.0.insert(uri.to_string(), layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYMBOLS: [&str; 4] = ["gain", "bass", "mid", "treble"];

    #[test]
    fn named_parameters_come_first_then_the_plugins_order() {
        let arranged = |layout: &Layout| {
            let mut symbols = SYMBOLS.to_vec();
            symbols.sort_by_key(|s| layout.rank(s));
            symbols
        };
        assert_eq!(arranged(&Layout::default()), SYMBOLS);
        let layout = Layout {
            // "level" is gone from the plugin
            order: vec!["treble".into(), "level".into(), "gain".into()],
            ..Layout::default()
        };
        assert_eq!(arranged(&layout), ["treble", "gain", "bass", "mid"]);
    }

    #[test]
    fn moving_keeps_the_shown_order_and_stops_at_the_ends() {
        let layout = Layout::default().moved(&SYMBOLS, "mid", -1);
        assert_eq!(layout.order, ["gain", "mid", "bass", "treble"]);
        let layout = layout.moved(&["gain", "mid", "bass", "treble"], "gain", -3);
        assert_eq!(layout.order, ["gain", "mid", "bass", "treble"]);
        let layout = layout.moved(&["gain", "mid", "bass", "treble"], "gain", 9);
        assert_eq!(layout.order, ["mid", "bass", "treble", "gain"]);
    }

    #[test]
    fn hiding_from_the_strip_and_back() {
        let layout = Layout::default().toggled("bass");
        assert!(!layout.in_strip("bass") && layout.in_strip("gain"));
        assert_eq!(layout.toggled("bass"), Layout::default());
    }

    #[test]
    fn layouts_are_saved_and_the_default_is_dropped() {
        let dir = std::env::temp_dir().join(format!("pedalboard-layout-{}", uuid::Uuid::new_v4()));
        let mut layouts = Layouts::default();
        layouts.set("urn:a", Layout::default().toggled("gain"));
        layouts.set("urn:b", Layout::default().toggled("gain"));
        layouts.set("urn:b", Layout::default());
        layouts.save(dir.as_path()).unwrap();
        let loaded = Layouts::load(dir.as_path());
        assert!(!loaded.get("urn:a").in_strip("gain"));
        assert_eq!(loaded.0.len(), 1);
        assert_eq!(Layouts::load(&dir.join("none")).0.len(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
