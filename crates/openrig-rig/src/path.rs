//! File paths carried by a rig. They come from other people's files: a
//! reader joins them to its models directory only once this check passes.

use crate::limits;

/// Whether `path` is a plain relative path that stays inside the folder it
/// is joined to, on every OS:
///
/// - not empty, at most [`limits::PATH`] bytes, `/`-separated;
/// - no leading `/`, no `\`, no drive (`C:`), no `:` at all (alternate data
///   streams on Windows);
/// - no empty, `.` or `..` component, no component ending in a dot or a
///   space (Windows drops them), no control character;
/// - no component named like a Windows device (`CON`, `NUL`, `COM1`…).
///
/// ```
/// use openrig_rig::is_safe_relative_path;
/// assert!(is_safe_relative_path("Tone3000/Amps/Plexi [12]/Crunch [34].nam"));
/// assert!(!is_safe_relative_path("../etc/passwd"));
/// assert!(!is_safe_relative_path("/etc/passwd"));
/// assert!(!is_safe_relative_path("C:/Windows"));
/// ```
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > limits::PATH {
        return false;
    }
    if path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return false;
    }
    path.split('/').all(safe_component)
}

fn safe_component(part: &str) -> bool {
    if part.is_empty() || part == "." || part == ".." {
        return false;
    }
    if part.ends_with('.') || part.ends_with(' ') || part.starts_with(' ') {
        return false;
    }
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes().get(3).is_some_and(u8::is_ascii_digit));
    !device
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_model_paths() {
        for path in [
            "a.nam",
            "Tone3000/Amps/Plexi [12]/Crunch [34].nam",
            "My IRs/V30 – 4x12.wav",
            "Ünïcode/model.json",
            ".hidden/x.wav",
        ] {
            assert!(is_safe_relative_path(path), "{path}");
        }
    }

    #[test]
    fn refuses_escapes() {
        let long = "a/".repeat(300) + "b";
        for path in [
            "",
            "/abs.nam",
            "../up.nam",
            "a/../../b",
            "a/./b",
            "a//b",
            "a/",
            "C:/x.nam",
            "c:x.nam",
            "a\\b.nam",
            "file.nam:stream",
            "a/\u{0}b",
            "a/line\nbreak",
            "NUL",
            "dir/com1.wav",
            "Lpt9",
            "trailing./x",
            "space /x",
            long.as_str(),
        ] {
            assert!(!is_safe_relative_path(path), "{path:?}");
        }
        assert!(is_safe_relative_path("COM10/x"));
        assert!(is_safe_relative_path("console/x"));
    }
}
