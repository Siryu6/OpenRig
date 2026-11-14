//! Rigs panel of the preset page: export the current chain as a `.openrig`
//! file (to `<data>/rigs`, and to a USB drive when one is mounted), list
//! the rigs found there, and import one: show what it needs, install it
//! (store plugins, TONE3000 models with the user's account), save it as a
//! new preset and load it. Reading the catalog, writing files and
//! downloading run on worker threads.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use slint::{ComponentHandle, Weak};
use tracing::{info, warn};

use pedalboard::rigs::format::{Rig, Tone3000Model};
use pedalboard::rigs::import::{self, Plan, Step, Subject};
use pedalboard::rigs::{self, export, CatalogPlugin, RigFile};
use pedalboard::state::{data_dir, AppState};

use crate::{model, show_status, spawn, ui, App, RigFileInfo, RigItem, Rigs};

#[derive(Default)]
struct Page {
    files: Vec<RigFile>,
    plan: Option<Plan>,
    /// The drive the note speaks of, and what it says of it
    /// (", and on <drive>"): dropped when the drive goes
    note_drive: Option<(PathBuf, String)>,
}

thread_local! {
    static PAGE: RefCell<Page> = RefCell::new(Page::default());
}

fn with_page<R>(f: impl FnOnce(&mut Page) -> R) -> Option<R> {
    PAGE.with(|p| match p.try_borrow_mut() {
        Ok(mut page) => Some(f(&mut page)),
        Err(_) => {
            warn!("Rigs page in use, update skipped");
            None
        }
    })
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let rigs = app.global::<Rigs>();
    {
        let (weak, st) = (app.as_weak(), state.clone());
        rigs.on_opened(move || {
            if let Some(app) = weak.upgrade() {
                opened(&app, &st);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        rigs.on_export(move |usb| {
            if let Some(app) = weak.upgrade() {
                export_current(&app, &st, usb);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        rigs.on_set_author(move |name| {
            st.set_rig_author(&name);
            if let Some(app) = weak.upgrade() {
                app.global::<Rigs>().set_author(st.rig_author().into());
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        rigs.on_open_file(move |index| {
            if let Some(app) = weak.upgrade() {
                open_file(&app, &st, index);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        rigs.on_install(move || {
            if let Some(app) = weak.upgrade() {
                install(&app, &st);
            }
        });
    }
    {
        let weak = app.as_weak();
        rigs.on_close_plan(move || {
            with_page(|p| p.plan = None);
            if let Some(app) = weak.upgrade() {
                let rigs = app.global::<Rigs>();
                rigs.set_plan_open(false);
                rigs.set_installed(false);
                rigs.set_progress("".into());
            }
        });
    }
}

/// The panel opens (or Refresh): the current preset, the drives, the files
fn opened(app: &App, state: &Arc<AppState>) {
    let rigs = app.global::<Rigs>();
    rigs.set_preset_name(state.current_preset_name().into());
    rigs.set_author(state.rig_author().into());
    if !rigs.get_busy() {
        rigs.set_note("".into());
        with_page(|p| p.note_drive = None);
    }
    list(&app.as_weak());
}

/// A USB drive was plugged in, ejected or pulled out: list again
pub(crate) fn drives_changed(app: &App, now: &[PathBuf]) {
    let stale = with_page(|p| {
        if p.note_drive
            .as_ref()
            .is_some_and(|(d, _)| pedalboard::drives::gone(d, now))
        {
            p.note_drive.take()
        } else {
            None
        }
    })
    .flatten();
    if let Some((_, said)) = stale {
        let rigs = app.global::<Rigs>();
        let note = rigs.get_note().replace(&said, "");
        rigs.set_note(note.into());
    }
    list(&app.as_weak());
}

/// Read the rigs of the rigs folder and the USB drives, on a worker thread
fn list(weak: &Weak<App>) {
    let weak = weak.clone();
    spawn("rigs-list", move || {
        let drives = rigs::removable_drives();
        let mut files = rigs::list(&rigs::rigs_dir(&data_dir()), false);
        for drive in &drives {
            files.extend(rigs::list(drive, true));
        }
        let usb = !drives.is_empty();
        // The drive "To USB" writes to and Eject ejects
        let drive = drives
            .first()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui(&weak, move |app| {
            let infos: Vec<RigFileInfo> = files.iter().map(file_info).collect();
            with_page(|p| p.files = files);
            let rigs = app.global::<Rigs>();
            rigs.set_files(model(infos));
            rigs.set_usb(usb);
            rigs.set_drive(drive.into());
        });
    });
}

fn file_info(file: &RigFile) -> RigFileInfo {
    // First what tells apart two rigs of the same name: file and date
    let file_name = file
        .path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let detail = match &file.rig {
        Ok(rig) => {
            let chain = rig.chain();
            let blocks = chain.plugins().len();
            let mut parts = vec![file_name];
            if let Some(date) = export_date(&rig.meta.created) {
                parts.push(date);
            }
            parts.push(plural(blocks, "block"));
            let files = rig.dependencies.files.len();
            if files > 0 {
                parts.push(plural(files, "model"));
            }
            if !rig.meta.author.is_empty() {
                parts.push(format!("by {}", rig.meta.author));
            }
            parts.join(" · ")
        }
        Err(e) => e.clone(),
    };
    RigFileInfo {
        name: file.name().into(),
        detail: detail.into(),
        usb: file.removable,
        readable: file.rig.is_ok(),
    }
}

/// When a rig was exported, in local time ("06/11/2026 18:30")
fn export_date(created: &str) -> Option<String> {
    let time = chrono::DateTime::parse_from_rfc3339(created).ok()?;
    Some(
        time.with_timezone(&chrono::Local)
            .format("%d/%m/%Y %H:%M")
            .to_string(),
    )
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The plugins of the store catalog: the kept index, else a fresh one.
/// Not on the UI thread (may download).
fn catalog() -> Vec<CatalogPlugin> {
    #[cfg(feature = "store")]
    {
        let store = pedalboard::store::Store::new(&data_dir());
        let index = store.cached().or_else(|| match store.refresh() {
            Ok(index) => Some(index),
            Err(e) => {
                warn!(
                    error = format_args!("{e:#}"),
                    "Plugin catalog unavailable for rigs"
                );
                None
            }
        });
        if let Some(index) = index {
            return rigs::catalog_plugins(&index, &store.installed());
        }
    }
    Vec::new()
}

/// The rig of `preset`, resolved with this pedalboard's plugins, store
/// catalog and models. Not on the UI thread (the catalog may download).
pub(crate) fn export_preset(
    state: &AppState,
    preset: &pedalboard::presets::Preset,
    author: String,
    options: export::Options,
) -> anyhow::Result<export::Exported> {
    let catalog = catalog();
    let scanner = state.lv2_host.scanner();
    let plugin_name = |uri: &str| scanner.find_by_uri(uri).map(|p| p.name.clone());
    let plugin_class = |uri: &str| {
        scanner
            .find_by_uri(uri)
            .map(|p| (p.category.clone(), p.class.clone()))
    };
    let models_dir = state.models_dir();
    let context = export::Context {
        models_dir: &models_dir,
        catalog: &catalog,
        plugin_name: &plugin_name,
        plugin_class: &plugin_class,
        author,
        app_version: env!("CARGO_PKG_VERSION"),
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    export::export(preset, &context, options)
}

/// Export the chain as it is now, on a worker thread
fn export_current(app: &App, state: &Arc<AppState>, usb: bool) {
    let rigs = app.global::<Rigs>();
    if rigs.get_busy() {
        return;
    }
    let preset = state.current_as_preset();
    let options = export::Options {
        include_midi: rigs.get_include_midi(),
    };
    let author = state.rig_author();
    let shown = rigs.get_drive();
    rigs.set_busy(true);
    rigs.set_note("Exporting…".into());
    with_page(|p| p.note_drive = None);
    let (weak, st) = (app.as_weak(), state.clone());
    let started = spawn("rigs-export", move || {
        let result = (|| {
            let exported = export_preset(&st, &preset, author, options)?;
            let path = rigs::write(&rigs::rigs_dir(&data_dir()), &exported.rig)?;
            info!(path = %path.display(), "Rig exported");
            let mut note = format!(
                "Saved as {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            let mut on_drive = None;
            if usb {
                match pedalboard::drives::chosen(&rigs::removable_drives(), &shown) {
                    Some(drive) => match rigs::write(&drive, &exported.rig) {
                        Ok(copy) => {
                            info!(path = %copy.display(), "Rig copied to a drive");
                            let said = format!(", and on {}", pedalboard::drives::name(&drive));
                            note.push_str(&said);
                            on_drive = Some((drive, said));
                        }
                        Err(e) => {
                            warn!(error = format_args!("{e:#}"), "Rig not copied to the drive");
                            note.push_str(", not on the USB drive");
                        }
                    },
                    None => note.push_str(", no USB drive found"),
                }
            }
            let local = exported.local_items();
            if !local.is_empty() {
                note.push_str(&format!(". Only on this pedalboard: {}", local.join(", ")));
            }
            if !exported.dropped.is_empty() {
                note.push_str(&format!(
                    ". Left out: {} outside the models folder",
                    plural(exported.dropped.len(), "file")
                ));
            }
            anyhow::Ok((note, on_drive))
        })();
        ui(&weak, move |app| {
            let rigs = app.global::<Rigs>();
            rigs.set_busy(false);
            let (note, on_drive) = match result {
                Ok(done) => done,
                Err(e) => {
                    warn!(error = format_args!("{e:#}"), "Rig not exported");
                    (format!("Not exported: {e:#}"), None)
                }
            };
            with_page(|p| p.note_drive = on_drive);
            show_status(app, note.clone().into());
            rigs.set_note(note.into());
            list(&app.as_weak());
        });
    });
    if !started {
        rigs.set_busy(false);
        rigs.set_note("Cannot export now".into());
    }
}

/// Open the rig `index` of the list
fn open_file(app: &App, state: &Arc<AppState>, index: i32) {
    let rig: Option<Rig> = with_page(|p| {
        usize::try_from(index)
            .ok()
            .and_then(|i| p.files.get(i))
            .and_then(|f| f.rig.as_ref().ok().cloned())
    })
    .flatten();
    let Some(rig) = rig else {
        return;
    };
    open_rig(app, state, rig, false);
}

/// Open `rig` in the import screen: what it needs, on a worker thread (the
/// catalog may have to be downloaded); then install it right away with
/// `install_now` (a rig downloaded from openrig.eu, the user tapped Install)
pub(crate) fn open_rig(app: &App, state: &Arc<AppState>, rig: Rig, install_now: bool) {
    let rigs = app.global::<Rigs>();
    if rigs.get_busy() {
        return;
    }
    rigs.set_busy(true);
    rigs.set_installed(false);
    rigs.set_plan_name(rig.meta.name.clone().into());
    rigs.set_plan_summary("Checking what this rig needs…".into());
    rigs.set_progress("Checking what this rig needs…".into());
    rigs.set_plan_warning("".into());
    rigs.set_plan_items(model(Vec::new()));
    rigs.set_plan_open(true);
    let (weak, st) = (app.as_weak(), state.clone());
    let started = spawn("rigs-plan", move || {
        let catalog = catalog();
        let scanner = st.lv2_host.scanner();
        let installed = |uri: &str| scanner.find_by_uri(uri).is_some();
        let models_dir = st.models_dir();
        let context = import::Context {
            models_dir: &models_dir,
            catalog: &catalog,
            installed: &installed,
            tone3000: cfg!(feature = "tone3000"),
        };
        let result = import::plan(rig, &context);
        ui(&weak, move |app| {
            let rigs = app.global::<Rigs>();
            rigs.set_busy(false);
            rigs.set_progress("".into());
            match result {
                Ok(plan) => {
                    show_plan(app, plan);
                    if install_now {
                        install(app, &st);
                    }
                }
                Err(e) => {
                    warn!(error = format_args!("{e:#}"), "Rig refused");
                    rigs.set_plan_summary("This rig cannot be imported".into());
                    rigs.set_plan_warning(format!("{e:#}").into());
                    rigs.set_installed(true); // nothing to install: Done only
                }
            }
        });
    });
    if !started {
        rigs.set_busy(false);
        rigs.set_plan_open(false);
    }
}

fn show_plan(app: &App, plan: Plan) {
    let rigs = app.global::<Rigs>();
    let items: Vec<RigItem> = plan.items.iter().map(plan_item).collect();
    rigs.set_plan_items(model(items));
    rigs.set_plan_summary(summary(&plan).into());
    let mut warnings = Vec::new();
    if plan.to_download() > 0 && !tone3000_connected() {
        warnings.push("Connect TONE3000 in Settings to get the models".to_string());
    }
    if plan.missing() > 0 {
        warnings.push(format!(
            "{} missing: imported without",
            plural(plan.missing(), "item")
        ));
    }
    rigs.set_plan_warning(warnings.join(" · ").into());
    with_page(|p| p.plan = Some(plan));
}

fn summary(plan: &Plan) -> String {
    let blocks = plan.rig.chain().plugins().len();
    let mut parts = vec![plural(blocks, "block")];
    if plan.to_install() > 0 {
        parts.push(format!(
            "{} to install",
            plural(plan.to_install(), "plugin")
        ));
    }
    if plan.to_download() > 0 {
        parts.push(format!(
            "{} to download",
            plural(plan.to_download(), "model")
        ));
    }
    if plan.missing() > 0 {
        parts.push(format!("{} missing", plan.missing()));
    }
    if plan.to_install() + plan.to_download() + plan.missing() == 0 {
        parts.push("everything is on this pedalboard".into());
    }
    parts.join(" · ")
}

fn plan_item(item: &import::Item) -> RigItem {
    let kind = match item.subject {
        Subject::Plugin { .. } => "Plugin",
        Subject::File { .. } => "Model",
    };
    let (status, detail) = match &item.step {
        Step::Ready if item.local_path.is_some() => ("ready", "already downloaded".to_string()),
        Step::Ready => ("ready", "on this pedalboard".to_string()),
        Step::Store { .. } => ("store", "from the plugin store".to_string()),
        Step::Tone3000(model) => ("tone3000", tone3000_detail(model)),
        Step::Missing(reason) => ("missing", reason.clone()),
    };
    RigItem {
        label: item.label.clone().into(),
        detail: format!("{kind} · {detail}").into(),
        status: status.into(),
    }
}

fn tone3000_detail(model: &Tone3000Model) -> String {
    let mut detail = "from TONE3000".to_string();
    if !model.creator.is_empty() {
        detail.push_str(&format!(", by {}", model.creator));
    }
    if let Some(license) = model.license.as_deref().filter(|l| !l.is_empty()) {
        detail.push_str(&format!(" ({license})"));
    }
    detail
}

#[cfg(feature = "tone3000")]
fn tone3000_connected() -> bool {
    crate::tone3000_ui::connected()
}

#[cfg(not(feature = "tone3000"))]
fn tone3000_connected() -> bool {
    false
}

/// Installs with this app's store and TONE3000 account
struct AppInstaller {
    weak: Weak<App>,
    models_dir: std::path::PathBuf,
    #[cfg(feature = "store")]
    store: pedalboard::store::Store,
    #[cfg(feature = "store")]
    index: Option<pedalboard::store::index::Index>,
    /// The account is gone: no use asking again for each model
    #[cfg_attr(not(feature = "tone3000"), allow(dead_code))]
    signed_out: Option<String>,
}

impl import::Installer for AppInstaller {
    #[cfg(feature = "store")]
    fn install_plugin(&mut self, plugin_id: &str) -> anyhow::Result<()> {
        // Installing needs an index that is not stale
        let index = match self.index.take() {
            Some(index) if !index.is_stale() => index,
            _ => self.store.refresh()?,
        };
        let result = self.store.install(&index, plugin_id, |_, _| {});
        self.index = Some(index);
        result
    }

    #[cfg(not(feature = "store"))]
    fn install_plugin(&mut self, _plugin_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("This OpenRig has no plugin store")
    }

    #[cfg(feature = "tone3000")]
    fn download_model(&mut self, model: &Tone3000Model) -> anyhow::Result<std::path::PathBuf> {
        use pedalboard::tone3000::{library, AuthError};
        if let Some(reason) = &self.signed_out {
            anyhow::bail!("{reason}");
        }
        let result = crate::tone3000_ui::with_account(|token| {
            library::download_for_rig(
                token,
                &self.models_dir,
                model.tone_id,
                model.model_id,
                &model.model_name,
            )
        });
        if let Err(e) = &result {
            if let Some(auth @ (AuthError::SignedOut | AuthError::Revoked)) = e.downcast_ref() {
                self.signed_out = Some(auth.to_string());
            }
        }
        result
    }

    #[cfg(not(feature = "tone3000"))]
    fn download_model(&mut self, _model: &Tone3000Model) -> anyhow::Result<std::path::PathBuf> {
        let _ = &self.models_dir;
        anyhow::bail!("This OpenRig has no TONE3000 support")
    }

    fn progress(&mut self, done: usize, total: usize, label: &str) {
        let text = if label.is_empty() {
            "Saving the preset…".to_string()
        } else {
            format!("{} of {}: {label}", done + 1, total)
        };
        ui(&self.weak, move |app| {
            app.global::<Rigs>().set_progress(text.into());
        });
    }
}

/// Install the open rig, on a worker thread, then save and load its preset
fn install(app: &App, state: &Arc<AppState>) {
    let rigs = app.global::<Rigs>();
    if rigs.get_busy() {
        return;
    }
    let Some(plan) = with_page(|p| p.plan.clone()).flatten() else {
        return;
    };
    rigs.set_busy(true);
    rigs.set_progress("Starting…".into());
    let (weak, st) = (app.as_weak(), state.clone());
    let started = spawn("rigs-install", move || {
        let mut installer = AppInstaller {
            weak: weak.clone(),
            models_dir: st.models_dir(),
            #[cfg(feature = "store")]
            store: pedalboard::store::Store::new(&data_dir()),
            #[cfg(feature = "store")]
            index: None,
            signed_out: None,
        };
        #[cfg(feature = "store")]
        {
            installer.index = installer.store.cached();
        }
        let result = import::install(&plan, &mut installer);
        if result.as_ref().is_ok_and(|o| o.plugins_installed) {
            st.lv2_host.rescan();
        }
        ui(&weak, move |app| finish(app, &st, &plan, result));
    });
    if !started {
        rigs.set_busy(false);
        rigs.set_progress("".into());
        show_status(app, "Cannot install now".into());
    }
}

/// Back on the UI thread after an install
fn finish(app: &App, state: &Arc<AppState>, plan: &Plan, result: anyhow::Result<import::Outcome>) {
    let rigs = app.global::<Rigs>();
    rigs.set_busy(false);
    rigs.set_progress("".into());
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!(error = format_args!("{e:#}"), "Rig not installed");
            show_status(app, format!("Rig not installed: {e:#}").into());
            return;
        }
    };
    if outcome.plugins_installed {
        crate::set_available_plugins(app, state);
    }
    // What happened to each item
    let items: Vec<RigItem> = plan
        .items
        .iter()
        .map(|item| {
            let mut row = plan_item(item);
            let label = item.label.as_str();
            if let Some((_, reason)) = outcome.failed.iter().find(|(l, _)| l == label) {
                row.status = "failed".into();
                row.detail = reason.clone().into();
            } else if outcome.done.iter().any(|l| l == label) {
                row.status = "done".into();
            }
            row
        })
        .collect();
    rigs.set_plan_items(model(items));

    let name = outcome.preset.name.clone();
    let id = match state.add_preset(outcome.preset) {
        Ok(id) => id,
        Err(e) => {
            warn!(error = format_args!("{e:#}"), "Rig preset not saved");
            show_status(app, format!("Preset not saved: {e:#}").into());
            return;
        }
    };
    crate::load_preset(app, state, &id);
    rigs.set_installed(true);
    rigs.set_preset_name(state.current_preset_name().into());
    let mut message = format!("Preset \"{name}\" added");
    if !outcome.failed.is_empty() {
        message.push_str(&format!(
            ", {} failed",
            plural(outcome.failed.len(), "item")
        ));
    }
    if !outcome.missing.is_empty() {
        message.push_str(&format!(
            ", {} missing",
            plural(outcome.missing.len(), "item")
        ));
    }
    info!(
        preset = %name,
        installed = outcome.done.len(),
        failed = outcome.failed.len(),
        missing = outcome.missing.len(),
        "Rig imported"
    );
    rigs.set_plan_summary(message.clone().into());
    // Blocks without their model report it as the chain is built: added
    // to this summary rather than hiding it
    crate::show_status_pinned(app, message.into());
}
