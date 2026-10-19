//! Plugin store page: the signed catalog index (kept on the board, so it
//! can be browsed offline), search and families, the page of an entry,
//! installing, updating and removing. Downloads and installs run on a
//! worker thread; once a bundle is in place the LV2 plugins are scanned
//! again, so the new plugin can be added to the chain at once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{Color, ComponentHandle, Image, Weak};
use tracing::{info, warn};

use pedalboard::state::AppState;
use pedalboard::store::index::{Index, Package, Plugin};
use pedalboard::store::install::Installed;
use pedalboard::store::{self, Status, Store};
use pedalboard::ui::slint_bridge::block_color;

use crate::{model, App, CatalogFamily, CatalogItem};

/// Families in the order of the Add effect page
const FAMILIES: [&str; 8] = [
    "Distortion",
    "Simulator",
    "Dynamics",
    "Filter",
    "Modulation",
    "Delay",
    "Reverb",
    "Utility",
];
/// The catalog is checked again when the Store page opens past this age
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 3600);

/// What is going on with an entry right now
#[derive(Debug, Clone)]
enum Job {
    Installing(f32),
    Removing,
}

struct Page {
    store: Store,
    index: Option<Index>,
    installed: Vec<Installed>,
    jobs: HashMap<String, Job>,
    /// Last error per entry
    notes: HashMap<String, String>,
    pictures: HashMap<String, Image>,
    query: String,
    family: String,
    busy: bool,
    /// Last refresh error, shown under the search field
    error: Option<String>,
    refreshed: Option<Instant>,
}

thread_local! {
    static PAGE: RefCell<Option<Page>> = const { RefCell::new(None) };
}

fn with_page<R>(f: impl FnOnce(&mut Page) -> R) -> Option<R> {
    PAGE.with(|p| p.borrow_mut().as_mut().map(f))
}

/// Run on the UI thread
fn ui(app: &Weak<App>, f: impl FnOnce(&App) + Send + 'static) {
    let app = app.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app.upgrade() {
            f(&app);
        }
    });
}

/// "39.5 MB", "820 KB"
fn size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

