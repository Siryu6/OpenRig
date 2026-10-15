//! Test LV2 plugins compiled on the fly with the system C compiler

use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a bundle `<name>.lv2` from C source and Turtle files; returns the
/// directory holding it, or None if it cannot be compiled here (no C
/// compiler or LV2 headers)
pub fn build_bundle(name: &str, source: &str, manifest: &str, ttl: &str) -> Option<PathBuf> {
    let root = std::env::temp_dir().join(format!("pedalboard-lv2-{}", uuid::Uuid::new_v4()));
    let bundle = root.join(format!("{name}.lv2"));
    std::fs::create_dir_all(&bundle).ok()?;
    std::fs::write(bundle.join(format!("{name}.c")), source).ok()?;
    std::fs::write(bundle.join("manifest.ttl"), manifest).ok()?;
    std::fs::write(bundle.join(format!("{name}.ttl")), ttl).ok()?;

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(cc)
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(bundle.join(format!("{name}.so")))
        .arg(bundle.join(format!("{name}.c")))
        .status()
        .ok()?;
    status.success().then_some(root)
}

/// LV2 path with the test bundles first; the system path provides the LV2
/// specs (plugin classes, atom and time vocabularies…)
pub fn lv2_path(bundles: &Path) -> String {
    let mut path = bundles.display().to_string();
    for system in ["/usr/lib/lv2", "/usr/local/lib/lv2", "/usr/lib64/lv2"] {
        if Path::new(system).exists() {
            path.push(':');
            path.push_str(system);
        }
    }
    path
}
