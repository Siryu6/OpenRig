//! openrig.eu in the Rigs panel: browse and search the community's rigs,
//! open one, install it (downloaded, then the panel's import: plan, store
//! plugins, TONE3000 models, new preset), link the pedalboard to an
//! account (device code), favourites, and publishing the current preset.
//! Every network call runs on a worker thread; answers come back through
//! `ui()` and are dropped when the user moved on.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use slint::{ComponentHandle, Weak};
use tracing::{info, warn};

use pedalboard::rigs::export;
use pedalboard::rigs::format::Rig;
use pedalboard::rigs::online::{
    self, Account, ApiError, Client, Detail, LinkEvent, Linking, Page as RigPage, Query, Summary,
};
use pedalboard::state::{data_dir, AppState};

use crate::{model, show_status, spawn, ui, App, OnlineDep, OnlineRig, Rigs, RigsOnline};

/// Rigs asked for per page
const PER_PAGE: u32 = 24;

#[derive(Default)]
struct Page {
    /// What the list shows; `query.page` is the last page loaded
    query: Query,
    /// 0 browse, 1 favourites
    tab: i32,
    rigs: Vec<Summary>,
    /// Bumped by every new list, so late answers are dropped
    generation: u64,
    detail: Option<Summary>,
    /// Slugs of the account's favourites
    favourites: HashSet<String>,
    account: Option<Account>,
    linking: Option<Linking>,
    link_generation: u64,
    /// The exported preset waiting to be published, and the account's rig
    /// of the same name
    publish: Option<(Rig, Option<Summary>)>,
}

thread_local! {
    static PAGE: RefCell<Page> = RefCell::new(Page::default());
}

fn with_page<R>(f: impl FnOnce(&mut Page) -> R) -> Option<R> {
    PAGE.with(|p| match p.try_borrow_mut() {
        Ok(mut page) => Some(f(&mut page)),
        Err(_) => {
            warn!("openrig.eu page in use, update skipped");
            None
        }
    })
}

