//! `relevo web` (docs/web.md): the timeline in the person's own browser. A small HTTP server on
//! 127.0.0.1 serves the page, compiled in, and passes the page's requests for the two read-only
//! timeline routes on to the team's server, with the token and the pinned certificate of the
//! config. The browser never sees the token.
//!
//! Only the tab opened with the secret may ask: the secret goes in the address after `#`, so it
//! is never sent in a request line, and the page sends it back in `Relevo-Web-Key`. It is a
//! one-time key: the first answer that goes well trades it for a session key of this server's
//! (`SESSION_HEADER`), which the page holds in the tab and this process holds in memory, and the
//! address's key is refused from then on — the copy the browser kept of the address (history, a
//! synced session) is worth nothing. The browser
//! is not given that address in its command line, which every local user can read: it opens a
//! small file of this user's that sends it on (`Launcher`). Every request
//! must name this server in `Host` (against DNS rebinding) and, if it has an `Origin`, come from
//! this server's page. The page and its assets hold no data, and are served without the key: a
//! browser cannot send a header when it opens an address.

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use crate::client::{Client, Error};
use crate::config;

/// The header the page sends its key in.
pub const KEY_HEADER: &str = "relevo-web-key";
/// The header the answer to the first request carries the session key in, and the only way to
/// get it: it never goes in the address.
pub const SESSION_HEADER: &str = "relevo-web-session";
/// The most the team's server may answer with: a long thread of 64 KiB notes fits.
const MAX_ANSWER: u64 = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

const INDEX: &str = include_str!("web/index.html");
const SCRIPT: &str = include_str!("web/app.js");
const STYLE: &str = include_str!("web/app.css");
const MARK: &str = include_str!("../assets/brand/relevo-mark.svg");
const FAVICON: &str = include_str!("../assets/brand/relevo-mark-small.svg");

/// What every answer carries: nothing is kept, nothing runs or loads but this server's own
/// files, nothing is framed, sniffed, told where it came from, or readable by another origin.
const SECURITY_HEADERS: [(&str, &str); 7] = [
    ("cache-control", "no-store"),
    (
        "content-security-policy",
        "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    ("cross-origin-resource-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
];

/// The local server's state: the client of the team's server, the port it listens on, the key of
/// the address while it has not been traded, the session keys handed out, and the launcher file
/// while the page has not used the key yet.
pub struct Web {
    client: Client,
    port: u16,
    /// The key of the address, until the page trades it for a session key; then nothing.
    key: Mutex<Option<String>>,
    /// The session keys this server issued. They live here and in the tab, nowhere else, and die
    /// with this process.
    sessions: Mutex<HashSet<String>>,
    launcher: Mutex<Option<Launcher>>,
}

impl Web {
    pub fn new(client: Client, port: u16, key: String) -> Arc<Web> {
        Arc::new(Web {
            client,
            port,
            key: Mutex::new(Some(key)),
            sessions: Mutex::new(HashSet::new()),
            launcher: Mutex::new(None),
        })
    }

    /// Keeps `launcher` until the page first asks with the key, or this server stops.
    pub fn keep_launcher(&self, launcher: Launcher) {
        *self.launcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(launcher);
    }

    /// Deletes the launcher file, if it is still there.
    pub fn drop_launcher(&self) {
        self.launcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    /// Whether `given` is the key of the address, which works until it is traded for a session
    /// key.
    fn is_address_key(&self, given: &[u8]) -> bool {
        self.key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .is_some_and(|key| same(given, key.as_bytes()))
    }

    /// Whether `given` is a session key this server handed out.
    fn knows_session(&self, given: &[u8]) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|session| same(given, session.as_bytes()))
    }

    /// Takes `session` as one of this server's keys and retires the key of the address: what the
    /// browser kept of that address — in the history, in a synced session — asks for nothing.
    fn accept_session(&self, session: &str) {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string());
    }
}

