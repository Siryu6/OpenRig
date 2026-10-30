//! Folder browsing in a list of files: each item has a location (the folder
//! holding it, relative to the models directory, `/`-separated, "" for the
//! top). A level shows the subfolders of a folder, then the items in it.
//! A level holding a single subfolder and nothing else is skipped.

/// What a folder shows
#[derive(Debug, Default, PartialEq)]
pub struct Level {
    /// Subfolders, sorted by name, with the number of items below each
    pub folders: Vec<(String, usize)>,
    /// Indexes of the items right in the folder, in their order
    pub items: Vec<usize>,
}

/// `folder/name`, or `name` at the top
pub fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

fn parent(folder: &str) -> &str {
    folder.rsplit_once('/').map_or("", |(p, _)| p)
}

/// The part of `location` below `folder`, if it is inside it
fn below<'a>(location: &'a str, folder: &str) -> Option<&'a str> {
    if folder.is_empty() {
        Some(location)
    } else {
        location
            .strip_prefix(folder)
            .and_then(|rest| rest.strip_prefix('/').or(rest.is_empty().then_some("")))
    }
}

pub fn level<S: AsRef<str>>(locations: &[S], folder: &str) -> Level {
    let mut level = Level::default();
    for (i, location) in locations.iter().enumerate() {
        match below(location.as_ref(), folder) {
            Some("") => level.items.push(i),
            Some(rest) => {
                let name = rest.split('/').next().unwrap_or(rest);
                match level.folders.iter_mut().find(|(n, _)| n == name) {
                    Some((_, count)) => *count += 1,
                    None => level.folders.push((name.to_string(), 1)),
                }
            }
            None => {}
        }
    }
    level.folders.sort_by_key(|(name, _)| name.to_lowercase());
    level
}

fn skipped<S: AsRef<str>>(locations: &[S], folder: &str) -> bool {
    let level = level(locations, folder);
    level.folders.len() == 1 && level.items.is_empty()
}

/// The level shown when opening `folder`: down while it would only show
/// one subfolder
pub fn enter<S: AsRef<str>>(locations: &[S], folder: &str) -> String {
    let mut folder = folder.to_string();
    loop {
        let level = level(locations, &folder);
        match level.folders.as_slice() {
            [(name, _)] if level.items.is_empty() => folder = join(&folder, name),
            _ => return folder,
        }
    }
}

/// The level above `folder`, skipping those that would only show one
/// subfolder
pub fn up<S: AsRef<str>>(locations: &[S], folder: &str) -> String {
    let mut folder = parent(folder);
    while !folder.is_empty() && skipped(locations, folder) {
        folder = parent(folder);
    }
    if folder.is_empty() {
        enter(locations, "")
    } else {
        folder.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCATIONS: [&str; 5] = [
        "Tone3000/Amps",
        "Tone3000/Amps",
        "Tone3000/Pedals",
        "Tone3000/Cabs/Mesa",
        "Tone3000",
    ];

    #[test]
    fn a_level_lists_subfolders_then_items() {
        let level = level(&LOCATIONS, "Tone3000");
        assert_eq!(
            level.folders,
            [("Amps".into(), 2), ("Cabs".into(), 1), ("Pedals".into(), 1)]
        );
        assert_eq!(level.items, [4]);
        assert_eq!(super::level(&LOCATIONS, "Tone3000/Amps").items, [0, 1]);
        // A folder name that only starts like another is not inside it
        assert!(super::level(&LOCATIONS, "Tone").folders.is_empty());
    }

    #[test]
    fn levels_with_one_subfolder_only_are_skipped() {
        // The top only holds Tone3000
        assert_eq!(enter(&LOCATIONS, ""), "Tone3000");
        assert_eq!(enter(&LOCATIONS, "Tone3000/Cabs"), "Tone3000/Cabs/Mesa");
        assert_eq!(up(&LOCATIONS, "Tone3000/Cabs/Mesa"), "Tone3000");
        assert_eq!(up(&LOCATIONS, "Tone3000/Amps"), "Tone3000");
        assert_eq!(up(&LOCATIONS, "Tone3000"), "Tone3000");
        // Flat list: nothing to browse
        assert_eq!(enter(&["", ""], ""), "");
    }
}
