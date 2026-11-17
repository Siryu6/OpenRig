//! The Add effect page: the installed plugins and built-in blocks, narrowed
//! by family and by a text search. The full list stays here; the page shows
//! the entries that match.

use std::cell::RefCell;

use slint::ComponentHandle;

use pedalboard::state::AppState;
use pedalboard::ui::search;
use pedalboard::ui::slint_bridge as views;

use crate::{model, plates, rgb, App, PluginItem};

/// One entry and the folded text the search looks in
struct Entry {
    item: PluginItem,
    text: String,
}

thread_local! {
    static ENTRIES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// The store's words for plugin `uri` (family, tags…), empty without a store
fn store_words(uri: &str) -> String {
    #[cfg(feature = "store")]
    return crate::catalog_ui::search_words_for(uri);
    #[cfg(not(feature = "store"))]
    {
        let _ = uri;
        String::new()
    }
}

/// Fill the page from the latest LV2 scan, keeping the search and family
pub(crate) fn set_plugins(app: &App, state: &AppState) {
    let plate_size = plates::block_size(app, 1.0);
    let entries: Vec<Entry> = views::plugin_views(state)
        .into_iter()
        .map(|p| {
            let text = p.search_text(&store_words(&p.uri));
            let picture = crate::store_picture(&p.uri).map(|(p, _)| p);
            let category = state.plugin_category(&p.uri);
            let item = PluginItem {
                faceplate: plates::block_face(
                    &p.uri,
                    p.faceplate.as_ref(),
                    category.as_ref(),
                    plate_size,
                ),
                has_picture: picture.is_some(),
                picture: picture.unwrap_or_default(),
                uri: p.uri.into(),
                name: p.name.into(),
                author: p.author.into(),
                category: p.category.into(),
                kind: p.kind.into(),
                hint: p.hint.into(),
                color: rgb(p.color),
            };
            Entry { item, text }
        })
        .collect();
    app.set_browser_total(entries.len() as i32);
    ENTRIES.with(|e| *e.borrow_mut() = entries);
    app.set_demo_plugins(state.lv2_host.is_demo());
    show(app);
}

/// Show the entries of the page's family that match its search
fn show(app: &App) {
    let query = app.get_browser_query();
    let family = app.get_browser_category();
    let shown: Vec<PluginItem> = ENTRIES.with(|e| {
        e.borrow()
            .iter()
            .filter(|e| family == "All" || e.item.category == family)
            .filter(|e| search::matches(&e.text, &query))
            .map(|e| e.item.clone())
            .collect()
    });
    app.set_browser_plugins(model(shown));
}

pub(crate) fn setup(app: &App) {
    let weak = app.as_weak();
    app.on_browser_filter(move || {
        if let Some(app) = weak.upgrade() {
            show(&app);
        }
    });
}