/// The file the browser is told to open: a page that sends it on to the address with the key.
/// The address itself never goes in a command line (`ps` shows every user's, and a browser
/// started for it keeps it there while it runs); the file's path does, and only this user can
/// read the file: it is mode 0600, in a directory of its own of mode 0700. Deleted when dropped.
pub struct Launcher {
    dir: PathBuf,
    file: PathBuf,
}

impl Launcher {
    /// Writes the launcher of `address` in a new directory under `base`.
    pub fn new(base: &FsPath, address: &str) -> std::io::Result<Launcher> {
        // The address is ours: digits and base64url. Nothing in it needs escaping in HTML.
        debug_assert!(
            address
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:/.#=".contains(&b))
        );
        let dir = base.join(crate::db::random_id("relevo-web-"));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let launcher = Launcher {
            file: dir.join("open.html"),
            dir,
        };
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&launcher.file)?;
        write!(
            f,
            "<!doctype html>\n<meta charset=\"utf-8\">\n<meta name=\"referrer\" content=\"no-referrer\">\n\
             <meta http-equiv=\"refresh\" content=\"0;url={address}\">\n<title>Relevo</title>\n\
             <p><a href=\"{address}\">Open Relevo's timeline</a></p>\n"
        )?;
        Ok(launcher)
    }

    /// What the opener is given.
    pub fn path(&self) -> &FsPath {
        &self.file
    }
}

impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Where launchers go: `$XDG_RUNTIME_DIR` (this user's only), else the temporary directory.
fn launcher_base() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute() && d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// A new key: 32 random bytes, base64url.
pub fn new_key() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn router(web: Arc<Web>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE) }),
        )
        .route("/mark.svg", get(|| async { asset("image/svg+xml", MARK) }))
        .route(
            "/favicon.svg",
            get(|| async { asset("image/svg+xml", FAVICON) }),
        )
        .route("/api/timeline", get(timeline))
        .route("/api/timeline/threads/{id}", get(thread))
        .fallback(|| async { refused(StatusCode::NOT_FOUND, "NOT_FOUND", "no such route") })
        .method_not_allowed_fallback(|| async {
            refused(StatusCode::NOT_FOUND, "NOT_FOUND", "no such route")
        })
        .layer(middleware::from_fn_with_state(web.clone(), guard))
        .with_state(web)
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

fn refused(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// Two byte strings equal, in a time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Checks `Host`, `Origin` and, for the API, the key, and refuses every method but GET; then adds the security headers to the
/// answer, refusal or not.
async fn guard(State(web): State<Arc<Web>>, req: Request, next: Next) -> Response {
    let checked = check(&web, &req);
    if checked.is_ok() && req.uri().path().starts_with("/api/") {
        // The page has the key: the launcher is not needed any more.
        web.drop_launcher();
    }
    let mut res = match checked {
        // GET only: axum would route HEAD to a GET handler too, which would ask the team's
        // server. Anything else is no route at all.
        Ok(_) if req.method() != Method::GET => {
            refused(StatusCode::NOT_FOUND, "NOT_FOUND", "no such route")
        }
        Ok(session) => {
            let mut res = next.run(req).await;
            // The key of the address is only spent when the page really got its session key: a
            // first request that went wrong (the team's server down) is tried again with it.
            if let Some(session) = session
                && res.status().is_success()
            {
                web.accept_session(&session);
                res.headers_mut().insert(
                    HeaderName::from_static(SESSION_HEADER),
                    HeaderValue::from_str(&session).expect("base64url is a header value"),
                );
            }
            res
        }
        Err(why) => refused(StatusCode::FORBIDDEN, "FORBIDDEN", why),
    };
    let headers = res.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    res
}

/// Why the request is refused, if it is. `Ok(Some(session))` is the page asking with the key of
/// the address: `session` is the key to give it, if the answer goes well.
fn check(web: &Web, req: &Request) -> Result<Option<String>, &'static str> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let ours = [
        format!("127.0.0.1:{}", web.port),
        format!("localhost:{}", web.port),
    ];
    if !ours.iter().any(|o| o == host) {
        return Err("this server answers to 127.0.0.1 and localhost only");
    }
    let origin_ok = match req.headers().get(header::ORIGIN) {
        Some(origin) if origin.as_bytes() != format!("http://{host}").as_bytes() => {
            return Err("requests from other pages are refused");
        }
        Some(_) => true,
        None => false,
    };
    if req.uri().path().starts_with("/api/") {
        // The browser says where the request came from. Only `cross-site` is refused: a request
        // with no such header (an old browser, a tool of this user's) is refused for nothing.
        // When Origin already matched above, skip this check: Chrome on macOS sends `cross-site`
        // for same-origin requests to 127.0.0.1, and Origin is the more reliable signal. The page
        // and its files are not checked: they hold no data, and loading the page from the
        // launcher file (`file://`) is `cross-site` to the browser.
        if !origin_ok
            && req
                .headers()
                .get("sec-fetch-site")
                .is_some_and(|site| site.as_bytes() == b"cross-site")
        {
            return Err("requests from another site are refused");
        }
        let given = req
            .headers()
            .get(KEY_HEADER)
            .map(HeaderValue::as_bytes)
            .unwrap_or_default();
        if web.is_address_key(given) {
            return Ok(Some(new_key()));
        }
        if !web.knows_session(given) {
            return Err("missing or wrong key: open the address relevo web printed");
        }
    }
    Ok(None)
}

