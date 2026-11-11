//! `relevo web` (docs/web.md): the timeline in the person's own browser. A small HTTP server on
//! 127.0.0.1 serves the page, compiled in, and passes the page's requests for the two read-only
//! timeline routes on to the team's server, with the token and the pinned certificate of the
//! config. The browser never sees the token.
//!
//! Only the tab opened with the secret may ask: the secret goes in the address after `#`, so it
//! is never sent in a request line, and the page sends it back in `Relevo-Web-Key`. The browser
//! is not given that address in its command line, which every local user can read: it opens a
//! small file of this user's that sends it on (`Launcher`). Every request
//! must name this server in `Host` (against DNS rebinding) and, if it has an `Origin`, come from
//! this server's page. The page and its assets hold no data, and are served without the key: a
//! browser cannot send a header when it opens an address.

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
/// The most the team's server may answer with: a long thread of 64 KiB notes fits.
const MAX_ANSWER: u64 = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

const INDEX: &str = include_str!("web/index.html");
const SCRIPT: &str = include_str!("web/app.js");
const STYLE: &str = include_str!("web/app.css");
const MARK: &str = include_str!("../assets/brand/relevo-mark.svg");
const FAVICON: &str = include_str!("../assets/brand/relevo-mark-small.svg");

/// What every answer carries: nothing is kept, nothing runs or loads but this server's own
/// files, nothing is framed, sniffed or told where it came from.
const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("cache-control", "no-store"),
    (
        "content-security-policy",
        "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
];

/// The local server's state: the client of the team's server, the port it listens on, the key
/// of this run, and the launcher file while the page has not used the key yet.
pub struct Web {
    client: Client,
    port: u16,
    key: String,
    launcher: Mutex<Option<Launcher>>,
}

impl Web {
    pub fn new(client: Client, port: u16, key: String) -> Arc<Web> {
        Arc::new(Web {
            client,
            port,
            key,
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
        Ok(()) if req.method() != Method::GET => {
            refused(StatusCode::NOT_FOUND, "NOT_FOUND", "no such route")
        }
        Ok(()) => next.run(req).await,
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

/// Why the request is refused, if it is.
fn check(web: &Web, req: &Request) -> Result<(), &'static str> {
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
    if let Some(origin) = req.headers().get(header::ORIGIN)
        && origin.as_bytes() != format!("http://{host}").as_bytes()
    {
        return Err("requests from other pages are refused");
    }
    if req.uri().path().starts_with("/api/") {
        let key = req
            .headers()
            .get(KEY_HEADER)
            .map(HeaderValue::as_bytes)
            .unwrap_or_default();
        if !same(key, web.key.as_bytes()) {
            return Err("missing or wrong key: open the address relevo web printed");
        }
    }
    Ok(())
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
    println!("Relevo's timeline for {}:\n  {address}", config.url);
    println!("It only reads: nothing is delivered, handled or claimed. Ctrl-C stops it.");
    if !no_open {
        let opened = match Launcher::new(&launcher_base(), &address) {
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
        if !opened {
            println!("Open the address above in your browser.");
        }
    }
    tokio::select! {
        _ = crate::serve::serve(listener, router(web.clone()), None, crate::serve::Limits::default()) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    web.drop_launcher();
    Ok(())
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
