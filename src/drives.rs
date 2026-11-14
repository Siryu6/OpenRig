//! USB drives: listed (`list`), noticed when they come and go (`watch`),
//! and ejected before they are pulled out (`Drive::eject`). A `Drive` is
//! one mount point: its name, the folders rigs and backups are read from
//! and written to, and its eject. On the pedalboard OS, udev mounts them in
//! `/media` and the root helper `/usr/libexec/pedalboard/drives` ejects
//! them (a FIFO `request` and a file `status` in `PEDALBOARD_DRIVES_DIR`,
//! the protocol of the settings helper); elsewhere the desktop does both.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::system_config::{request_state, send_request};

/// Directory of the root helper, set by the launcher of the pedalboard OS
pub const DIR_ENV: &str = "PEDALBOARD_DRIVES_DIR";

/// Folder of the drives, instead of the system's places
pub const MEDIA_DIR_ENV: &str = "PEDALBOARD_MEDIA_DIR";

/// How often `watch` looks at the drives
pub const WATCH_EVERY: Duration = Duration::from_secs(2);

/// How long an eject may take (writing what is cached to a slow stick)
const EJECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Folder at the top of a drive for the app when it cannot write at the
/// top: on ext4 the OS makes it and gives it to the app (the other files
/// keep their owners). Same name in the OS helper.
pub const FOLDER: &str = "OpenRig";

fn helper_dir() -> Option<PathBuf> {
    std::env::var_os(DIR_ENV)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// Drives can be ejected from the app (the pedalboard OS)
pub fn can_eject() -> bool {
    helper_dir().is_some()
}

/// A removable drive mounted by the system
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Drive {
    path: PathBuf,
}

impl Drive {
    /// The drive mounted on `path`
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Its mount point: what the pages keep to name it
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its name, as the pages show it (its label)
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn own_folder(&self) -> Option<PathBuf> {
        let own = self.path.join(FOLDER);
        own.symlink_metadata()
            .is_ok_and(|m| m.is_dir())
            .then_some(own)
    }

    /// The folders where rigs and backups are looked for: its top, and its
    /// `OpenRig` folder when there is one (a link is not)
    pub fn folders(&self) -> Vec<PathBuf> {
        std::iter::once(self.path.clone())
            .chain(self.own_folder())
            .collect()
    }

    /// Where the app writes: the top when it may write there (FAT and
    /// exFAT drives belong to it), else the `OpenRig` folder (ext4)
    pub fn write_folder(&self) -> PathBuf {
        match self.own_folder() {
            Some(own) if !writable(&self.path) => own,
            _ => self.path.clone(),
        }
    }

    /// Everything is written, its file systems unmounted and it is
    /// powered off. Waits for the helper: not on the UI thread.
    pub fn eject(&self) -> Result<()> {
        let Some(dir) = helper_dir() else {
            bail!("Eject the drive from the system");
        };
        eject_with(&dir, &self.path, EJECT_TIMEOUT)
    }
}

/// Removable drives mounted by the system, where rigs and backups can be
/// copied to and read from: `/media/<drive>` (the OpenRig OS),
/// `/media/<user>/<drive>`, `/run/media/<user>/<drive>` (Linux desktops),
/// `/Volumes/<drive>` (macOS). Empty elsewhere (Windows).
/// `PEDALBOARD_MEDIA_DIR` names a folder whose subfolders are taken as
/// drives instead (a system that mounts drives there, tests).
pub fn list() -> Vec<Drive> {
    let mut paths = Vec::new();
    if let Some(media) = std::env::var_os(MEDIA_DIR_ENV).filter(|m| !m.is_empty()) {
        paths = subdirs(Path::new(&media));
    } else if cfg!(target_os = "macos") {
        paths.extend(subdirs(Path::new("/Volumes")));
        // The system disk is listed there as a link to /
        paths.retain(|d| std::fs::canonicalize(d).is_ok_and(|c| c != Path::new("/")));
    } else if cfg!(target_os = "linux") {
        for root in ["/media", "/run/media"] {
            for dir in subdirs(Path::new(root)) {
                // /media/<drive> on some systems, /media/<user>/<drive> on others
                if is_mount_point(&dir) {
                    paths.push(dir);
                } else {
                    paths.extend(subdirs(&dir).into_iter().filter(|d| is_mount_point(d)));
                }
            }
        }
    }
    let mut drives: Vec<Drive> = paths.into_iter().map(Drive::new).collect();
    drives.sort();
    drives
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// A folder on another device than its parent
#[cfg(unix)]
fn is_mount_point(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(own), Some(Ok(parent))) = (std::fs::metadata(dir), dir.parent().map(std::fs::metadata))
    else {
        return false;
    };
    own.dev() != parent.dev()
}

#[cfg(not(unix))]
fn is_mount_point(_dir: &Path) -> bool {
    false
}

/// The drive a page shows (`shown`, the path of one of `list`), if it is
/// still plugged in
pub fn find(shown: &str) -> Option<Drive> {
    find_in(list(), shown)
}

fn find_in(drives: Vec<Drive>, shown: &str) -> Option<Drive> {
    if shown.is_empty() {
        return None;
    }
    drives.into_iter().find(|d| d.path.as_os_str() == shown)
}

/// A message made of parts, some about a drive ("Saved as Lead.openrig",
/// ", and on STICK"): those are dropped once their drive goes
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    parts: Vec<(String, Option<Drive>)>,
}

