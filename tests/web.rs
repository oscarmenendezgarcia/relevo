//! `relevo web`'s local server (docs/web.md): the key, `Host` and `Origin` checks, the security
//! headers, and the two read-only routes passed on to a real team server on loopback. Also runs
//! the note renderer's tests with node (`tests/web/render.test.js`), when node is installed.

use std::sync::Arc;

#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use relevo::client::Client;
use relevo::config::Config;
use relevo::web::{self, Web};
use relevo::{App, Clock, api, db, serve};
use serde_json::{Value, json};
use tower::ServiceExt;

const PORT: u16 = 47_123;
const KEY: &str = "test-key-0123456789abcdefghijklmnopqrstuvwx";

struct Setup {
    app: Arc<App>,
    web: Arc<Web>,
    /// The team server's address.
    url: String,
    /// Ana's and Bob's tokens.
    ana: String,
    bob: String,
    /// The key of this tab: the address's, traded for a session key (docs/web.md).
    key: String,
}

/// A team server on loopback with Ana and Bob, and a `relevo web` for Ana in front of it.
async fn setup() -> Setup {
    let app = App::new(db::open(":memory:").unwrap(), Clock::system(), 50);
    let now = db::now_unix();
    let ana = db::invite(&app.db(), "ana", "Ana", None, "laptop", now).unwrap();
    let bob = db::invite(&app.db(), "bob", "Bob", None, "laptop", now).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve::serve(
        listener,
        api::router(app.clone()),
        None,
        serve::Limits::default(),
    ));
    let url = format!("http://{addr}");
    let web = for_server(&url, &ana.secret);
    let key = trade(&web).await;
    Setup {
        app,
        web,
        url,
        ana: ana.secret,
        bob: bob.secret,
        key,
    }
}