/// The client of the site: with the account when there is one
fn client() -> Client {
    with_page(|p| p.account.as_ref().map(Account::client))
        .flatten()
        .unwrap_or_else(Client::configured)
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let online = app.global::<RigsOnline>();
    online.set_available(true);
    let base = online::base_url();
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string();
    online.set_site(host.into());
    let account = Account::load(&data_dir(), &base);
    with_page(|p| p.account = account);
    apply_account(app);

    {
        let weak = app.as_weak();
        online.on_opened(move || {
            if let Some(app) = weak.upgrade() {
                opened(&app);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_show(move |tab, sort, style| {
            if let Some(app) = weak.upgrade() {
                let online = app.global::<RigsOnline>();
                online.set_tab(tab);
                online.set_sort(sort.clone());
                online.set_style(style.clone());
                with_page(|p| {
                    p.tab = tab;
                    p.query.sort = sort.to_string();
                    p.query.style = style.to_string();
                });
                load(&app, false);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_search(move |text| {
            if let Some(app) = weak.upgrade() {
                let text = text.trim().to_string();
                let online = app.global::<RigsOnline>();
                online.set_query(text.as_str().into());
                online.set_tab(0);
                with_page(|p| {
                    p.tab = 0;
                    p.query.q = text;
                });
                load(&app, false);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_load_more(move || {
            if let Some(app) = weak.upgrade() {
                load(&app, true);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        online.on_open_rig(move |index| {
            if let Some(app) = weak.upgrade() {
                open_rig(&app, &st, index);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        online.on_install(move |slug| {
            if let Some(app) = weak.upgrade() {
                install(&app, &st, &slug);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_toggle_favourite(move || {
            if let Some(app) = weak.upgrade() {
                toggle_favourite(&app);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_link(move || {
            if let Some(app) = weak.upgrade() {
                link(&app);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_cancel_link(move || {
            with_page(|p| {
                p.link_generation += 1;
                p.linking = None;
            });
            if let Some(app) = weak.upgrade() {
                apply_account(&app);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_unlink(move || {
            if let Some(app) = weak.upgrade() {
                unlink(&app);
            }
        });
    }
    {
        let (weak, st) = (app.as_weak(), state.clone());
        online.on_open_publish(move || {
            if let Some(app) = weak.upgrade() {
                open_publish(&app, &st);
            }
        });
    }
    {
        let weak = app.as_weak();
        online.on_publish(move || {
            if let Some(app) = weak.upgrade() {
                publish(&app);
            }
        });
    }
}

/// Show whether this pedalboard is linked, and to whom
fn apply_account(app: &App) {
    let online = app.global::<RigsOnline>();
    let login = with_page(|p| p.account.as_ref().map(|a| a.login.clone())).flatten();
    match login {
        Some(login) => {
            online.set_link_status("linked".into());
            online.set_login(login.into());
        }
        None => {
            online.set_link_status("signed-out".into());
            online.set_login("".into());
            if online.get_tab() == 1 {
                online.set_tab(0);
                with_page(|p| p.tab = 0);
            }
        }
    }
}

/// The site refused the token: forget it here
fn signed_out(app: &App) {
    let had = with_page(|p| p.account.take().is_some()).unwrap_or(false);
    if had {
        if let Err(e) = Account::delete(&data_dir()) {
            warn!(error = format_args!("{e:#}"), "openrig.eu link not deleted");
        }
        info!("openrig.eu refused the token: unlinked");
    }
    with_page(|p| p.favourites.clear());
    apply_account(app);
    app.global::<RigsOnline>()
        .set_link_error(ApiError::Unauthorized.to_string().into());
}

/// The tab opens (or Refresh)
fn opened(app: &App) {
    load(app, false);
    refresh_favourites(&app.as_weak());
}

/// The account's favourites, to show which rigs are
fn refresh_favourites(weak: &Weak<App>) {
    let Some(account) = with_page(|p| p.account.clone()).flatten() else {
        return;
    };
    let weak = weak.clone();
    spawn("openrig-favourites", move || {
        let result = account.client().favourites();
        ui(&weak, move |app| match result {
            Ok(page) => {
                let slugs: HashSet<String> = page.items.into_iter().map(|r| r.slug).collect();
                with_page(|p| p.favourites = slugs);
                refresh_cards(app);
            }
            Err(ApiError::Unauthorized) => signed_out(app),
            Err(e) => warn!(error = %e, "openrig.eu favourites not read"),
        });
    });
}

/// Load the list (the next page with `more`), on a worker thread
fn load(app: &App, more: bool) {
    let online = app.global::<RigsOnline>();
    let Some((generation, tab, query)) = with_page(|p| {
        p.generation += 1;
        if more {
            p.query.page += 1;
        } else {
            p.query.page = 1;
            p.rigs.clear();
        }
        p.query.per_page = PER_PAGE;
        (p.generation, p.tab, p.query.clone())
    }) else {
        return;
    };
    if !more {
        online.set_rigs(model(Vec::new()));
        online.set_has_more(false);
        online.set_count("".into());
    }
    online.set_loading(true);
    online.set_error("".into());
    let client = client();
    let weak = app.as_weak();
    let started = spawn("openrig-list", move || {
        let result = if tab == 1 {
            client.favourites()
        } else {
            client.list(&query)
        };
        ui(&weak, move |app| shown(app, generation, tab, result));
    });
    if !started {
        online.set_loading(false);
    }
}

/// A page arrived
fn shown(app: &App, generation: u64, tab: i32, result: Result<RigPage, ApiError>) {
    if with_page(|p| p.generation) != Some(generation) {
        return; // the user moved on
    }
    let online = app.global::<RigsOnline>();
    online.set_loading(false);
    let page = match result {
        Ok(page) => page,
        Err(ApiError::Unauthorized) => {
            signed_out(app);
            load(app, false);
            return;
        }
        Err(e) => {
            warn!(error = %e, "openrig.eu list not loaded");
            online.set_error(e.to_string().into());
            return;
        }
    };
    let more = tab == 0 && page.has_more();
    let total = page.total;
    with_page(|p| {
        if tab == 1 {
            p.favourites = page.items.iter().map(|r| r.slug.clone()).collect();
        }
        p.rigs.extend(page.items);
    });
    online.set_has_more(more);
    online.set_count(if total == 1 {
        "1 rig".into()
    } else {
        format!("{total} rigs").into()
    });
    refresh_cards(app);
}

/// Show the rigs of the page, with their favourite marks
fn refresh_cards(app: &App) {
    let cards = with_page(|p| {
        p.rigs
            .iter()
            .map(|r| card(r, p.favourites.contains(&r.slug)))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let online = app.global::<RigsOnline>();
    online.set_rigs(model(cards));
    let detail = online.get_detail();
    if !detail.slug.is_empty() {
        let favourite = with_page(|p| p.favourites.contains(detail.slug.as_str())).unwrap_or(false);
        online.set_detail(OnlineRig {
            favourite,
            ..detail
        });
    }
}

fn plural(n: impl Into<i64>, word: &str) -> String {
    match n.into() {
        1 => format!("1 {word}"),
        n => format!("{n} {word}s"),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn card(rig: &Summary, favourite: bool) -> OnlineRig {
    OnlineRig {
        slug: rig.slug.as_str().into(),
        name: rig.name.as_str().into(),
        author: rig.author.login.as_str().into(),
        style: capitalized(&rig.style).into(),
        detail: format!(
            "v{} · {} · {}",
            rig.version,
            plural(rig.blocks, "block"),
            plural(rig.scenes, "scene")
        )
        .into(),
        upvotes: clamp(rig.upvotes),
        downloads: clamp(rig.downloads),
        store: rig.dependencies.store as i32,
        tone3000: rig.dependencies.tone3000 as i32,
        favourite,
    }
}

fn clamp(n: i64) -> i32 {
    n.clamp(0, i64::from(i32::MAX)) as i32
}

/// Open the rig `index` of the list: its detail, on a worker thread
fn open_rig(app: &App, state: &Arc<AppState>, index: i32) {
    let Some((rig, favourite)) = with_page(|p| {
        let rig = usize::try_from(index)
            .ok()
            .and_then(|i| p.rigs.get(i))?
            .clone();
        let favourite = p.favourites.contains(&rig.slug);
        p.detail = Some(rig.clone());
        Some((rig, favourite))
    })
    .flatten() else {
        return;
    };
    let online = app.global::<RigsOnline>();
    online.set_detail(card(&rig, favourite));
    online.set_detail_description(rig.description.as_str().into());
    online.set_detail_scenes(model(Vec::new()));
    online.set_detail_deps(model(Vec::new()));
    online.set_detail_versions("".into());
    online.set_detail_url(rig.url.as_str().into());
    online.set_progress("".into());
    online.set_detail_loading(true);
    online.set_detail_open(true);
    let client = client();
    let (weak, st) = (app.as_weak(), state.clone());
    let slug = rig.slug.clone();
    spawn("openrig-rig", move || {
        // With which of its plugins are on this pedalboard
        let result = client.rig(&slug).map(|detail| {
            let scanner = st.lv2_host.scanner();
            let here: Vec<bool> = detail
                .view
                .dependencies
                .iter()
                .map(|d| {
                    d.plugin_uri()
                        .is_some_and(|uri| scanner.find_by_uri(&uri).is_some())
                })
                .collect();
            (detail, here)
        });
        ui(&weak, move |app| {
            let online = app.global::<RigsOnline>();
            if online.get_detail().slug != slug.as_str() {
                return;
            }
            online.set_detail_loading(false);
            match result {
                Ok((detail, here)) => show_detail(app, &detail, &here),
                Err(e) => {
                    warn!(error = %e, "openrig.eu rig not loaded");
                    online.set_progress(e.to_string().into());
                }
            }
        });
    });
}

fn show_detail(app: &App, detail: &Detail, here: &[bool]) {
    let online = app.global::<RigsOnline>();
    let deps: Vec<OnlineDep> = detail
        .view
        .dependencies
        .iter()
        .enumerate()
        .map(|(i, dep)| {
            let here = here.get(i).copied().unwrap_or(false);
            let detail = match dep.kind.as_str() {
                "builtin" => "Built into OpenRig".to_string(),
                "store" => "Plugin from the store".to_string(),
                "tone3000" => {
                    let mut text = "Model from TONE3000".to_string();
                    if !dep.creator.is_empty() {
                        text.push_str(&format!(", by {}", dep.creator));
                    }
                    if let Some(license) = dep.license.as_deref().filter(|l| !l.is_empty()) {
                        text.push_str(&format!(" ({license})"));
                    }
                    text
                }
                other => format!("{other}: needs a newer OpenRig"),
            };
            OnlineDep {
                label: dep.label().into(),
                detail: detail.into(),
                kind: dep.kind.as_str().into(),
                here,
            }
        })
        .collect();
    online.set_detail_deps(model(deps));
    let scenes: Vec<slint::SharedString> = detail
        .view
        .scenes
        .iter()
        .map(|s| s.name.as_str().into())
        .collect();
    online.set_detail_scenes(model(scenes));
    online.set_detail_description(detail.summary.description.as_str().into());
    let versions: Vec<String> = detail
        .versions
        .iter()
        .take(5)
        .map(|v| {
            let date = chrono::DateTime::from_timestamp(v.created_at, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format(" (%d/%m/%Y)")
                        .to_string()
                })
                .unwrap_or_default();
            if v.changelog.trim().is_empty() {
                format!("v{}{date}", v.version)
            } else {
                format!("v{}{date}: {}", v.version, v.changelog.trim())
            }
        })
        .collect();
    online.set_detail_versions(versions.join("\n").into());
}

/// Download the rig `slug`, then install it through the import screen
fn install(app: &App, state: &Arc<AppState>, slug: &str) {
    if app.global::<Rigs>().get_busy() {
        show_status(
            app,
            "A rig is being installed, wait for it to finish".into(),
        );
        return;
    }
    let online = app.global::<RigsOnline>();
    let name = with_page(|p| {
        p.rigs
            .iter()
            .chain(p.detail.iter())
            .find(|r| r.slug == slug)
            .map(|r| r.name.clone())
    })
    .flatten()
    .unwrap_or_else(|| slug.to_string());
    online.set_progress(format!("Downloading {name}…").into());
    let client = client();
    let (weak, st, slug) = (app.as_weak(), state.clone(), slug.to_string());
    let started = spawn("openrig-download", move || {
        let result = client.download(&slug, None);
        ui(&weak, move |app| {
            let online = app.global::<RigsOnline>();
            match result {
                Ok(rig) => {
                    online.set_progress("".into());
                    info!(slug = %slug, "Rig downloaded from openrig.eu");
                    crate::rigs_ui::open_rig(app, &st, rig, true);
                }
                Err(e) => {
                    warn!(slug = %slug, error = %e, "Rig not downloaded");
                    online.set_progress(format!("Not downloaded: {e}").into());
                    show_status(app, format!("Not downloaded: {e}").into());
                }
            }
        });
    });
    if !started {
        online.set_progress("".into());
    }
}

/// Favourite the opened rig, or not any more
fn toggle_favourite(app: &App) {
    let online = app.global::<RigsOnline>();
    let detail = online.get_detail();
    let on = !detail.favourite;
    let client = client();
    let weak = app.as_weak();
    let slug = detail.slug.to_string();
    spawn("openrig-favourite", move || {
        let result = client.set_favourite(&slug, on);
        ui(&weak, move |app| match result {
            Ok(state) => {
                with_page(|p| {
                    if state.on {
                        p.favourites.insert(slug.clone());
                    } else {
                        p.favourites.remove(&slug);
                    }
                });
                refresh_cards(app);
            }
            Err(ApiError::Unauthorized) => signed_out(app),
            Err(e) => show_status(app, e.to_string().into()),
        });
    });
}

/// Start linking: ask for a code, show it, wait for the user's approval
fn link(app: &App) {
    let online = app.global::<RigsOnline>();
    online.set_link_error("".into());
    online.set_link_status("starting".into());
    let Some(generation) = with_page(|p| {
        p.link_generation += 1;
        p.link_generation
    }) else {
        return;
    };
    let weak = app.as_weak();
    let linking = Linking::start(
        online::base_url(),
        data_dir(),
        online::device_name(),
        move |event| {
            ui(&weak, move |app| link_event(app, generation, event));
        },
    );
    with_page(|p| p.linking = Some(linking));
}

fn link_event(app: &App, generation: u64, event: LinkEvent) {
    if with_page(|p| p.link_generation) != Some(generation) {
        return; // cancelled, or a newer link
    }
    let online = app.global::<RigsOnline>();
    match event {
        LinkEvent::Code(code) => {
            let url = code
                .verification_uri
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/');
            online.set_link_url(url.into());
            online.set_user_code(code.user_code.as_str().into());
            online.set_link_qr(crate::qr_image(code.qr_url()));
            online.set_link_status("waiting".into());
        }
        LinkEvent::Linked(account) => {
            with_page(|p| {
                p.account = Some(account);
                p.linking = None;
            });
            apply_account(app);
            refresh_favourites(&app.as_weak());
            show_status(
                app,
                format!("Linked to {} as @{}", online.get_site(), online.get_login()).into(),
            );
        }
        LinkEvent::Failed(message) => {
            with_page(|p| p.linking = None);
            apply_account(app);
            online.set_link_error(message.into());
        }
        LinkEvent::Cancelled => {
            with_page(|p| p.linking = None);
            apply_account(app);
        }
    }
}

/// Revoke the token on the site and forget it here
fn unlink(app: &App) {
    let Some(account) = with_page(|p| p.account.take()).flatten() else {
        return;
    };
    with_page(|p| p.favourites.clear());
    apply_account(app);
    refresh_cards(app);
    let weak = app.as_weak();
    spawn("openrig-unlink", move || {
        let result = online::unlink(&data_dir(), &account);
        ui(&weak, move |app| match result {
            Ok(()) => show_status(app, "This pedalboard is no longer linked".into()),
            Err(e) => {
                warn!(error = format_args!("{e:#}"), "openrig.eu not unlinked");
                app.global::<RigsOnline>()
                    .set_link_error(format!("{e:#}").into());
            }
        });
    });
}

/// Open the publish screen: export the current preset and check it can be
/// shared; look for the account's rig of the same name
fn open_publish(app: &App, state: &Arc<AppState>) {
    let Some(account) = with_page(|p| {
        p.publish = None;
        p.account.clone()
    })
    .flatten() else {
        return;
    };
    let online = app.global::<RigsOnline>();
    let preset = state.current_as_preset();
    let style = preset
        .tags
        .iter()
        .map(|t| t.trim().to_lowercase())
        .find(|t| online::STYLES.contains(&t.as_str()))
        .unwrap_or_default();
    online.set_publish_name(state.current_preset_name().into());
    online.set_publish_state("checking".into());
    online.set_publish_blockers(model(Vec::new()));
    online.set_publish_style(style.into());
    online.set_publish_description(preset.description.trim().into());
    online.set_own_rig("".into());
    online.set_as_version(true);
    online.set_changelog("".into());
    online.set_publish_message("".into());
    online.set_publish_url("".into());
    online.set_publish_open(true);
    let author = state.rig_author();
    let (weak, st) = (app.as_weak(), state.clone());
    let started = spawn("openrig-publish-check", move || {
        let result =
            crate::rigs_ui::export_preset(&st, &preset, author, export::Options::default()).map(
                |exported| {
                    let blockers = online::publish_blockers(&exported);
                    let own = if blockers.is_empty() {
                        account
                            .client()
                            .own_rig(&account.login, &exported.rig.meta.name)
                            .unwrap_or_else(|e| {
                                warn!(error = %e, "openrig.eu: own rigs not checked");
                                None
                            })
                    } else {
                        None
                    };
                    (exported.rig, blockers, own)
                },
            );
        ui(&weak, move |app| {
            let online = app.global::<RigsOnline>();
            match result {
                Ok((_, blockers, _)) if !blockers.is_empty() => {
                    let blockers: Vec<slint::SharedString> =
                        blockers.into_iter().map(Into::into).collect();
                    online.set_publish_blockers(model(blockers));
                    online.set_publish_state("blocked".into());
                }
                Ok((rig, _, own)) => {
                    online.set_own_rig(
                        own.as_ref()
                            .map(|o| o.name.as_str())
                            .unwrap_or_default()
                            .into(),
                    );
                    with_page(|p| p.publish = Some((rig, own)));
                    online.set_publish_state("ready".into());
                }
                Err(e) => {
                    warn!(
                        error = format_args!("{e:#}"),
                        "Preset not exported for openrig.eu"
                    );
                    online.set_publish_blockers(model(vec![format!("{e:#}").into()]));
                    online.set_publish_state("blocked".into());
                }
            }
        });
    });
    if !started {
        online.set_publish_open(false);
    }
}

/// Upload the exported preset: a new rig, or a new version of the
/// account's rig of the same name
fn publish(app: &App) {
    let online = app.global::<RigsOnline>();
    let Some((Some((mut rig, own)), Some(account))) =
        with_page(|p| (p.publish.clone(), p.account.clone()))
    else {
        return;
    };
    let description = online.get_publish_description().trim().to_string();
    if !description.is_empty() {
        rig.meta.description = description
            .chars()
            .take(pedalboard::rigs::format::limits::DESCRIPTION)
            .collect();
    }
    let style = online.get_publish_style().to_string();
    let changelog = online.get_changelog().to_string();
    let version_of = own.filter(|_| online.get_as_version()).map(|o| o.slug);
    let file = match rig.to_vec_pretty() {
        Ok(file) if file.len() <= online::MAX_PUBLISH_BYTES => file,
        Ok(_) => {
            online.set_publish_state("failed".into());
            online.set_publish_message("This rig is too large for openrig.eu".into());
            return;
        }
        Err(e) => {
            online.set_publish_state("failed".into());
            online.set_publish_message(e.to_string().into());
            return;
        }
    };
    online.set_publish_state("publishing".into());
    online.set_publish_message("".into());
    let weak = app.as_weak();
    let started = spawn("openrig-publish", move || {
        let client = account.client();
        let result = match &version_of {
            Some(slug) => client.new_version(slug, &file, &changelog),
            None => client.publish(&file, &style),
        };
        ui(&weak, move |app| {
            let online = app.global::<RigsOnline>();
            match result {
                Ok(summary) => {
                    info!(slug = %summary.slug, version = summary.version, "Rig published on openrig.eu");
                    let message = if version_of.is_some() {
                        format!(
                            "Version {} of \"{}\" published",
                            summary.version, summary.name
                        )
                    } else {
                        format!("\"{}\" published", summary.name)
                    };
                    online.set_publish_url(summary.url.as_str().into());
                    online.set_publish_message(message.clone().into());
                    online.set_publish_state("done".into());
                    show_status(app, message.into());
                    with_page(|p| p.publish = None);
                }
                Err(ApiError::Unauthorized) => {
                    online.set_publish_open(false);
                    signed_out(app);
                    online.set_account_open(true);
                }
                Err(e) => {
                    warn!(error = %e, "Rig not published");
                    online.set_publish_message(e.to_string().into());
                    online.set_publish_state("failed".into());
                }
            }
        });
    });
    if !started {
        online.set_publish_state("failed".into());
    }
}