fn colour(plugin: &Plugin) -> Color {
    let name = match (plugin.kind.as_str(), plugin.family.as_str()) {
        ("cab", _) => "orange",
        ("amp" | "combo", _) | (_, "Simulator") => "red",
        (_, "Distortion") => "yellow",
        (_, "Dynamics") => "pink",
        (_, "Filter") => "purple",
        (_, "Modulation") => "blue",
        (_, "Delay") => "green",
        (_, "Reverb") => "teal",
        _ => "silver",
    };
    let rgb = block_color(name).unwrap_or(0x696c72);
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Whether the entry matches every word of the search
fn matches(plugin: &Plugin, query: &str) -> bool {
    let text = format!(
        "{} {} {} {} {} {} {}",
        plugin.name,
        plugin.author,
        plugin.family,
        plugin.subcategory,
        plugin.inspired_by,
        plugin.tags.join(" "),
        plugin.summary
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|w| text.contains(&w.to_lowercase()))
}

impl Page {
    fn status(&self, state: &AppState, package: &Package, plugin: &Plugin) -> Status {
        let loaded = state.lv2_host.scanner().find_by_uri(&plugin.uri).is_some();
        store::status(package, plugin, &self.installed, loaded)
    }

    fn item(&self, state: &AppState, package: &Package, plugin: &Plugin) -> CatalogItem {
        let status = self.status(state, package, plugin);
        let (name, progress) = match self.jobs.get(&plugin.id) {
            Some(Job::Installing(p)) => ("installing", *p),
            Some(Job::Removing) => ("removing", 0.0),
            None => (status.name(), 0.0),
        };
        let note = self
            .notes
            .get(&plugin.id)
            .cloned()
            .unwrap_or_else(|| match &status {
                Status::Unavailable(reason) => reason.clone(),
                Status::Update { installed } => format!("Version {installed} is installed."),
                Status::Manual => {
                    "Found on this pedalboard, not installed by the store: it is left as it is."
                        .to_string()
                }
                _ => String::new(),
            });
        let picture = self.pictures.get(&plugin.id);
        CatalogItem {
            id: plugin.id.as_str().into(),
            name: plugin.name.as_str().into(),
            author: plugin.author.as_str().into(),
            family: plugin.family.as_str().into(),
            subcategory: plugin.subcategory.as_str().into(),
            summary: plugin.summary.as_str().into(),
            description: plugin.description.as_str().into(),
            inspired_by: plugin.inspired_by.as_str().into(),
            tags: plugin.tags.join(" ").into(),
            version: package.version.as_str().into(),
            license: plugin.license.as_str().into(),
            source: package.source.url.as_str().into(),
            official: package.origin == "upstream",
            size: package
                .artifact()
                .map(|a| size(a.size))
                .unwrap_or_default()
                .into(),
            controls: plugin.controls.len() as i32,
            state: name.into(),
            note: note.into(),
            progress,
            kind: plugin.kind.as_str().into(),
            color: colour(plugin),
            picture: picture.cloned().unwrap_or_default(),
            has_picture: picture.is_some(),
            picture_credit: plugin
                .media
                .picture
                .as_ref()
                .map(|p| format!("{}, {}", p.credit, p.license))
                .unwrap_or_default()
                .into(),
        }
    }

    fn status_line(&self) -> String {
        let Some(index) = &self.index else {
            return self.error.clone().unwrap_or_default();
        };
        let date = index.generated.get(..10).unwrap_or(&index.generated);
        let mut line = format!("Catalog of {date}");
        if index.is_stale() {
            line.push_str(" · out of date: refresh it to install");
        }
        if let Some(error) = &self.error {
            line = format!("{error} · showing the {}", line.to_lowercase());
        }
        line
    }

    fn show(&self, app: &App, state: &AppState) {
        let entries: Vec<(&Package, &Plugin)> =
            self.index.iter().flat_map(|i| i.entries()).collect();
        let found: Vec<&(&Package, &Plugin)> = entries
            .iter()
            .filter(|(_, p)| matches(p, &self.query))
            .collect();
        let mut families = vec![CatalogFamily {
            name: "All".into(),
            count: found.len() as i32,
        }];
        // While searching, only the families that have a match (and the picked one)
        families.extend(
            FAMILIES
                .iter()
                .map(|f| CatalogFamily {
                    name: (*f).into(),
                    count: found.iter().filter(|(_, p)| p.family == *f).count() as i32,
                })
                .filter(|f| {
                    self.query.trim().is_empty() || f.count > 0 || f.name == self.family.as_str()
                }),
        );
        let items: Vec<CatalogItem> = found
            .iter()
            .filter(|(_, p)| self.family == "All" || p.family == self.family)
            .map(|(package, plugin)| self.item(state, package, plugin))
            .collect();
        app.set_catalog_items(model(items));
        app.set_catalog_families(model(families));
        app.set_catalog_query(self.query.as_str().into());
        app.set_catalog_family(self.family.as_str().into());
        app.set_catalog_total(entries.len() as i32);
        app.set_catalog_status(self.status_line().into());
        app.set_catalog_busy(self.busy);
    }
}

/// Show the page again from its current state
fn redraw(app: &App, state: &AppState) {
    with_page(|p| p.show(app, state));
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let store = Store::new(&pedalboard::state::data_dir());
    store::install::clean(&store.lv2_dir);
    let index = store.cached();
    let page = Page {
        installed: store.installed(),
        index,
        store,
        jobs: HashMap::new(),
        notes: HashMap::new(),
        pictures: HashMap::new(),
        query: String::new(),
        family: "All".into(),
        busy: false,
        error: None,
        refreshed: None,
    };
    PAGE.with(|p| *p.borrow_mut() = Some(page));
    load_pictures(app, state);
    redraw(app, state);

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_search(move |q| {
        let Some(app) = weak.upgrade() else { return };
        with_page(|p| p.query = q.to_string());
        redraw(&app, &st);
    });

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_pick_family(move |f| {
        let Some(app) = weak.upgrade() else { return };
        with_page(|p| p.family = f.to_string());
        redraw(&app, &st);
    });

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_opened(move || {
        let Some(app) = weak.upgrade() else { return };
        let due = with_page(|p| {
            p.index.is_none()
                || p.index.as_ref().is_some_and(Index::is_stale)
                || p.refreshed.is_none_or(|t| t.elapsed() > REFRESH_AFTER)
        });
        redraw(&app, &st);
        if due == Some(true) {
            refresh(&app, &st, false);
        }
    });

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_refresh(move || {
        let Some(app) = weak.upgrade() else { return };
        refresh(&app, &st, true);
    });

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_install(move |id| {
        let Some(app) = weak.upgrade() else { return };
        install(&app, &st, id.to_string());
    });

    let weak = app.as_weak();
    let st = state.clone();
    app.on_catalog_remove(move |id| {
        let Some(app) = weak.upgrade() else { return };
        remove(&app, &st, id.to_string());
    });

    let st = state.clone();
    app.on_catalog_uri(move |id| {
        with_page(|p| {
            let (_, plugin) = p.index.as_ref()?.find(&id)?;
            st.lv2_host
                .scanner()
                .find_by_uri(&plugin.uri)
                .map(|_| plugin.uri.clone())
        })
        .flatten()
        .unwrap_or_default()
        .into()
    });
}

/// Download the index again, on a worker thread; asked from the refresh
/// button, also look again for plugins copied by hand into the LV2 folder
fn refresh(app: &App, state: &Arc<AppState>, rescan: bool) {
    let Some(store) = with_page(|p| {
        if p.busy {
            return None;
        }
        p.busy = true;
        Some(p.store.clone())
    })
    .flatten() else {
        return;
    };
    redraw(app, state);
    let weak = app.as_weak();
    let st = state.clone();
    std::thread::spawn(move || {
        let result = store.refresh();
        if rescan {
            st.lv2_host.rescan();
        }
        ui(&weak, move |app| {
            with_page(|p| {
                p.busy = false;
                p.installed = p.store.installed();
                match result {
                    Ok(index) => {
                        info!("Plugin catalog serial {} loaded", index.serial);
                        p.index = Some(index);
                        p.error = None;
                        p.refreshed = Some(Instant::now());
                    }
                    Err(e) => {
                        warn!("Plugin catalog not refreshed: {e:#}");
                        p.error = Some(format!("Cannot refresh the catalog: {e}"));
                    }
                }
            });
            if rescan {
                crate::set_available_plugins(app, &st);
            }
            redraw(app, &st);
            load_pictures(app, &st);
        });
    });
}

/// Pictures of the entries: from the media cache, else downloaded on a
/// worker thread and shown as they arrive
fn load_pictures(app: &App, state: &Arc<AppState>) {
    let Some((store, wanted)) = with_page(|p| {
        let wanted: Vec<Plugin> = p
            .index
            .iter()
            .flat_map(|i| i.entries())
            .filter(|(_, e)| e.media.picture.is_some() && !p.pictures.contains_key(&e.id))
            .map(|(_, e)| e.clone())
            .collect();
        (p.store.clone(), wanted)
    }) else {
        return;
    };
    if wanted.is_empty() {
        return;
    }
    let weak = app.as_weak();
    let st = state.clone();
    std::thread::spawn(move || {
        for plugin in wanted {
            let Some(path) = store.picture(&plugin) else {
                continue;
            };
            let st = st.clone();
            ui(&weak, move |app| match Image::load_from_path(&path) {
                Ok(image) => {
                    with_page(|p| p.pictures.insert(plugin.id.clone(), image));
                    redraw(app, &st);
                }
                Err(_) => warn!("Store picture unreadable: {}", path.display()),
            });
        }
    });
}

/// Install (or update) entry `id`, on a worker thread
fn install(app: &App, state: &Arc<AppState>, id: String) {
    let Some((store, index)) = with_page(|p| {
        if p.jobs.contains_key(&id) {
            return None;
        }
        let index = p.index.clone()?;
        p.jobs.insert(id.clone(), Job::Installing(0.0));
        p.notes.remove(&id);
        Some((p.store.clone(), index))
    })
    .flatten() else {
        return;
    };
    redraw(app, state);
    let weak = app.as_weak();
    let st = state.clone();
    std::thread::spawn(move || {
        let mut shown = -1i32;
        let result = store.install(&index, &id, |received, total| {
            let fraction = received as f32 / total.max(1) as f32;
            // Downloading is most of the work; unpacking takes the rest
            let percent = (fraction * 95.0) as i32;
            if percent != shown {
                shown = percent;
                let (id, st) = (id.clone(), st.clone());
                ui(&weak, move |app| {
                    with_page(|p| p.jobs.insert(id, Job::Installing(percent as f32 / 100.0)));
                    redraw(app, &st);
                });
            }
        });
        if result.is_ok() {
            st.lv2_host.rescan();
        }
        finish(&weak, &st, id, result, "installed");
    });
}

/// Remove the bundle of entry `id`, on a worker thread
fn remove(app: &App, state: &Arc<AppState>, id: String) {
    let Some((store, index)) = with_page(|p| {
        if p.jobs.contains_key(&id) {
            return None;
        }
        let index = p.index.clone()?;
        p.jobs.insert(id.clone(), Job::Removing);
        p.notes.remove(&id);
        Some((p.store.clone(), index))
    })
    .flatten() else {
        return;
    };
    redraw(app, state);
    let weak = app.as_weak();
    let st = state.clone();
    std::thread::spawn(move || {
        let result = store.remove(&index, &id);
        if result.is_ok() {
            // Blocks already in the chain keep running until it is rebuilt;
            // a preset that uses the plugin keeps its settings for it
            st.lv2_host.rescan();
        }
        finish(&weak, &st, id, result, "removed");
    });
}

/// Back on the UI thread after an install or a removal
fn finish(
    weak: &Weak<App>,
    state: &Arc<AppState>,
    id: String,
    result: anyhow::Result<()>,
    done: &'static str,
) {
    let st = state.clone();
    ui(weak, move |app| {
        let name = with_page(|p| {
            p.jobs.remove(&id);
            p.installed = p.store.installed();
            let name = p
                .index
                .as_ref()
                .and_then(|i| i.find(&id))
                .map(|(_, e)| e.name.clone())
                .unwrap_or_else(|| id.clone());
            if let Err(e) = &result {
                warn!("Store: {id} not {done}: {e:#}");
                p.notes.insert(id.clone(), format!("{e}"));
            }
            name
        })
        .unwrap_or_default();
        crate::set_available_plugins(app, &st);
        redraw(app, &st);
        let message = match &result {
            Ok(()) => format!("{name} {done}"),
            Err(e) => format!("{name}: {e}"),
        };
        crate::show_status(app, message.into());
    });
}
