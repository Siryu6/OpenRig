//! USB drives: noticed when they come and go (`watch`), and ejected before
//! they are pulled out (`eject`). They are listed by
//! `rigs::removable_drives`. On the pedalboard OS, udev mounts them in
//! `/media` and the root helper `/usr/libexec/pedalboard/drives` ejects
//! them (a FIFO `request` and a file `status` in `PEDALBOARD_DRIVES_DIR`,
//! the protocol of the settings helper); elsewhere the desktop does both.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::system_config::{request_state, send_request};

/// Directory of the root helper, set by the launcher of the pedalboard OS
pub const DIR_ENV: &str = "PEDALBOARD_DRIVES_DIR";

/// How often `watch` looks at the drives
pub const WATCH_EVERY: Duration = Duration::from_secs(2);

/// How long an eject may take (writing what is cached to a slow stick)
const EJECT_TIMEOUT: Duration = Duration::from_secs(60);

fn helper_dir() -> Option<PathBuf> {
    std::env::var_os(DIR_ENV)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// Drives can be ejected from the app (the pedalboard OS)
pub fn can_eject() -> bool {
    helper_dir().is_some()
}

/// The drive a page shows (`shown`, a path from `rigs::removable_drives`),
/// if it is still plugged in
pub fn chosen(drives: &[PathBuf], shown: &str) -> Option<PathBuf> {
    if shown.is_empty() {
        return None;
    }
    drives.iter().find(|d| d.as_os_str() == shown).cloned()
}

/// `drive` was ejected or pulled out: not in `drives` (the drives now)
pub fn gone(drive: &Path, drives: &[PathBuf]) -> bool {
    !drives.iter().any(|d| d == drive)
}

/// The name of a drive, as the pages show it (its label)
pub fn name(drive: &Path) -> String {
    drive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| drive.display().to_string())
}

/// Eject `drive` (one of `rigs::removable_drives`): everything is written,
/// its file systems unmounted and it is powered off. Waits for the helper:
/// not on the UI thread.
pub fn eject(drive: &Path) -> Result<()> {
    let Some(dir) = helper_dir() else {
        bail!("Eject the drive from the system");
    };
    eject_with(&dir, drive, EJECT_TIMEOUT)
}

fn eject_with(dir: &Path, drive: &Path, timeout: Duration) -> Result<()> {
    let name = drive
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid drive")?;
    let id = send_request(dir, "drive helper", &format!("eject {name}"))?;
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(dir.join("status")) {
            match request_state(&text, &id) {
                Some(("done", _)) => return Ok(()),
                Some(("error", "")) => bail!("The drive could not be ejected"),
                Some(("error", message)) => bail!("{message}"),
                _ => {}
            }
        }
        if start.elapsed() > timeout {
            bail!("The drive helper did not answer");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Call `changed` with the drives each time they change (one plugged in,
/// ejected or pulled out), from a thread of its own that looks every
/// `WATCH_EVERY`: a few directory reads, and a stat that cannot hang the UI
/// on a drive gone bad. Runs as long as the app.
pub fn watch(changed: impl Fn(Vec<PathBuf>) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("drives-watch".into())
        .spawn(move || {
            let mut last = crate::rigs::removable_drives();
            loop {
                std::thread::sleep(WATCH_EVERY);
                let now = crate::rigs::removable_drives();
                if now != last {
                    tracing::info!(drives = ?now, "USB drives changed");
                    last = now.clone();
                    changed(now);
                }
            }
        })
        .map(|_| ())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[allow(clippy::unwrap_used)]
    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pb-drives-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn chosen_is_the_drive_shown_if_still_there() {
        let drives = vec![PathBuf::from("/media/A"), PathBuf::from("/media/B")];
        assert_eq!(chosen(&drives, "/media/B"), Some(PathBuf::from("/media/B")));
        assert_eq!(chosen(&drives, "/media/C"), None);
        assert_eq!(chosen(&drives, ""), None);
        assert_eq!(chosen(&[], "/media/A"), None);
        assert_eq!(name(Path::new("/media/MY STICK")), "MY STICK");
        assert!(!gone(Path::new("/media/A"), &drives));
        assert!(gone(Path::new("/media/C"), &drives));
        assert!(gone(Path::new("/media/A"), &[]));
    }

    #[test]
    fn eject_without_helper_fails_fast() {
        let dir = dir("none");
        let started = Instant::now();
        assert!(eject_with(&dir, Path::new("/media/STICK"), Duration::from_secs(5)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A helper that answers the first request with `answer`. The FIFO is
    /// open (read and write, so it never blocks) before this returns: the
    /// request finds a reader at once, and the helper never waits forever.
    #[allow(clippy::unwrap_used)]
    fn helper(dir: &Path, answer: &'static str) -> std::thread::JoinHandle<String> {
        use std::io::BufRead;
        let fifo = dir.join("request");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .unwrap();
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(file).read_line(&mut line).unwrap();
            let id = line.split(' ').next().unwrap().to_string();
            std::fs::write(dir.join("status"), format!("{id} {answer}\n")).unwrap();
            line
        })
    }

    #[test]
    fn eject_asks_the_helper() {
        let dir = dir("done");
        let helper = helper(&dir, "done");
        eject_with(&dir, Path::new("/media/MY_STICK"), Duration::from_secs(10)).unwrap();
        assert!(helper
            .join()
            .unwrap()
            .trim_end()
            .ends_with(" eject MY_STICK"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eject_reports_the_helper_error() {
        let dir = dir("error");
        let helper = helper(&dir, "error The drive is in use, try again");
        let result = eject_with(&dir, Path::new("/media/STICK"), Duration::from_secs(10));
        helper.join().unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "The drive is in use, try again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