/// `GET /api/timeline`, its query string passed on as it came.
async fn timeline(State(web): State<Arc<Web>>, uri: Uri) -> Response {
    let path = match uri.query() {
        Some(query) => format!("/api/timeline?{query}"),
        None => "/api/timeline".to_string(),
    };
    pass_on(&web, &path).await
}

/// `GET /api/timeline/threads/:id`, for an id that can be one.
async fn thread(State(web): State<Arc<Web>>, id: Result<Path<String>, PathRejection>) -> Response {
    match id {
        Ok(Path(id))
            if (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            pass_on(&web, &format!("/api/timeline/threads/{id}")).await
        }
        _ => refused(StatusCode::NOT_FOUND, "NOT_FOUND", "no such thread"),
    }
}

/// Asks the team's server, and answers with what it said. No answer from it at all is a 502
/// with `UNREACHABLE`, which the page shows as such.
async fn pass_on(web: &Web, path: &str) -> Response {
    match web.client.fetch(path, MAX_ANSWER, TIMEOUT).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(Error::Api { status, body, .. }) if body["error"].is_object() => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, axum::Json(body)).into_response()
        }
        Err(e) => {
            tracing::debug!(error = %e, "the team's server did not answer");
            refused(StatusCode::BAD_GATEWAY, "UNREACHABLE", &e.to_string())
        }
    }
}

/// `relevo web`: serves the page on 127.0.0.1 (`port`, or a free one) until Ctrl-C, and opens
/// it in the browser unless `no_open`.
pub async fn run(port: Option<u16>, no_open: bool) -> Result<()> {
    let config = config::load(config::env)?;
    if let Some(warning) = config::plain_http_warning(&config.url) {
        eprintln!("{warning}");
    }
    let client = Client::new(&config, Some(String::new()), "/".into())?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .with_context(|| match port {
            Some(port) => format!("listening on 127.0.0.1:{port}"),
            None => "listening on 127.0.0.1".to_string(),
        })?;
    let port = listener.local_addr()?.port();
    let key = new_key();
    let address = format!("http://127.0.0.1:{port}/#k={key}");
    let web = Web::new(client, port, key);
    let mut opened = false;
    if !no_open {
        opened = match Launcher::new(&launcher_base(), &address) {
            Ok(launcher) => {
                let opened = open_browser(launcher.path()).await;
                if opened {
                    web.keep_launcher(launcher);
                }
                opened
            }
            Err(e) => {
                tracing::debug!(error = %e, "could not write the launcher");
                false
            }
        };
    }
    // The address holds the key of this run. It goes to stdout in full only where it cannot be
    // kept: a terminal, or the line the person has to copy because no browser opened. With the
    // browser open and stdout going to a file, or to a service unit, the key stays out of it.
    let quiet = opened && !stdout_is_terminal();
    println!(
        "Relevo's timeline for {}:\n  {}",
        config.url,
        if quiet {
            format!("http://127.0.0.1:{port}/")
        } else {
            address
        }
    );
    println!("It only reads: nothing is delivered, handled or claimed. Ctrl-C stops it.");
    if quiet {
        println!(
            "Opened in your browser. The key of this run is not printed: stdout is no terminal."
        );
    } else if !opened {
        println!("Open the address above in your browser.");
    }
    tokio::select! {
        _ = crate::serve::serve(listener, router(web.clone()), None, crate::serve::Limits::default()) => {}
        _ = stop_signal() => {}
    }
    web.drop_launcher();
    Ok(())
}

