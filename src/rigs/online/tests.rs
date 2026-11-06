//! The API client against a local HTTP server that answers like the site

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

use crate::presets::{Preset, PresetChain};
use crate::rigs::format::Source;
use crate::rigs::{export, import, CatalogPlugin};

/// A request the server got
#[derive(Debug, Clone)]
struct Request {
    method: String,
    /// Path and query
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// What the server answers: status, content type, body
type Answer = (u16, &'static str, Vec<u8>);

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    /// Serve on a free local port; `handler` answers each request
    fn start(handler: impl Fn(&Request) -> Answer + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let (status, content_type, body) = handler(&request);
                seen.lock().unwrap().push(request);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { base, requests }
    }

    fn client(&self) -> Client {
        Client::new(&self.base)
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _): &&(String, String)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let mut body = Vec::new();
    if let Some(length) = header("content-length").and_then(|l| l.parse::<usize>().ok()) {
        body.resize(length, 0);
        reader.read_exact(&mut body).ok()?;
    } else if header("transfer-encoding").is_some_and(|t| t.contains("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

fn json(status: u16, value: serde_json::Value) -> Answer {
    (status, "application/json", value.to_string().into_bytes())
}

fn error(status: u16, code: &str, message: &str) -> Answer {
    json(
        status,
        serde_json::json!({"error": code, "message": message}),
    )
}

fn summary(slug: &str, name: &str, login: &str) -> serde_json::Value {
    serde_json::json!({
        "slug": slug,
        "name": name,
        "description": "Markdown…",
        "style": "crunch",
        "tags": ["plexi", "rock"],
        "author": { "login": login, "url": format!("https://openrig.eu/u/{login}") },
        "version": 2,
        "upvotes": 14, "favourites": 6, "downloads": 120, "comments": 3,
        "blocks": 6, "scenes": 3,
        "dependencies": { "store": 1, "tone3000": 2, "builtin": 3 },
        "created_at": 1793000000, "updated_at": 1793500000,
        "url": format!("https://openrig.eu/r/{slug}"),
        "download_url": format!("https://openrig.eu/api/v1/rigs/{slug}/download"),
        "a_field_of_later": true
    })
}

/// A fresh folder; tests run in parallel, each call gets its own
fn temp(name: &str) -> PathBuf {
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pb-online-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn lists_and_searches() {
    let server = Server::start(|_| {
        json(
            200,
            serde_json::json!({
                "items": [summary("plexi-crunch", "Plexi Crunch", "marta")],
                "total": 37, "page": 1, "per_page": 24
            }),
        )
    });
    let page = server
        .client()
        .list(&Query {
            q: " plexi & rock ".into(),
            sort: "top-week".into(),
            style: "crunch".into(),
            page: 1,
            per_page: 0,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let rig = &page.items[0];
    assert_eq!(
        (rig.slug.as_str(), rig.author.login.as_str()),
        ("plexi-crunch", "marta")
    );
    assert_eq!(rig.dependencies.tone3000, 2);
    assert!(page.has_more());
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.target,
        "/api/v1/rigs?q=plexi%20%26%20rock&sort=top-week&style=crunch"
    );
    let agent = request.header("user-agent").unwrap();
    assert!(agent.starts_with("OpenRig/"), "{agent}");
    // Anonymous: no token sent
    assert!(request.header("authorization").is_none());
}

#[test]
fn reads_a_rig_detail() {
    let server = Server::start(|r| {
        assert_eq!(r.target, "/api/v1/rigs/plexi-crunch");
        let mut detail = summary("plexi-crunch", "Plexi Crunch", "marta");
        detail["view"] = serde_json::json!({
            "format": 1, "name": "Plexi Crunch", "chain": [],
            "scenes": [{"name": "Verse", "off": [3]}, {"name": "Solo", "off": []}],
            "dependencies": [
                {"type": "builtin", "name": "Amp", "kind": "nam"},
                {"type": "store", "name": "ZamComp", "package": "zam-plugins", "version": "4.3", "uri": "urn:zamaudio:ZamComp"},
                {"type": "tone3000", "tone_id": 41, "model_id": 987, "tone_title": "Plexi",
                 "model_name": "Crunch", "creator": "someone", "gear": "amp", "format": "nam",
                 "license": "CC BY 4.0", "url": "https://www.tone3000.com/tones/plexi-41"}
            ]
        });
        detail["versions"] = serde_json::json!([
            {"version": 2, "changelog": "Less gain", "sha256": "ab", "size": 1200,
             "app_version": "0.11.0", "created_at": 1793500000, "download_url": "x"},
            {"version": 1, "changelog": "", "sha256": "cd", "size": 1100,
             "app_version": "0.10.0", "created_at": 1793000000, "download_url": "y"}
        ]);
        json(200, detail)
    });
    let detail = server.client().rig("plexi-crunch").unwrap();
    assert_eq!(detail.summary.name, "Plexi Crunch");
    assert_eq!(detail.view.scenes.len(), 2);
    let labels: Vec<String> = detail.view.dependencies.iter().map(|d| d.label()).collect();
    assert_eq!(labels, ["Amp", "ZamComp", "Plexi · Crunch"]);
    let uris: Vec<Option<String>> = detail
        .view
        .dependencies
        .iter()
        .map(|d| d.plugin_uri())
        .collect();
    assert_eq!(
        uris,
        [
            Some("urn:pedalboard:nam".to_string()),
            Some(COMP.to_string()),
            None
        ]
    );
    assert_eq!(detail.versions[0].changelog, "Less gain");
    // A slug that is not one is not sent at all
    assert!(matches!(
        server.client().rig("../me/token"),
        Err(ApiError::NotFound(_))
    ));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn maps_errors() {
    let server = Server::start(|r| match r.target.as_str() {
        "/api/v1/rigs/gone" => error(404, "not_found", "No such rig."),
        "/api/v1/rigs/busy" => error(429, "rate_limited", "Too many requests."),
        "/api/v1/rigs/broken" => (502, "text/html", b"<html>".to_vec()),
        "/api/v1/me" => error(401, "unauthorized", "Unknown token."),
        _ => (413, "text/plain", b"Payload Too Large".to_vec()),
    });
    let client = server.client();
    assert!(matches!(client.rig("gone"), Err(ApiError::NotFound(m)) if m == "No such rig."));
    assert!(matches!(client.rig("busy"), Err(ApiError::RateLimited(_))));
    assert!(matches!(client.rig("broken"), Err(ApiError::Server(m)) if m.contains("502")));
    // Calls that need an account are not made without one
    assert!(matches!(client.me(), Err(ApiError::Unauthorized)));
    assert_eq!(server.requests().len(), 3);
    let linked = server.client().with_token("orc_x");
    assert!(matches!(linked.me(), Err(ApiError::Unauthorized)));
    let huge = linked.publish(b"{}", "").unwrap_err();
    assert_eq!(huge.code(), "too_large");

    // Nothing listening: offline, not a crash
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let offline = Client::new(&base).list(&Query::default()).unwrap_err();
    assert!(matches!(offline, ApiError::Offline(_)), "{offline:?}");
    assert!(offline.to_string().contains("not reachable"));
}

#[test]
fn only_https_but_for_a_site_set_by_the_owner() {
    // An https site never falls back to plain HTTP
    let client = Client::new("https://openrig.eu/");
    assert_eq!(client.base(), "https://openrig.eu");
    assert_eq!(client.url("/rigs"), "https://openrig.eu/api/v1/rigs");
}

/// Scripted answers to the token polls, then what was waited
fn run_link(
    polls: Vec<Answer>,
    cancel_after: usize,
) -> (LinkEvent, Vec<u64>, Vec<String>, PathBuf) {
    let polls = Mutex::new(polls.into_iter());
    let server = Server::start(move |r| match r.target.as_str() {
        "/api/v1/device/code" => {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(body["device_name"], "OpenRig (test)");
            assert_eq!(body["app_version"], env!("CARGO_PKG_VERSION"));
            json(
                200,
                serde_json::json!({
                    "device_code": "SECRET-DEVICE-CODE",
                    "user_code": "BCDF-4KQ7",
                    "verification_uri": "https://openrig.eu/link",
                    "verification_uri_complete": "https://openrig.eu/link?code=BCDF4KQ7",
                    "expires_in": 600,
                    "interval": 5
                }),
            )
        }
        "/api/v1/device/token" => {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(body["device_code"], "SECRET-DEVICE-CODE");
            polls
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| error(400, "authorization_pending", "Waiting for the code."))
        }
        other => panic!("unexpected {other}"),
    });
    let dir = temp("link");
    let waits = Mutex::new(Vec::new());
    let wait = |d: Duration| {
        let mut waits = waits.lock().unwrap();
        waits.push(d.as_secs());
        waits.len() <= cancel_after
    };
    let events = Mutex::new(Vec::new());
    let on_event = |event: LinkEvent| {
        if let LinkEvent::Code(code) = &event {
            events
                .lock()
                .unwrap()
                .push(format!("{} {}", code.user_code, code.qr_url()));
            // The secret never shows in logs
            assert!(!format!("{code:?}").contains("SECRET"));
        }
    };
    let last = link(&server.client(), &dir, "OpenRig (test)", &wait, &on_event);
    let waits = waits.into_inner().unwrap();
    (last, waits, events.into_inner().unwrap(), dir)
}

#[test]
fn links_after_pending_and_slow_down() {
    let granted = json(
        200,
        serde_json::json!({"access_token": "orc_TOKEN", "token_type": "Bearer", "user": {"login": "marta"}}),
    );
    let (last, waits, events, dir) = run_link(
        vec![
            error(400, "authorization_pending", "Waiting."),
            error(400, "slow_down", "Poll less often."),
            error(429, "rate_limited", "Too many requests."),
            granted,
        ],
        usize::MAX,
    );
    assert_eq!(events, ["BCDF-4KQ7 https://openrig.eu/link?code=BCDF4KQ7"]);
    // 5 s as asked, 5 more after slow_down; a rate limit is retried
    assert_eq!(waits, [5, 5, 10, 10]);
    let LinkEvent::Linked(account) = last else {
        panic!("not linked")
    };
    assert_eq!(account.login, "marta");
    assert!(!format!("{account:?}").contains("orc_TOKEN"));
    // Saved for the next start, for this site only, owner-only
    let base = account.base_url.clone();
    let saved = Account::load(&dir, &base).unwrap();
    assert_eq!(saved.login, "marta");
    assert!(Account::load(&dir, "https://openrig.eu").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(Account::path(&dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn link_ends_on_refusal_expiry_and_cancel() {
    let cases = [
        ("access_denied", "refused"),
        ("expired_token", "expired"),
        ("invalid_grant", "no longer valid"),
    ];
    for (code, expected) in cases {
        let (last, _, _, dir) = run_link(vec![error(400, code, "x")], usize::MAX);
        match last {
            LinkEvent::Failed(message) => assert!(message.contains(expected), "{message}"),
            _ => panic!("{code} must end the link"),
        }
        assert!(!Account::path(&dir).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    // The code's lifetime runs out while the user does nothing
    let (last, waits, _, dir) = run_link(Vec::new(), usize::MAX);
    assert!(matches!(last, LinkEvent::Failed(m) if m.contains("expired")));
    assert_eq!(waits.iter().sum::<u64>(), 605);
    std::fs::remove_dir_all(dir).unwrap();
    // Cancelled by the user during the second wait
    let (last, waits, _, dir) = run_link(Vec::new(), 1);
    assert!(matches!(last, LinkEvent::Cancelled));
    assert_eq!(waits.len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn favourites_need_the_token() {
    let server = Server::start(|r| {
        assert_eq!(r.header("authorization"), Some("Bearer orc_T"));
        match (r.method.as_str(), r.target.as_str()) {
            ("GET", "/api/v1/me") => json(
                200,
                serde_json::json!({"login": "marta", "name": "Marta", "url": "u"}),
            ),
            ("GET", "/api/v1/me/favourites") => json(
                200,
                serde_json::json!({"items": [summary("a", "A", "x")], "total": 1, "page": 1, "per_page": 100}),
            ),
            ("PUT", "/api/v1/rigs/a/favourite") => {
                json(200, serde_json::json!({"on": true, "count": 7}))
            }
            ("DELETE", "/api/v1/rigs/a/favourite") => {
                json(200, serde_json::json!({"on": false, "count": 6}))
            }
            other => panic!("unexpected {other:?}"),
        }
    });
    let client = server.client().with_token("orc_T");
    assert_eq!(client.me().unwrap().login, "marta");
    let favourites = client.favourites().unwrap();
    assert_eq!(favourites.items[0].slug, "a");
    assert!(!favourites.has_more());
    let on = client.set_favourite("a", true).unwrap();
    assert!(on.on && on.count == 7);
    let off = client.set_favourite("a", false).unwrap();
    assert!(!off.on && off.count == 6);
}

#[test]
fn publishes_rigs_and_versions() {
    let server = Server::start(|r| {
        assert_eq!(r.header("authorization"), Some("Bearer orc_T"));
        assert_eq!(r.header("content-type"), Some(MEDIA_TYPE));
        match (r.method.as_str(), r.target.as_str()) {
            ("POST", "/api/v1/rigs?style=metal") => {
                assert_eq!(r.body, b"RIG FILE");
                json(201, summary("lead", "Lead", "marta"))
            }
            ("POST", "/api/v1/rigs") => error(
                422,
                "local_dependencies",
                "This rig uses files only its author has: My IR.wav",
            ),
            ("POST", "/api/v1/rigs?style=rock") => error(
                429,
                "rate_limited",
                "You have published a lot today; try again tomorrow.",
            ),
            ("POST", "/api/v1/rigs/lead/versions?changelog=Less%20gain%2C%20more%20mids") => {
                json(201, summary("lead", "Lead", "marta"))
            }
            ("POST", "/api/v1/rigs/theirs/versions") => {
                error(403, "forbidden", "Only the author can update this rig.")
            }
            other => panic!("unexpected {other:?}"),
        }
    });
    let client = server.client().with_token("orc_T");
    let published = client.publish(b"RIG FILE", "metal").unwrap();
    assert_eq!(published.url, "https://openrig.eu/r/lead");
    let refused = client.publish(b"RIG FILE", "").unwrap_err();
    assert_eq!(refused.code(), "local_dependencies");
    assert!(refused.to_string().contains("My IR.wav"));
    let limited = client.publish(b"RIG FILE", "rock").unwrap_err();
    assert!(matches!(limited, ApiError::RateLimited(m) if m.contains("tomorrow")));
    let version = client
        .new_version("lead", b"RIG FILE", " Less gain, more mids ")
        .unwrap();
    assert_eq!(version.slug, "lead");
    let forbidden = client.new_version("theirs", b"RIG FILE", "").unwrap_err();
    assert!(matches!(forbidden, ApiError::Forbidden(m) if m.contains("author")));
}

#[test]
fn finds_own_rig_by_name() {
    let server = Server::start(|r| {
        assert!(r
            .target
            .starts_with("/api/v1/rigs?q=Lead%20Solo&sort=new&per_page=100"));
        json(
            200,
            serde_json::json!({"items": [
                summary("lead-solo", "Lead Solo", "someone-else"),
                summary("lead-solo-live", "Lead Solo live", "marta"),
                summary("lead-solo-2", "lead  solo", "Marta"),
            ], "total": 3, "page": 1, "per_page": 100}),
        )
    });
    let client = server.client();
    let own = client.own_rig("marta", " Lead Solo ").unwrap().unwrap();
    assert_eq!(own.slug, "lead-solo-2");
    assert!(client.own_rig("nobody", "Lead Solo").unwrap().is_none());
    assert!(client.own_rig("marta", "  ").unwrap().is_none());
}

#[test]
fn unlinks_even_when_the_token_is_already_gone() {
    let revoked = Arc::new(AtomicBool::new(false));
    let seen = revoked.clone();
    let server = Server::start(move |r| {
        assert_eq!(
            (r.method.as_str(), r.target.as_str()),
            ("DELETE", "/api/v1/me/token")
        );
        if seen.swap(true, Ordering::Relaxed) {
            error(401, "unauthorized", "Unknown token.")
        } else {
            (204, "text/plain", Vec::new())
        }
    });
    for _ in 0..2 {
        let dir = temp("unlink");
        let account = Account::new(&server.base, "marta", "orc_T");
        account.save(&dir).unwrap();
        assert!(Account::load(&dir, &server.base).is_some());
        unlink(&dir, &account).unwrap();
        assert!(!Account::path(&dir).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    assert!(revoked.load(Ordering::Relaxed));
    assert_eq!(server.requests().len(), 2);
}

const COMP: &str = "urn:zamaudio:ZamComp";

fn catalog() -> Vec<CatalogPlugin> {
    vec![CatalogPlugin {
        uri: COMP.into(),
        package: "zam-plugins".into(),
        version: "4.3".into(),
        installed_version: Some("4.3".into()),
        plugin_id: "zam-comp".into(),
    }]
}

/// The file another pedalboard published: one store plugin
fn published_rig() -> Vec<u8> {
    let mut preset = Preset::new("Squash".into());
    preset.chain = Some(PresetChain {
        nodes: vec![crate::audio::ChainNode::Plugin {
            instance_id: 1,
            uri: COMP.into(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }],
        ..PresetChain::default()
    });
    let catalog = catalog();
    let none = |_: &str| None;
    let exported = export::export(
        &preset,
        &export::Context {
            models_dir: Path::new("/nowhere"),
            catalog: &catalog,
            plugin_name: &|_| Some("ZamComp".into()),
            plugin_class: &none,
            author: "marta".into(),
            app_version: "0.11.0",
            created: String::new(),
        },
        export::Options::default(),
    )
    .unwrap();
    assert!(publish_blockers(&exported).is_empty());
    exported.rig.to_vec_pretty().unwrap()
}

#[derive(Default)]
struct FakeInstaller {
    plugins: Vec<String>,
}

impl import::Installer for FakeInstaller {
    fn install_plugin(&mut self, plugin_id: &str) -> Result<()> {
        self.plugins.push(plugin_id.to_string());
        Ok(())
    }

    fn download_model(&mut self, _model: &crate::rigs::format::Tone3000Model) -> Result<PathBuf> {
        anyhow::bail!("no models here")
    }
}

#[test]
fn downloads_then_imports_like_a_file() {
    let file = published_rig();
    let server = Server::start(move |r| match r.target.as_str() {
        "/api/v1/rigs/squash/download?version=1" => (200, MEDIA_TYPE, file.clone()),
        "/api/v1/rigs/junk/download" => (200, MEDIA_TYPE, b"{\"hello\": 1}".to_vec()),
        other => panic!("unexpected {other}"),
    });
    let client = server.client();
    let rig = client.download("squash", Some(1)).unwrap();
    assert_eq!(rig.meta.name, "Squash");
    // The same path as a rig from a USB drive: plan, then install
    let catalog = catalog();
    let installed = |_: &str| false;
    let models = temp("models");
    let plan = import::plan(
        rig,
        &import::Context {
            models_dir: &models,
            catalog: &catalog,
            installed: &installed,
            tone3000: false,
        },
    )
    .unwrap();
    assert_eq!(plan.to_install(), 1);
    let mut installer = FakeInstaller::default();
    let outcome = import::install(&plan, &mut installer).unwrap();
    assert_eq!(installer.plugins, ["zam-comp"]);
    assert_eq!(outcome.preset.name, "Squash");
    assert!(outcome.plugins_installed);
    // Something that is not a rig is refused before any plan
    let junk = client.download("junk", None).unwrap_err();
    assert!(matches!(junk, ApiError::Server(m) if m.contains("broken rig")));
    std::fs::remove_dir_all(models).unwrap();
}

#[test]
fn local_items_block_publishing() {
    let mut exported = export::Exported {
        rig: Rig::from_slice(&published_rig()).unwrap(),
        report: Default::default(),
        dropped: Vec::new(),
    };
    exported.rig.dependencies.plugins[0].source = Source::Local {
        what: "plugin".into(),
        name: "My Fuzz".into(),
    };
    exported.report = exported.rig.validate();
    assert_eq!(
        publish_blockers(&exported),
        ["My Fuzz (only on this pedalboard)"]
    );
}

#[test]
fn reads_the_base_url() {
    assert_eq!(
        base_url_from(Some(" http://127.0.0.1:8098/ ".into())),
        "http://127.0.0.1:8098"
    );
    assert_eq!(base_url_from(Some(" ".into())), DEFAULT_URL);
    assert_eq!(base_url_from(None), DEFAULT_URL);
}