/// Asks once with the key of the address and answers with the session key traded for it.
async fn trade(web: &Arc<Web>) -> String {
    let (status, headers, _) = ask(
        web,
        "GET",
        "/api/timeline?tab=board",
        &[("relevo-web-key", KEY)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    headers[web::SESSION_HEADER].to_str().unwrap().to_string()
}

fn for_server(url: &str, token: &str) -> Arc<Web> {
    let config = Config {
        url: url.to_string(),
        token: token.to_string(),
        cert: None,
        adapters: vec![],
    };
    let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
    Web::new(client, PORT, KEY.to_string())
}

/// One request to the local server, with these headers (the right `Host` unless given).
async fn ask(
    web: &Arc<Web>,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req = req.header("host", format!("127.0.0.1:{PORT}"));
    }
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let res = web::router(web.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

fn json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn assert_security_headers(headers: &HeaderMap, what: &str) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_else(|| panic!("{what}: no {name}"))
            .to_string()
    };
    assert_eq!(get("cache-control"), "no-store", "{what}");
    let csp = get("content-security-policy");
    assert!(csp.starts_with("default-src 'self'"), "{what}: {csp}");
    assert!(csp.contains("img-src 'self' data:"), "{what}: {csp}");
    assert!(!csp.contains("unsafe-inline"), "{what}: {csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{what}: {csp}");
    assert_eq!(get("x-content-type-options"), "nosniff", "{what}");
    assert_eq!(get("referrer-policy"), "no-referrer", "{what}");
    assert_eq!(get("x-frame-options"), "DENY", "{what}");
    assert_eq!(get("cross-origin-resource-policy"), "same-origin", "{what}");
    assert_eq!(get("cross-origin-opener-policy"), "same-origin", "{what}");
}

/// What a browser says about where a request came from: only `cross-site` is refused, and a
/// request that says nothing is refused for nothing.
#[tokio::test]
async fn a_request_from_another_site_is_refused() {
    let s = setup().await;
    let key = ("relevo-web-key", s.key.as_str());
    for path in ["/", "/app.js", "/api/timeline?tab=board"] {
        let (status, headers, body) = ask(
            &s.web,
            "GET",
            path,
            &[key, ("sec-fetch-site", "cross-site")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "cross-site {path}");
        assert_eq!(json(&body)["error"]["code"], "FORBIDDEN");
        assert_security_headers(&headers, "cross-site");
    }
    for site in ["none", "same-origin", "same-site", ""] {
        let (status, _, _) = ask(
            &s.web,
            "GET",
            "/api/timeline?tab=board",
            &[key, ("sec-fetch-site", site)],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{site}");
    }
    // Without the header at all, as a browser that does not send it.
    let (status, _, _) = ask(&s.web, "GET", "/api/timeline?tab=board", &[key]).await;
    assert_eq!(status, StatusCode::OK);
}

fn sessions(app: &App) -> i64 {
    app.db()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn the_page_and_its_assets_need_no_key_and_carry_the_security_headers() {
    let s = setup().await;
    for (path, kind) in [
        ("/", "text/html"),
        ("/app.js", "text/javascript"),
        ("/app.css", "text/css"),
        ("/favicon.svg", "image/svg+xml"),
        ("/mark.svg", "image/svg+xml"),
    ] {
        let (status, headers, body) = ask(&s.web, "GET", path, &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let content_type = headers["content-type"].to_str().unwrap();
        assert!(content_type.starts_with(kind), "{path}: {content_type}");
        assert!(!body.is_empty(), "{path}");
        assert_security_headers(&headers, path);
    }
    let (_, _, page) = ask(&s.web, "GET", "/", &[]).await;
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains(r#"<script src="/app.js" defer></script>"#));
    assert!(page.contains(r#"href="/favicon.svg""#));
    // The key is never in the page: it lives in the address's fragment only.
    assert!(!page.contains(KEY));
    // The page is one column with Everyone first, a tablist, and no side bar.
    assert!(page.contains(r#"role="tablist""#));
    assert!(page.find("Everyone").unwrap() < page.find("For you").unwrap());
    assert!(!page.contains("<aside"));
}

#[tokio::test]
async fn the_api_needs_the_key() {
    let s = setup().await;
    for headers in [
        vec![],
        vec![("relevo-web-key", "wrong")],
        vec![("relevo-web-key", "")],
        vec![("relevo-web-key", &KEY[..KEY.len() - 1])],
        // The key is a header, not a query parameter or a cookie.
        vec![("cookie", "k=test-key-0123456789abcdefghijklmnopqrstuvwx")],
    ] {
        let (status, h, body) = ask(&s.web, "GET", "/api/timeline?tab=board", &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
        assert_eq!(json(&body)["error"]["code"], "FORBIDDEN");
        assert_security_headers(&h, "403");
        let thread = "/api/timeline/threads/r_aaaaaaaaaaaa";
        let (status, _, _) = ask(&s.web, "GET", thread, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let uri = format!("/api/timeline?tab=board&k={KEY}");
    let (status, _, _) = ask(&s.web, "GET", &uri, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(sessions(&s.app), 0, "nothing reached the team's server");
}

/// The key of the address is good for one trade (docs/web.md): the answer that gives the tab its
/// session key retires it, so the copy the browser kept of the address — in the history, in a
/// synced session — asks for nothing.
#[tokio::test]
async fn the_key_of_the_address_is_traded_for_a_session_key_once() {
    let s = setup().await;
    let web = for_server(&s.url, &s.ana);
    let (status, headers, _) = ask(
        &web,
        "GET",
        "/api/timeline?tab=board",
        &[("relevo-web-key", KEY)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = headers[web::SESSION_HEADER].to_str().unwrap().to_string();
    assert!(!session.is_empty() && session != KEY, "{session}");

    // The session key keeps working, as many times as the page refreshes.
    for _ in 0..3 {
        let (status, _, _) = ask(
            &web,
            "GET",
            "/api/timeline?tab=board",
            &[("relevo-web-key", session.as_str())],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    // The address's key is refused, and so is a request with neither key, or a key of another
    // run of `relevo web`.
    let other = for_server(&s.url, &s.ana);
    let foreign = trade(&other).await;
    for given in ["", KEY, "nope", foreign.as_str()] {
        for path in [
            "/api/timeline?tab=board",
            "/api/timeline/threads/r_aaaaaaaaaaaa",
        ] {
            let (status, h, body) = ask(&web, "GET", path, &[("relevo-web-key", given)]).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{given} {path}");
            assert_eq!(json(&body)["error"]["code"], "FORBIDDEN");
            assert_security_headers(&h, "403");
        }
    }
}

/// A first request that did not go well trades nothing: the page retries every 30 seconds with
/// the key of the address, which is still in its `sessionStorage`.
#[tokio::test]
async fn a_first_request_that_fails_trades_nothing() {
    // The team's server cannot be reached at all.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let web = for_server(&format!("http://{addr}"), "rlv_nothing");
    let key = [("relevo-web-key", KEY)];
    let (status, headers, _) = ask(&web, "GET", "/api/timeline?tab=board", &key).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        headers.get(web::SESSION_HEADER).is_none(),
        "a 502 trades nothing"
    );
    let (status, _, _) = ask(&web, "GET", "/api/timeline?tab=board", &key).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "the key still works");
    // The team's server says no: the same.
    let s = setup().await;
    let web = for_server(&s.url, "rlv_wrong");
    let (status, headers, _) = ask(&web, "GET", "/api/timeline?tab=board", &key).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers.get(web::SESSION_HEADER).is_none(),
        "an error trades nothing"
    );
    let (status, _, _) = ask(&web, "GET", "/api/timeline?tab=board", &key).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the key still works");
}

#[tokio::test]
async fn host_and_origin_must_be_this_server() {
    let s = setup().await;
    let key = ("relevo-web-key", s.key.as_str());
    let uri = "/api/timeline?tab=board";
    for host in [
        "evil.example",
        "evil.example:47123",
        "127.0.0.1",
        "127.0.0.1:47124",
        "localhost.evil.example:47123",
        "[::1]:47123",
        "",
    ] {
        for path in ["/", uri] {
            let (status, headers, _) = ask(&s.web, "GET", path, &[("host", host), key]).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{host} {path}");
            assert_security_headers(&headers, "wrong host");
        }
    }
    for host in ["127.0.0.1:47123", "localhost:47123"] {
        let (status, _, _) = ask(&s.web, "GET", uri, &[("host", host), key]).await;
        assert_eq!(status, StatusCode::OK, "{host}");
        let origin = format!("http://{host}");
        let (status, _, _) = ask(
            &s.web,
            "GET",
            uri,
            &[("host", host), ("origin", &origin), key],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{origin}");
    }
    for origin in [
        "http://evil.example",
        "null",
        "https://127.0.0.1:47123",
        "http://127.0.0.1:47124",
        "http://localhost:47123",
        "",
    ] {
        let (status, _, _) = ask(&s.web, "GET", uri, &[("origin", origin), key]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
    }
}

#[tokio::test]
async fn only_the_two_timeline_routes_are_passed_on_and_only_get() {
    let s = setup().await;
    let key = ("relevo-web-key", s.key.as_str());
    for (method, path) in [
        ("GET", "/api/inbox"),
        ("GET", "/api/inbox?wait=1"),
        ("GET", "/api/threads/r_aaaaaaaaaaaa"),
        ("GET", "/api/people"),
        ("POST", "/api/send"),
        ("DELETE", "/api/session"),
        ("GET", "/api/dist/manifest.json"),
        ("GET", "/health"),
        ("POST", "/api/timeline?tab=board"),
        ("HEAD", "/api/timeline?tab=board"),
        ("HEAD", "/api/timeline/threads/r_aaaaaaaaaaaa"),
        ("OPTIONS", "/api/timeline?tab=board"),
        ("HEAD", "/"),
        ("PUT", "/api/timeline?tab=board"),
        ("DELETE", "/api/timeline/threads/r_aaaaaaaaaaaa"),
        ("GET", "/api/timeline/threads/r_aaaaaaaaaaaa/x"),
        ("GET", "/api/timeline/threads/..%2Fpeople"),
        ("GET", "/api/timeline/threads/a%3Fb"),
        ("GET", "/api/timeline/extra"),
        ("POST", "/"),
        ("GET", "/index.html"),
    ] {
        let (status, headers, body) = ask(&s.web, method, path, &[key]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        // An answer to HEAD has no body.
        if method != "HEAD" {
            assert_eq!(json(&body)["error"]["code"], "NOT_FOUND", "{method} {path}");
        }
        assert_security_headers(&headers, path);
    }
    // None of it reached the team's server: it would have counted as a session.
    assert_eq!(sessions(&s.app), 0);
}

#[tokio::test]
async fn the_timeline_is_passed_on_with_the_token_and_changes_nothing() {
    let s = setup().await;
    // Bob writes to Ana, through the team's router.
    let res = api::router(s.app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/send")
                .header("authorization", format!("Bearer {}", s.bob))
                .header("relevo-mentions", "1")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "text": "@ana <script>hi</script>" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let sessions_before = sessions(&s.app);

    let key = ("relevo-web-key", s.key.as_str());
    let (status, headers, body) = ask(&s.web, "GET", "/api/timeline?tab=threads", &[key]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/json");
    assert_security_headers(&headers, "timeline");
    let v = json(&body);
    assert_eq!(v["you"], "ana");
    assert_eq!(v["names"]["bob"], "Bob");
    let id = v["threads"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(v["threads"][0]["pending"], true);
    assert_eq!(
        v["threads"][0]["first_note"]["text"],
        "@ana <script>hi</script>"
    );

    let (status, _, body) = ask(
        &s.web,
        "GET",
        &format!("/api/timeline/threads/{id}"),
        &[key],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["notes"][0]["author"], "bob");

    // The team server's errors come through as they are.
    let (status, _, body) = ask(&s.web, "GET", "/api/timeline?tab=nope", &[key]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&body)["error"]["code"], "BAD_REQUEST");
    let (status, _, body) = ask(
        &s.web,
        "GET",
        "/api/timeline/threads/r_aaaaaaaaaaaa",
        &[key],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["code"], "NOT_FOUND");

    // Reading made no session and delivered nothing: Ana's agent still gets Bob's note as new.
    assert_eq!(sessions(&s.app), sessions_before);
    let res = api::router(s.app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/inbox")
                .header("authorization", format!("Bearer {}", s.ana))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let inbox = json(&res.into_body().collect().await.unwrap().to_bytes());
    assert_eq!(
        inbox["threads"][0]["new_notes"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn an_unreachable_team_server_is_a_502_the_page_understands() {
    // A port nothing listens on.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let web = for_server(&format!("http://{addr}"), "rlv_nothing");
    let (status, headers, body) = ask(
        &web,
        "GET",
        "/api/timeline?tab=board",
        &[("relevo-web-key", KEY)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(json(&body)["error"]["code"], "UNREACHABLE");
    assert_security_headers(&headers, "502");
    // A wrong token is the team server's answer, passed on.
    let s = setup().await;
    let web = for_server(&s.url, "rlv_wrong");
    let (status, _, body) = ask(
        &web,
        "GET",
        "/api/timeline?tab=board",
        &[("relevo-web-key", KEY)],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&body)["error"]["code"], "UNAUTHORIZED");
}

/// The note renderer's own tests, with node. Skipped (and said so) where node is not installed.
#[test]
fn the_note_renderer_passes_its_tests() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/render.test.js");
    match std::process::Command::new("node").arg(script).output() {
        Err(e) => eprintln!("node is not installed ({e}): the renderer's tests did not run"),
        Ok(out) => assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// A `relevo web` of this test's own, with a stand-in `xdg-open` in front of it that records what
/// it was given, and a config whose team server is a port nothing listens on: every ask is a 502.
#[cfg(target_os = "linux")]
struct WebRun {
    child: Child,
    /// What it has printed, gathered as it comes by a thread of this test's.
    out: Arc<Mutex<String>>,
    /// The temporary folder of this run.
    dir: PathBuf,
    /// `$XDG_RUNTIME_DIR`, where the launcher goes.
    run: PathBuf,
    /// The stand-in opener's marker files: `seen.args`, `seen.file`, `seen.modes`, `seen.done`.
    seen: PathBuf,
}

#[cfg(target_os = "linux")]
impl WebRun {
    /// Starts `relevo` with these arguments. The opener is a stand-in of this test's, and the
    /// team's server a port nothing listens on, so every ask is a 502.
    fn start(tag: &str, args: &[&str]) -> WebRun {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(relevo::db::random_id(&format!("relevo-web-{tag}-")));
        let (bin, run) = (dir.join("bin"), dir.join("run"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A stand-in opener: it records its arguments, the file it was given and their modes.
        let seen = dir.join("seen");
        let opener = bin.join("xdg-open");
        std::fs::write(
            &opener,
            format!(
                "#!/bin/sh\nout={seen}\nprintf '%s\\n' \"$@\" > \"$out.args\"\n\
                 cp \"$1\" \"$out.file\"\nstat -c %a \"$1\" \"$(dirname \"$1\")\" > \"$out.modes\"\n\
                 touch \"$out.done\"\n",
                seen = seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.join("config.json");
        relevo::config::save(
            &config,
            &Config {
                url: "http://127.0.0.1:9".to_string(),
                token: "rlv_nothing".to_string(),
                cert: None,
                adapters: vec![],
            },
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_relevo"))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &dir)
            .env("RELEVO_CONFIG", &config)
            .env("XDG_RUNTIME_DIR", &run)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Its stdout, gathered line by line as it comes: the pipe must not fill, and the test
        // reads what was printed.
        let out = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stdout.take().unwrap();
        let gathered = out.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(&mut pipe);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                gathered.lock().unwrap().push_str(&line);
                line.clear();
            }
        });
        WebRun {
            child,
            out,
            dir,
            run,
            seen,
        }
    }

    /// Waits for the opener to have run: by then the launcher file is there.
    fn wait_opener(&self) {
        wait_until("the opener never ran", || {
            self.seen.with_extension("done").exists()
        });
    }

    /// Everything printed so far.
    fn printed(&self) -> String {
        self.out.lock().unwrap().clone()
    }

    /// Waits until `needle` is among what was printed.
    fn wait_printed(&self, needle: &str) {
        let wanted = needle.to_string();
        wait_until(&format!("it never printed {wanted:?}"), || {
            self.printed().contains(&wanted)
        });
    }

    /// What the opener was given: one argument per line.
    fn opener_args(&self) -> String {
        std::fs::read_to_string(self.seen.with_extension("args")).unwrap()
    }

    /// The copy the opener made of the file it was given.
    fn opener_copy(&self) -> String {
        std::fs::read_to_string(self.seen.with_extension("file")).unwrap()
    }

    /// The modes of the launcher file and of its folder, as `stat` said them.
    fn opener_modes(&self) -> String {
        std::fs::read_to_string(self.seen.with_extension("modes")).unwrap()
    }

    /// The launcher file the opener was given.
    fn launcher(&self) -> PathBuf {
        PathBuf::from(self.opener_args().trim())
    }

    /// The address the launcher sends the browser on to: the one with the key of this run.
    fn address(&self) -> String {
        let copy = self.opener_copy();
        let from = copy.find("url=").unwrap() + 4;
        copy[from..].split('"').next().unwrap().to_string()
    }

    fn key(&self) -> String {
        self.address().split("#k=").nth(1).unwrap().to_string()
    }

    fn port(&self) -> u16 {
        self.address()
            .trim_start_matches("http://127.0.0.1:")
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Asks the local server as the page would, and answers with the whole answer.
    fn ask(&self, key: &str) -> String {
        use std::io::{Read, Write};
        let port = self.port();
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            conn,
            "GET /api/timeline?tab=board HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Relevo-Web-Key: {key}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        conn.read_to_string(&mut answer).unwrap();
        answer
    }

    /// Stops it as the system would, and answers with how it ended. It must leave on its own:
    /// this never kills the process, which would hide one that ignored the signal.
    fn stop(&mut self, signal: i32) -> ExitStatus {
        unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "it is still running 10 s after the signal"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for WebRun {
    fn drop(&mut self) {
        // Only a test that failed gets here with the process still running.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Waits for `done`, and fails with `what` when it never happens.
#[cfg(target_os = "linux")]
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `relevo web` gives the opener a file of its user's, never the address with the key: every
/// local user can read a command line. The file sends the browser on to the address, and is gone
/// once the page asked with the key.
#[cfg(target_os = "linux")]
#[test]
fn the_key_never_goes_in_the_openers_command_line() {
    let run = WebRun::start("open", &["web"]);
    run.wait_opener();
    let args = run.opener_args();
    let key = run.key();
    let launcher = run.launcher();
    assert!(
        !args.contains(&key),
        "the key is in the opener's arguments: {args}"
    );
    assert_eq!(args.lines().count(), 1, "{args}");
    assert!(launcher.starts_with(&run.run), "{args}");
    // It sends the browser on to the address with the key, and only this user can read it.
    let address = run.address();
    assert!(
        run.opener_copy().contains(&format!(
            r#"<meta http-equiv="refresh" content="0;url={address}">"#
        )),
        "{}",
        run.opener_copy()
    );
    assert_eq!(
        run.opener_modes().split_whitespace().collect::<Vec<_>>(),
        ["600", "700"]
    );
    assert!(launcher.exists());
    // The page asks with the key: the launcher, and the folder of its own around it, are gone.
    let answer = run.ask(&key);
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
    assert!(!launcher.exists());
    assert!(!launcher.parent().unwrap().exists());
}

/// Stopped before the page ever asked, which is how it usually ends, `relevo web` deletes the
/// launcher that holds the key however it is stopped: Ctrl-C, a kill, a terminal closed. The
/// process must leave on its own, so this never kills it.
#[cfg(target_os = "linux")]
#[test]
fn the_launcher_is_deleted_however_relevo_web_is_stopped() {
    for (name, signal) in [
        ("SIGINT", libc::SIGINT),
        ("SIGTERM", libc::SIGTERM),
        ("SIGHUP", libc::SIGHUP),
    ] {
        let mut run = WebRun::start("stop", &["web"]);
        run.wait_opener();
        let launcher = run.launcher();
        assert!(
            launcher.exists() && launcher.parent().unwrap().exists(),
            "{name}: the launcher is where the opener was told, still holding the key"
        );
        let status = run.stop(signal);
        assert!(
            status.success(),
            "{name}: it did not stop by itself: {status:?}"
        );
        assert!(
            !launcher.exists(),
            "{name}: the launcher file is left behind"
        );
        assert!(
            !launcher.parent().unwrap().exists(),
            "{name}: its folder is left behind"
        );
    }
}

/// Stdout piped (as in a service unit): the key of the address is not printed, only the URL
/// without it, so the key does not end up in a log.
#[cfg(target_os = "linux")]
#[test]
fn the_key_is_not_printed_when_stdout_is_not_a_terminal() {
    let run = WebRun::start("key-quiet", &["web"]);
    run.wait_printed("http://127.0.0.1:");
    let printed = run.printed();
    let key = run.key();
    assert!(
        !printed.contains(&key),
        "key was printed to non-terminal stdout: {printed}"
    );
    assert!(
        printed.contains("http://127.0.0.1:"),
        "URL was not printed at all: {printed}"
    );
}

/// Stopped before the page asked, `relevo web` deletes the launcher too.
#[cfg(target_os = "linux")]
#[test]
fn the_launcher_is_deleted_when_relevo_web_stops() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(relevo::db::random_id("relevo-web-launcher-"));
    std::fs::create_dir(&base).unwrap();
    let address = format!("http://127.0.0.1:{PORT}/#k={KEY}");
    let launcher = web::Launcher::new(&base, &address).unwrap();
    let file = launcher.path().to_path_buf();
    assert!(!file.to_string_lossy().contains(KEY));
    assert!(std::fs::read_to_string(&file).unwrap().contains(&address));
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(file.parent().unwrap()), 0o700);
    let s = for_server("http://127.0.0.1:9", "rlv_nothing");
    s.keep_launcher(launcher);
    assert!(file.exists());
    s.drop_launcher();
    assert!(!file.exists() && !file.parent().unwrap().exists());
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    std::fs::remove_dir(&base).unwrap();
}