impl Note {
    /// Add `text`
    pub fn push(&mut self, text: impl Into<String>) {
        self.parts.push((text.into(), None));
    }

    /// Add `text`, which holds while `drive` is plugged in
    pub fn push_on(&mut self, drive: &Drive, text: impl Into<String>) {
        self.parts.push((text.into(), Some(drive.clone())));
    }

    pub fn text(&self) -> String {
        self.parts.iter().map(|(t, _)| t.as_str()).collect()
    }

    /// Drop the parts about drives not in `now` (the drives now); true
    /// when there were some
    pub fn drop_gone(&mut self, now: &[Drive]) -> bool {
        let before = self.parts.len();
        self.parts
            .retain(|(_, drive)| drive.as_ref().is_none_or(|d| now.contains(d)));
        self.parts.len() != before
    }
}

impl From<&str> for Note {
    fn from(text: &str) -> Self {
        let mut note = Note::default();
        if !text.is_empty() {
            note.push(text);
        }
        note
    }
}

impl From<String> for Note {
    fn from(text: String) -> Self {
        text.as_str().into()
    }
}

#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path for the call
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn writable(dir: &Path) -> bool {
    std::fs::metadata(dir).is_ok_and(|m| !m.permissions().readonly())
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

/// Call `changed` with the drives at once, then each time they change (one
/// plugged in, ejected or pulled out), from a thread of its own that looks
/// every `WATCH_EVERY`: a few directory reads, and a stat that cannot hang
/// the UI on a drive gone bad. Runs as long as the app.
pub fn watch(changed: impl Fn(Vec<Drive>) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("drives-watch".into())
        .spawn(move || {
            let mut last = None;
            loop {
                let now = list();
                if last.as_ref() != Some(&now) {
                    tracing::info!(drives = ?now, "USB drives changed");
                    last = Some(now.clone());
                    changed(now);
                }
                std::thread::sleep(WATCH_EVERY);
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
    fn finds_the_drive_shown_if_still_there() {
        let drives = || vec![Drive::new("/media/A"), Drive::new("/media/B")];
        assert_eq!(find_in(drives(), "/media/B"), Some(Drive::new("/media/B")));
        assert_eq!(find_in(drives(), "/media/C"), None);
        assert_eq!(find_in(drives(), ""), None);
        assert_eq!(find_in(Vec::new(), "/media/A"), None);
        assert_eq!(Drive::new("/media/MY STICK").name(), "MY STICK");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn lists_the_folders_of_the_media_dir() {
        let media = dir("media");
        std::fs::create_dir_all(media.join("B")).unwrap();
        std::fs::create_dir_all(media.join("A")).unwrap();
        std::fs::write(media.join("file"), "").unwrap();
        assert_eq!(
            subdirs(&media)
                .into_iter()
                .map(Drive::new)
                .collect::<std::collections::BTreeSet<_>>(),
            [Drive::new(media.join("A")), Drive::new(media.join("B"))].into()
        );
        std::fs::remove_dir_all(&media).unwrap();
    }

    #[test]
    fn a_note_drops_what_it_says_of_a_drive_gone() {
        let (a, b) = (Drive::new("/media/A"), Drive::new("/media/B"));
        let mut note = Note::default();
        note.push("Saved as Lead.openrig");
        note.push_on(&a, ", and on A");
        note.push(". Only on this pedalboard: amp.nam");
        assert_eq!(
            note.text(),
            "Saved as Lead.openrig, and on A. Only on this pedalboard: amp.nam"
        );
        assert!(!note.drop_gone(&[a.clone(), b.clone()]));
        assert!(note.drop_gone(std::slice::from_ref(&b)));
        assert_eq!(
            note.text(),
            "Saved as Lead.openrig. Only on this pedalboard: amp.nam"
        );
        // A message all about a drive goes with it
        let mut saved = Note::default();
        saved.push_on(&b, "Saved on B: openrig-backup.tar.gz (2 KB)");
        assert!(saved.drop_gone(&[a]));
        assert_eq!(saved, Note::default());
        assert_eq!(Note::from(""), Note::default());
        assert_eq!(Note::from("Exporting…").text(), "Exporting…");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn writes_in_the_openrig_folder_when_the_top_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let top = dir("ext4");
        let drive = Drive::new(&top);
        // FAT, exFAT: the top
        assert_eq!(drive.folders(), vec![top.clone()]);
        assert_eq!(drive.write_folder(), top);
        std::fs::create_dir(top.join(FOLDER)).unwrap();
        assert_eq!(drive.folders(), vec![top.clone(), top.join(FOLDER)]);
        // Writable top: still the top
        assert_eq!(drive.write_folder(), top);
        // ext4 owned by another user: the folder the OS gave the app
        std::fs::set_permissions(&top, std::fs::Permissions::from_mode(0o555)).unwrap();
        if !writable(&top) {
            // (root may write anywhere)
            assert_eq!(drive.write_folder(), top.join(FOLDER));
        }
        std::fs::set_permissions(&top, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_link_is_not_the_openrig_folder() {
        let top = dir("link");
        std::os::unix::fs::symlink("/", top.join(FOLDER)).unwrap();
        let drive = Drive::new(&top);
        assert_eq!(drive.folders(), vec![top.clone()]);
        assert_eq!(drive.write_folder(), top);
        std::fs::remove_dir_all(&top).unwrap();
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