/// Whether stdout is the terminal of the person reading this now: the one place the key of the
/// address may be printed.
fn stdout_is_terminal() -> bool {
    // SAFETY: `isatty` only asks about this process's own standard output.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Asks the system to open `file` in the browser: `open` on macOS, `xdg-open` elsewhere.
/// Whether it could.
async fn open_browser(file: &FsPath) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let started = tokio::process::Command::new(opener)
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match started {
        Ok(mut child) => match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(Ok(status)) => status.success(),
            // Still starting the browser: it most likely will.
            Err(_) => {
                tokio::spawn(async move { child.wait().await });
                true
            }
            Ok(Err(_)) => false,
        },
        Err(_) => false,
    }
}

/// Resolves when SIGINT, SIGTERM or SIGHUP comes (a closed terminal sends SIGHUP), so the
/// launcher file holding the key is removed however `relevo web` is stopped.
#[cfg(unix)]
async fn stop_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = hup.recv() => {}
    }
}

/// Resolves on Ctrl-C.
#[cfg(not(unix))]
async fn stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_32_random_bytes() {
        let (a, b) = (new_key(), new_key());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(same(a.as_bytes(), a.as_bytes()));
        assert!(!same(a.as_bytes(), b.as_bytes()));
        assert!(!same(b"abc", b"ab"));
    }

    /// All text is readable: at least 4.5:1 (WCAG AA) on every background it sits on, in the dark
    /// theme and in the light one, the amber pill included.
    #[test]
    fn all_text_has_enough_contrast_in_both_themes() {
        fn colours(name: &str) -> Vec<[f64; 3]> {
            STYLE
                .match_indices(&format!("{name}: #"))
                .map(|(at, m)| {
                    let hex = &STYLE[at + m.len()..at + m.len() + 6];
                    let c = |i: usize| f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
                    [c(0), c(2), c(4)]
                })
                .collect()
        }
        fn luminance(rgb: [f64; 3]) -> f64 {
            let f = |c: f64| {
                let c = c / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(rgb[0]) + 0.7152 * f(rgb[1]) + 0.0722 * f(rgb[2])
        }
        for (text, background) in [
            ("--text", "--bg"),
            ("--text", "--soft"),
            ("--body", "--bg"),
            ("--faint", "--bg"),
            ("--faint", "--soft"),
            ("--need", "--bg"),
            ("--need", "--soft"),
            ("--mention", "--bg"),
            ("--pill-text", "--pill"),
        ] {
            let (fg, bg) = (colours(text), colours(background));
            assert_eq!(
                (fg.len(), bg.len()),
                (2, 2),
                "{text} and {background}, dark and light"
            );
            for theme in 0..2 {
                let (a, b) = (luminance(fg[theme]), luminance(bg[theme]));
                let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(
                    ratio >= 4.5,
                    "{text} on {background}, theme {theme}: {ratio:.2}"
                );
            }
        }
        // The initial in your seat's ring: each avatar colour mixed in sRGB with the text colour,
        // by --ghost-mix (one per theme), on the page and on the soft tint of a fresh row.
        let avatars = colours("--c");
        assert_eq!(avatars.len(), 8, "the eight avatar colours");
        let mixes: Vec<f64> = STYLE
            .match_indices("--ghost-mix: ")
            .map(|(at, m)| {
                let rest = &STYLE[at + m.len()..];
                rest[..rest.find('%').unwrap()].parse::<f64>().unwrap() / 100.0
            })
            .collect();
        assert_eq!(mixes.len(), 2, "--ghost-mix, dark and light");
        let (text, bg, soft) = (colours("--text"), colours("--bg"), colours("--soft"));
        for theme in 0..2 {
            for (i, c) in avatars.iter().enumerate() {
                let m = mixes[theme];
                let ink = [0, 1, 2].map(|k| c[k] * (1.0 - m) + text[theme][k] * m);
                for (name, under) in [("--bg", bg[theme]), ("--soft", soft[theme])] {
                    let (a, b) = (luminance(ink), luminance(under));
                    let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                    assert!(
                        ratio >= 4.5,
                        "the seat's initial for .c{i} on {name}, theme {theme}: {ratio:.2}"
                    );
                }
            }
        }
    }

    /// One word for each thing (docs/web.md): what is pending is *Needs you* (the filter and the
    /// group), the pill on a pending thread says what to do, *Reply needed*, the person is *You*,
    /// and the group under *Needs you* is *The rest*. The words that were there before are
    /// gone from the page, and the tests would not notice them coming back.
    #[test]
    fn the_page_says_the_one_word_for_each_thing() {
        for gone in ["Waiting for you", "Your agent", "Earlier"] {
            assert!(!SCRIPT.contains(gone), "the page says {gone}");
        }
        assert!(
            SCRIPT.contains("el('span', 'status', 'Reply needed')"),
            "the pill"
        );
        assert!(SCRIPT.contains("['needs', 'Needs you',"), "the group");
        assert!(
            SCRIPT.contains("['rest', 'The rest',"),
            "the group under Needs you"
        );
        assert!(
            SCRIPT.contains("? 'You' :"),
            "the person, in the row and in the notes"
        );
    }

    /// Threads no longer close (docs/web.md): the filters are All, Needs you, Answered and the
    /// teams, no row is marked or greyed out as closed, and nothing reads `closed_by` or
    /// `counts.closed`.
    #[test]
    fn nothing_on_the_page_is_closed() {
        assert!(
            SCRIPT.contains(
                "const SHOWS = [\n    ['all', 'All', 'threads'],\n    ['waiting', 'Needs you', \
                 'waiting'],\n    ['answered', 'Answered', 'answered'],\n  ];"
            ),
            "the filters"
        );
        for (name, file) in [
            ("index.html", INDEX),
            ("app.js", SCRIPT),
            ("app.css", STYLE),
        ] {
            for gone in [
                "'Closed'",
                "'closed'",
                "closed_by",
                "closed-label",
                ".closed",
                "chips",
            ] {
                assert!(!file.contains(gone), "{name} has {gone}");
            }
        }
    }

    /// A note's header never wraps (docs/web.md): the label, then the name, are cut with an
    /// ellipsis, and the time keeps its place.
    #[test]
    fn a_notes_header_stays_one_line() {
        let who = rule(".who {");
        assert!(who.contains("flex-wrap: nowrap"), "{who}");
        let parts = rule(".who b, .who span {");
        for part in [
            "text-overflow: ellipsis",
            "white-space: nowrap",
            "min-width: 0",
        ] {
            assert!(parts.contains(part), "{part}: {parts}");
        }
        // The label gives way before the name; the time never does.
        assert!(rule("\n.who span {").contains("flex: 0 10 auto"));
        assert!(rule("\n.who b {").contains("flex: 0 1 auto"));
        assert!(rule(".who time {").contains("flex-shrink: 0"));
        // Every note's header is made by one function, with the whole name and label on hover.
        assert_eq!(SCRIPT.matches("el('div', 'who')").count(), 1);
        assert!(SCRIPT.contains("via.title = via.textContent"));
    }

    /// A row's name for screen readers is its first note and its status, not the whole row.
    #[test]
    fn a_row_is_named_by_its_first_note_and_status() {
        assert!(SCRIPT.contains("a.setAttribute('aria-labelledby', names.join(' '))"));
        for part in ["-from'", "-text'", "-status'"] {
            assert!(SCRIPT.contains(part), "{part}");
        }
        // Open, the first note shows its time and context as the replies do, in their colour.
        assert!(SCRIPT.contains("open ? noteTime : short"));
        assert!(SCRIPT.contains("open && first.context ? ' in ' + first.context : ''"));
        assert!(rule(".card.open .text {").contains("color: var(--text)"));
    }

    /// A note the server marks as its own (`system`, the release notes) shows the Relevo mark, the
    /// page's own image, as its avatar: by the flag, never by the author's handle.
    #[test]
    fn relevos_notes_show_the_mark() {
        assert!(SCRIPT.contains("function avatar(handle, system) {\n    if (system) {"));
        assert!(!SCRIPT.contains("=== 'relevo'"));
        assert!(SCRIPT.contains("avatar(t.from, first.system)"));
        assert_eq!(SCRIPT.matches("avatar(n.author, n.system)").count(), 2);
        assert!(SCRIPT.contains("mark.src = '/mark.svg';"));
        assert!(rule(".av.system {").contains("background: none"));
    }

    /// The body of `selector`'s rule of the stylesheet, as in `.banner {`.
    fn rule(selector: &str) -> String {
        let head = STYLE
            .find(selector)
            .unwrap_or_else(|| panic!("the stylesheet has no {selector}"));
        let body = &STYLE[head + selector.len()..];
        body[..body.find('}').unwrap()].trim().to_string()
    }

    /// The line of a failed refresh never moves the page and is always a live region: its space
    /// is reserved from the start (a `hidden` box would push every row down a line, and back up,
    /// every 30 seconds) and it is never `display:none`, which screen readers do not announce.
    /// Nor is it the amber of the status pill: it asks for nothing.
    #[test]
    fn the_banner_never_moves_the_page_and_is_always_read() {
        assert!(
            INDEX.contains(r#"<div class="banner" id="banner" role="status"></div>"#),
            "the banner is rendered from the start, with nothing hidden"
        );
        assert!(!SCRIPT.contains("$('banner').hidden"), "it is never hidden");
        let banner = rule(".banner {");
        assert!(banner.contains("min-height:"), "its height: {banner}");
        assert!(!banner.contains("display: none"), "{banner}");
        assert!(banner.contains("color: var(--text)"), "{banner}");
    }

    /// The page and its assets are self-contained: no inline script, nothing remote, and note
    /// text never reaches innerHTML (the script does not use it at all).
    #[test]
    fn the_page_loads_nothing_remote_and_never_sets_html() {
        for (name, file) in [
            ("index.html", INDEX),
            ("app.js", SCRIPT),
            ("app.css", STYLE),
        ] {
            for bad in [
                "innerHTML",
                "outerHTML",
                "insertAdjacentHTML",
                "document.write",
                "eval(",
                "new Function",
                "http://",
                "https://",
                "@import",
                "url(",
            ] {
                assert!(!file.contains(bad), "{name} has {bad}");
            }
        }
        // The only scripts are files of this server's.
        assert_eq!(INDEX.matches("<script").count(), 1);
        assert!(INDEX.contains(r#"<script src="/app.js" defer></script>"#));
        // No inline event handler (` onclick=` and the like).
        let inline_handler = INDEX.match_indices(" on").any(|(at, _)| {
            let rest = &INDEX[at + 3..];
            let name = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            name > 0 && rest[name..].starts_with('=')
        });
        assert!(!inline_handler);
    }
}
