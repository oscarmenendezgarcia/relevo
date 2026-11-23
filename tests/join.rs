//! Invitations (docs/protocol.md, "Invitations"), through the router: a code works once, for 24
//! hours, and is traded for a token; the pages show it without using it; an address that tries
//! too many codes that do not work is refused for an hour. `relevo serve` and the install script
//! for real are in `serve_invite_and_join_for_real`.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const NOW: i64 = 1_800_000_000;
const URL: &str = "https://relevo.office:7878";

/// A temporary directory, removed at the end.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Temp {
        let dir = std::env::temp_dir().join(db::random_id("relevo-join-"));
        std::fs::create_dir(&dir).unwrap();
        Temp(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A dist in `dir` whose binaries are `binary`'s bytes, for each of `targets`.
fn make_dist(dir: &Path, binary: &[u8], targets: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut files = vec![];
    for target in targets {
        let name = format!("relevo-{target}");
        std::fs::write(dir.join(&name), binary).unwrap();
        files.push(json!({ "name": name, "kind": "binary", "target": target,
                           "size": binary.len(), "sha256": sha256(binary) }));
    }
    files
        .push(json!({ "name": "relevo-pi-0.7.0.tar.gz", "kind": "pi", "size": 2, "sha256": "00" }));
    let manifest = json!({ "version": "0.7.0", "files": files });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(dir.join("manifest.sig"), [7u8; 64]).unwrap();
}

struct Fixture {
    app: Arc<App>,
    _temp: Temp,
}

/// A server at `URL` with its own certificate, a dist for Linux, and `ana`, not invited yet.
fn fixture() -> Fixture {
    let temp = Temp::new();
    let dist = temp.0.join("dist");
    make_dist(
        &dist,
        b"elf",
        &["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"],
    );
    let app = App::with_dist(
        db::open(":memory:").unwrap(),
        Clock::fixed(NOW),
        5,
        Some(dist),
    );
    Fixture { app, _temp: temp }
}

/// `URL`, as `serve`'s own TLS address: with its key's pin and its certificate's fingerprint.
fn own() -> db::Address {
    db::Address {
        url: URL.into(),
        pin: Some("PIN+/=".into()),
        fingerprint: Some("AB:CD".into()),
    }
}

/// Another address, with nothing pinned (a proxy's, or plain http).
fn other(url: &str) -> db::Address {
    db::Address {
        url: url.into(),
        pin: None,
        fingerprint: None,
    }
}

impl Fixture {
    fn invite(&self, handle: &str) -> String {
        let now = self.app.clock.now();
        db::invite_link(&self.app.db(), handle, "Ana", None, &own(), now)
            .unwrap()
            .code
    }

    /// The status, the headers and the body of a request from `peer` (none: as through the
    /// router alone).
    async fn call(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        peer: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let mut req = match body {
            Some(body) => req
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        if let Some(peer) = peer {
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        let res = api::router(self.app.clone()).oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, headers, body)
    }

    async fn join(&self, code: &str) -> (StatusCode, Value) {
        let (status, _, body) = self
            .call(
                "POST",
                "/api/join",
                Some(json!({ "code": code })),
                None,
                &[],
            )
            .await;
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    async fn page(&self, code: &str, user_agent: &str) -> (StatusCode, HeaderMap, String) {
        let uri = format!("/join/{code}");
        let (status, headers, body) = self
            .call("GET", &uri, None, None, &[("user-agent", user_agent)])
            .await;
        (status, headers, String::from_utf8(body).unwrap())
    }

    async fn script(&self, code: &str) -> (StatusCode, String) {
        let uri = format!("/join/{code}/install");
        let (status, _, body) = self.call("GET", &uri, None, None, &[]).await;
        (status, String::from_utf8(body).unwrap())
    }
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn an_invitation_works_once_and_gets_a_token_that_works() {
    let f = fixture();
    let code = f.invite("ana");
    // `XXXX-XXXX`, of the alphabet without 0, 1, I and O.
    assert_eq!(code.len(), 9, "{code}");
    assert_eq!(&code[4..5], "-");
    assert!(
        code.replace('-', "")
            .bytes()
            .all(|c| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(&c)),
        "{code}"
    );
    // Only its hash is stored.
    let stored: String = f
        .app
        .db()
        .query_row("SELECT hash || url FROM invites", [], |r| r.get(0))
        .unwrap();
    assert!(!stored.contains(&code.replace('-', "")) && !stored.contains(&code));

    let (status, body) = f.join(&code).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["handle"], "ana");
    assert_eq!(body["url"], URL);
    let token = body["token"].as_str().unwrap();
    assert!(token.starts_with("rlv_"));
    let (status, _, people) = f
        .call(
            "GET",
            "/api/people",
            None,
            None,
            &[("authorization", &format!("Bearer {token}"))],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&people).unwrap()["you"],
        "ana"
    );
    let tokens = db::list_tokens(&f.app.db(), Some("ana")).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].label, "joined 2027-01-15");

    // Once.
    let (status, body) = f.join(&code).await;
    assert_eq!(
        (status, error_code(&body)),
        (StatusCode::GONE, "INVITE_USED")
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Ask whoever invited you for a new link")
    );
    assert_eq!(db::list_tokens(&f.app.db(), Some("ana")).unwrap().len(), 1);
}

#[tokio::test]
async fn codes_are_read_in_any_case_with_or_without_the_dash() {
    let f = fixture();
    let code = f.invite("ana");
    let loose = code.replace('-', "").to_lowercase();
    assert_eq!(f.join(&loose).await.0, StatusCode::OK);
    for bad in [
        "",
        "nope",
        "AAAA-AAA",
        "AAAA-AAAAA",
        "0000-1111",
        "IIII-OOOO",
    ] {
        let (status, body) = f.join(bad).await;
        assert_eq!(
            (status, error_code(&body)),
            (StatusCode::NOT_FOUND, "INVITE_UNKNOWN"),
            "{bad}"
        );
    }
    let (status, _, body) = f
        .call("POST", "/api/join", Some(json!({ "kode": "x" })), None, &[])
        .await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (status, error_code(&body)),
        (StatusCode::BAD_REQUEST, "BAD_REQUEST")
    );
}

#[tokio::test]
async fn an_invitation_expires_after_24_hours() {
    let f = fixture();
    let code = f.invite("ana");
    f.app.clock.advance(24 * 3600 - 1);
    assert_eq!(f.page(&code, "").await.0, StatusCode::OK);
    f.app.clock.advance(1);
    let (status, body) = f.join(&code).await;
    assert_eq!(
        (status, error_code(&body)),
        (StatusCode::GONE, "INVITE_EXPIRED")
    );
    let (status, _, page) = f.page(&code, "").await;
    assert_eq!(status, StatusCode::GONE);
    assert!(page.contains("This invitation has expired"), "{page}");
    assert!(
        page.contains("Invitations work once and last 24 hours. Ask whoever invited you"),
        "{page}"
    );
    assert!(
        db::list_tokens(&f.app.db(), Some("ana"))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_new_invitation_replaces_the_unused_one_and_deactivating_drops_it() {
    let f = fixture();
    let first = f.invite("ana");
    let second = f.invite("ana");
    assert_ne!(first, second);
    assert_eq!(error_code(&f.join(&first).await.1), "INVITE_UNKNOWN");
    let count = |f: &Fixture| -> i64 {
        f.app
            .db()
            .query_row("SELECT count(*) FROM invites", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&f), 1);
    db::deactivate(&f.app.db(), "ana", NOW).unwrap();
    assert_eq!(error_code(&f.join(&second).await.1), "INVITE_UNKNOWN");
    assert_eq!(count(&f), 0);
    // Invited back, and joined: a used invitation stays, used, when a new one is made.
    let third = f.invite("ana");
    assert_eq!(f.join(&third).await.0, StatusCode::OK);
    let fourth = f.invite("ana");
    assert_eq!(count(&f), 2);
    assert_eq!(error_code(&f.join(&third).await.1), "INVITE_USED");
    assert_eq!(f.join(&fourth).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_page_shows_the_invitation_without_using_it() {
    let f = fixture();
    let code = f.invite("ana");
    let (status, headers, page) = f.page(&code, "Mozilla/5.0 (X11; Linux x86_64)").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.starts_with("default-src 'none'; style-src 'sha256-"),
        "{csp}"
    );
    assert!(csp.contains("script-src 'sha256-"), "{csp}");
    // Nothing from elsewhere, nothing inline but the style and the script the policy names.
    assert!(
        !page.contains(" src=") && !page.contains(" href="),
        "{page}"
    );
    let handler = page.split(" on").skip(1).any(|rest| {
        let name: String = rest.chars().take_while(char::is_ascii_lowercase).collect();
        !name.is_empty() && rest[name.len()..].starts_with('=')
    });
    assert!(!page.contains(" style=") && !handler, "{page}");
    assert_eq!(page.matches("<style>").count(), 1);
    assert_eq!(page.matches("<script>").count(), 1);

    assert!(page.contains("You're invited to Relevo"));
    assert!(page.contains("You'll join as <b>@ana</b>. "));
    assert!(page.contains("Open this on the computer where your agents run."));
    assert!(!page.contains("not encrypted"));
    assert!(page.contains(&format!("{URL}/join/{code}</code>")));
    let curl = format!("curl -sS --pinnedpubkey sha256//PIN+/= -k {URL}/join/{code}/install");
    assert!(page.contains(&format!("{curl} | sh</code>")), "{page}");
    assert!(page.contains("Works once · expires in 24 h"));
    // For agents, folded but in the page: what to run, and to ask first.
    assert!(page.contains("<details class=\"agents\" id=\"for-agents\">\n<summary>For AI agents"));
    assert!(page.contains(&format!("{curl} | sh -s -- --setup AGENT")));
    assert!(page.contains("Replace AGENT with what you are: <code>claude-code</code>"));
    assert!(page.contains("first ask the user"));
    // On Linux, the Linux tab; the dist has no binary for macOS, and says so there.
    assert!(page.contains("aria-controls=\"panel-linux\" aria-selected=\"true\""));
    assert!(page.contains("<div id=\"panel-mac\" role=\"tabpanel\" hidden>"));
    assert!(page.contains("No macOS build on this server yet. Ask whoever invited you."));
    // From a Mac, the macOS tab.
    let (_, _, page) = f
        .page(&code, "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5)")
        .await;
    assert!(page.contains("aria-controls=\"panel-mac\" aria-selected=\"true\""));
    assert!(page.contains("<div id=\"panel-linux\" role=\"tabpanel\" hidden>"));

    // Looking used nothing up.
    assert_eq!(f.join(&code).await.0, StatusCode::OK);
    let (status, _, page) = f.page(&code, "").await;
    assert_eq!(status, StatusCode::GONE);
    assert!(
        page.contains("<h1>This invitation was already used</h1>"),
        "{page}"
    );
    assert!(page.contains("Ask whoever invited you for a new link."));
    assert!(!page.contains("curl"));
    let (status, _, page) = f.page("ZZZZ-ZZZZ", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(page.contains("<h1>Invitation not found</h1>"), "{page}");
}

#[tokio::test]
async fn the_page_says_a_bot_joins_as_one() {
    let f = fixture();
    let code = f.invite("reviewer");
    db::set_bot(&f.app.db(), "reviewer", true).unwrap();
    let (status, _, page) = f.page(&code, "Mozilla/5.0 (X11; Linux x86_64)").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains("You'll join as <b>@reviewer</b> (a bot). "),
        "{page}"
    );
}

#[tokio::test]
async fn names_are_text_on_the_page() {
    let f = fixture();
    let code = db::invite_link(
        &f.app.db(),
        "eve",
        "<script>alert(1)</script> & \"Eve\"",
        None,
        &own(),
        NOW,
    )
    .unwrap()
    .code;
    let (_, _, page) = f.page(&code, "").await;
    // The name is not on the page; nothing of it runs.
    assert!(!page.contains("alert(1)"), "{page}");
    assert_eq!(page.matches("<script>").count(), 1);
}

#[tokio::test]
async fn without_a_certificate_of_its_own_nothing_is_pinned() {
    let f = fixture();
    let code = db::invite_link(
        &f.app.db(),
        "ana",
        "Ana",
        None,
        &other("https://relevo.example.com"),
        NOW,
    )
    .unwrap()
    .code;
    let (_, _, page) = f.page(&code, "").await;
    assert!(
        page.contains(&format!(
            "curl -sS https://relevo.example.com/join/{code}/install | sh"
        )),
        "{page}"
    );
    assert!(!page.contains("sha256//") && !page.contains(" -k https"));
    assert!(!page.contains("not encrypted"));
    let (_, script) = f.script(&code).await;
    assert!(script.contains("\npin=''\nfingerprint=''\n"), "{script}");

    // Plain http: the page says to use it only on a network you trust.
    let code = db::invite_link(
        &f.app.db(),
        "bob",
        "Bob",
        None,
        &other("http://10.0.0.5:7878"),
        NOW,
    )
    .unwrap()
    .code;
    let (_, _, page) = f.page(&code, "").await;
    assert!(page.contains(
        "<p class=\"warn\">This link is not encrypted (plain HTTP). Use it only on a network you"
    ));
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `sh -n`, and shellcheck when there is one (CI has it).
fn check_sh(script: &str) {
    let temp = Temp::new();
    let file = temp.0.join("install.sh");
    std::fs::write(&file, script).unwrap();
    let out = Command::new("sh").arg("-n").arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if have("shellcheck") {
        let out = Command::new("shellcheck").arg(&file).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    } else {
        eprintln!("no shellcheck here: only sh -n checked the install script");
    }
}

#[tokio::test]
async fn the_install_script_is_the_invitations() {
    let f = fixture();
    let code = f.invite("ana");
    let (status, script) = f.script(&code).await;
    assert_eq!(status, StatusCode::OK);
    assert!(script.starts_with("#!/bin/sh\n"));
    assert!(
        script.contains(&format!("\nurl='{URL}'\ncode='{code}'\n")),
        "{script}"
    );
    assert!(
        script.contains("\npin='PIN+/='\nfingerprint='AB:CD'\n"),
        "{script}"
    );
    let sum = sha256(b"elf");
    assert!(
        script.contains(&format!(
            "\nsums='x86_64-unknown-linux-gnu {sum}\naarch64-unknown-linux-gnu {sum}'\n"
        )),
        "{script}"
    );
    for placeholder in ["@URL@", "@CODE@", "@PIN@", "@FINGERPRINT@", "@SUMS@"] {
        assert!(
            !script.contains(placeholder),
            "{placeholder} left: {script}"
        );
    }
    check_sh(&script);
    // Looking used nothing up.
    assert_eq!(f.join(&code).await.0, StatusCode::OK);

    // A code that does not work: a script that says so, and fails.
    let (status, script) = f.script(&code).await;
    assert_eq!(status, StatusCode::GONE);
    check_sh(&script);
    let out = Command::new("sh").arg("-c").arg(&script).output().unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("This invitation was already used"), "{said}");
}

#[tokio::test]
async fn the_binary_comes_from_the_dist_while_the_code_works() {
    let f = fixture();
    let code = f.invite("ana");
    let get = |uri: String| {
        let f = &f;
        async move { f.call("GET", &uri, None, None, &[]).await }
    };
    let (status, headers, body) = get(format!(
        "/join/{code}/relevo?target=aarch64-unknown-linux-gnu"
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert_eq!(body, b"elf");
    // Only a binary the manifest lists, by its target.
    for target in ["aarch64-apple-darwin", "pi", "../manifest.json", ""] {
        let (status, ..) = get(format!("/join/{code}/relevo?target={target}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{target}");
    }
    let (status, ..) = get(format!("/join/{code}/relevo")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(f.join(&code).await.0, StatusCode::OK);
    let (status, _, body) = get(format!(
        "/join/{code}/relevo?target=aarch64-unknown-linux-gnu"
    ))
    .await;
    assert_eq!(status, StatusCode::GONE);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error_code(&body), "INVITE_USED");
}

#[tokio::test]
async fn an_address_that_tries_too_many_codes_is_refused_for_an_hour() {
    let f = fixture();
    let code = f.invite("ana");
    let from = |peer: &'static str, code: String| {
        let f = &f;
        async move {
            let (status, _, body) = f
                .call(
                    "POST",
                    "/api/join",
                    Some(json!({ "code": code })),
                    Some(peer),
                    &[],
                )
                .await;
            let body: Value = serde_json::from_slice(&body).unwrap();
            (status, error_code(&body).to_string())
        }
    };
    // Ten codes that do not work, through any of the routes.
    for i in 0..9 {
        let (status, _) = from("10.0.0.1:4000", format!("ZZZZ-ZZZ{}", i + 2)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let req = Request::builder()
        .uri("/join/ZZZZ-ZZZZ")
        .body(Body::empty())
        .unwrap();
    let mut req = req;
    req.extensions_mut()
        .insert(ConnectInfo("10.0.0.1:4001".parse::<SocketAddr>().unwrap()));
    let res = api::router(f.app.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // Then even a good one is refused, from that address and no other.
    let (status, said) = from("10.0.0.1:4002", code.clone()).await;
    assert_eq!(
        (status, said.as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED")
    );
    let (status, _, page) = f
        .call(
            "GET",
            &format!("/join/{code}"),
            None,
            Some("10.0.0.1:1"),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        String::from_utf8(page)
            .unwrap()
            .contains("Too many attempts")
    );
    let (status, ..) = f
        .call(
            "GET",
            &format!("/join/{code}"),
            None,
            Some("10.0.0.2:1"),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // An hour after the first, it may try again.
    f.app.clock.advance(3600);
    let (status, _) = from("10.0.0.1:4003", code.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // IPv6: a /64 is one address.
    for i in 0..10 {
        let peer: &'static str = Box::leak(format!("[2001:db8::{i}]:1").into_boxed_str());
        from(peer, "ZZZZ-ZZZZ".into()).await;
    }
    let code = f.invite("bob");
    let (status, _) = from("[2001:db8::ffff:1]:1", code.clone()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) = from("[2001:db8:0:1::1]:1", code).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_join_body_is_small() {
    let f = fixture();
    let code = f.invite("ana");
    let big = json!({ "code": code, "pad": "x".repeat(4096) });
    let (status, ..) = f
        .call("POST", "/api/join", Some(big), Some("10.0.0.1:1"), &[])
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Refused before it was read: it counts as no try, and the code still works.
    assert_eq!(f.join(&code).await.0, StatusCode::OK);
}

#[tokio::test]
async fn used_and_expired_codes_do_not_count_and_loopback_is_one_address() {
    let f = fixture();
    let used = f.invite("ana");
    assert_eq!(f.join(&used).await.0, StatusCode::OK);
    let expired = f.invite("bob");
    f.app.clock.advance(24 * 3600);
    let post = |peer: &'static str, code: &str| {
        let (f, body) = (&f, json!({ "code": code }));
        async move {
            f.call("POST", "/api/join", Some(body), Some(peer), &[])
                .await
                .0
        }
    };
    for _ in 0..20 {
        assert_eq!(post("10.0.0.1:1", &used).await, StatusCode::GONE);
        assert_eq!(post("10.0.0.1:1", &expired).await, StatusCode::GONE);
    }
    let fresh = f.invite("carl");
    assert_eq!(post("10.0.0.1:1", &fresh).await, StatusCode::OK);

    // Behind a proxy on this machine, everyone comes from loopback: one address, limited too.
    for i in 0..10 {
        let peer = if i % 2 == 0 {
            "127.0.0.1:1"
        } else {
            "[::ffff:127.0.0.1]:2"
        };
        assert_eq!(post(peer, "ZZZZ-ZZZZ").await, StatusCode::NOT_FOUND);
    }
    let code = f.invite("dora");
    assert_eq!(
        post("127.0.0.1:3", &code).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn past_the_ceiling_only_addresses_that_failed_are_refused() {
    let f = fixture();
    let code = f.invite("ana");
    let get = |peer: &'static str, code: &str| {
        let (f, path) = (&f, format!("/join/{code}"));
        async move { f.call("GET", &path, None, Some(peer), &[]).await.0 }
    };
    // 200 codes that do not exist, from 20 addresses (10 each).
    for a in 0..20 {
        for _ in 0..10 {
            let peer: &'static str = Box::leak(format!("10.1.{a}.1:1").into_boxed_str());
            assert_eq!(get(peer, "ZZZZ-ZZZZ").await, StatusCode::NOT_FOUND);
        }
    }
    // Someone with a good code, from an address that never failed, still gets in: many
    // addresses guessing cannot shut the team's invitations.
    assert_eq!(get("10.2.0.1:1", &code).await, StatusCode::OK);
    // A new address gets one try at a code that does not exist, then nothing for the hour,
    // not even with a good code.
    assert_eq!(get("10.3.0.1:1", "ZZZZ-ZZZZ").await, StatusCode::NOT_FOUND);
    assert_eq!(
        get("10.3.0.1:1", &code).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // An hour later the tries are old: it may try again.
    f.app.clock.advance(3600);
    assert_eq!(get("10.3.0.1:1", &code).await, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn tries_at_once_do_not_pass_the_limit_together() {
    let f = Arc::new(fixture());
    let tries: Vec<_> = (0..50)
        .map(|_| {
            let f = f.clone();
            tokio::spawn(async move {
                let body = json!({ "code": "ZZZZ-ZZZZ" });
                f.call("POST", "/api/join", Some(body), Some("10.0.0.9:1"), &[])
                    .await
                    .0
            })
        })
        .collect();
    let mut statuses = vec![];
    for t in tries {
        statuses.push(t.await.unwrap());
    }
    let unknown = statuses
        .iter()
        .filter(|s| **s == StatusCode::NOT_FOUND)
        .count();
    let refused = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((unknown, refused), (10, 40));
}

/// What a running `relevo serve` logged so far.
fn log_of(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// `relevo serve --tls` records its address and its key's pin; `relevo invite` prints the link;
/// the install script, run by sh with curl pinning that key, installs the binary from the dist
/// into a new HOME and connects it with the code. Neither the code nor the token is logged.
#[test]
fn serve_invite_and_join_for_real() {
    if !have("curl") {
        eprintln!("no curl here: skipped");
        return;
    }
    let temp = Temp::new();
    let db_file = temp.0.join("relevo.db");
    let db_path = db_file.to_str().unwrap();
    let relevo = env!("CARGO_BIN_EXE_relevo");
    let binary = std::fs::read(relevo).unwrap();
    let target = relevo::update::own_target();
    make_dist(&temp.0.join("dist"), &binary, &[&target]);
    let log = temp.0.join("serve.log");
    /// The server, stopped when the test ends, however it ends.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let server = Kill(
        Command::new(relevo)
            .args(["--db", db_path, "serve", "--addr", "127.0.0.1:0", "--tls"])
            .args(["--tls-name", "127.0.0.1"])
            .env("RUST_LOG", "debug")
            .env_remove("RELEVO_PUBLIC_URL")
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::fs::File::options().append(true).open(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let said = (0..200)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let said = log_of(&log);
            said.contains("invitation links point at").then_some(said)
        })
        .unwrap_or_else(|| panic!("serve did not start: {}", log_of(&log)));

    // What it recorded: its address, with the port it got, and its key's pin.
    let conn = db::open(db_path).unwrap();
    let public = db::public(&conn).unwrap();
    let url = public.url.clone().unwrap();
    assert!(url.starts_with("https://127.0.0.1:"), "{url}");
    assert!(said.contains(&url), "{said}");
    let cert = relevo::tls::stored(&relevo::tls::dir_for(db_path))
        .unwrap()
        .unwrap();
    assert_eq!(
        public.pin.clone().unwrap(),
        relevo::tls::key_pin(&cert.chain[0]).unwrap()
    );
    assert_eq!(public.fingerprint.clone().unwrap(), cert.fingerprint());
    drop(conn);

    // `relevo invite`: the link, to that address; --url says another.
    let admin = |args: &[&str]| {
        let out = Command::new(relevo)
            .args(["--db", db_path])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let said = admin(&[
        "invite",
        "ana",
        "--name",
        "Ana",
        "--url",
        "https://relevo.example.com/",
    ]);
    assert!(
        said.contains("\n  https://relevo.example.com/join/"),
        "{said}"
    );
    // Another address than serve's own TLS one: nothing of serve's certificate goes with it.
    let code_of = |said: &str| {
        said.split("/join/")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .unwrap()
            .to_string()
    };
    let address = |code: &str| {
        let conn = db::open(db_path).unwrap();
        match db::find_invite(&conn, code, db::now_unix()).unwrap() {
            db::Invite::Valid { address, .. } => address,
            other => panic!("{other:?}"),
        }
    };
    let elsewhere = address(&code_of(&said));
    assert_eq!((elsewhere.pin, elsewhere.fingerprint), (None, None));
    let said = admin(&["invite", "ana"]);
    let own = address(&code_of(&said));
    assert_eq!(own.pin, public.pin);
    assert_eq!(own.fingerprint, public.fingerprint);
    let code = code_of(&said);
    assert_eq!(
        said,
        format!(
            "Invited @ana (Ana). Send them this link — it works once, for 24 hours:\n\n  \
             {url}/join/{code}\n\nThey can open it, or just give it to their agent (Claude \
             Code, pi, Hermes).\n\nNote: {url} is reached from this machine only. Behind a \
             proxy, start the server with --public-url https://… (RELEVO_PUBLIC_URL), or give \
             this link's address with --url.\n"
        )
    );

    // The script, as the page says to run it, in a new HOME without ~/.local/bin in PATH.
    let home = temp.0.join("home");
    std::fs::create_dir(&home).unwrap();
    // A stand-in for Claude Code's `claude`, which records how it was run.
    let bin = temp.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let ran = temp.0.join("claude.log");
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$1 $2\" = 'mcp get' ] && exit 1\nexit 0\n",
            ran.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let curl =
        |pin: &str| format!("curl -sS --pinnedpubkey sha256//{pin} -k {url}/join/{code}/install");
    let pin = public.pin.clone().unwrap();
    let run_with = |pin: &str, extra: &str| {
        Command::new("sh")
            .arg("-c")
            .arg(format!("{} | sh -s -- {extra}", curl(pin)))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap()
    };
    let run = |extra: &str| run_with(&pin, extra);
    // Another key than the server's, as a machine in between would show: curl refuses it, even
    // with -k, and nothing is installed (sh is given nothing to run).
    let out = run_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("pinned public key"), "{err}");
    assert!(!home.join(".local").exists());
    // A binary that is not the one the manifest lists is never installed.
    let file = temp.0.join("dist").join(format!("relevo-{target}"));
    std::fs::write(&file, b"tampered").unwrap();
    let out = run("--setup claude-code");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not the file the server lists"), "{err}");
    assert!(!home.join(".local/bin/relevo").exists());
    assert_eq!(
        std::fs::read_dir(home.join(".local/bin")).unwrap().count(),
        0
    );
    std::fs::write(&file, &binary).unwrap();

    let out = run("--setup claude-code");
    let said = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}{err}");
    let installed = home.join(".local/bin/relevo");
    assert_eq!(std::fs::read(&installed).unwrap(), binary);
    assert!(
        said.starts_with("Downloading relevo for ")
            && said.contains(&format!(
                ")\nInstalled in ~/.local/bin/relevo\nConnected \
             to {url} as @ana (1 person)\nAdded Relevo to Claude Code.\n\nDone. Restart Claude \
             Code to load Relevo.\n\nNote: ~/.local/bin is not in your PATH. To type `relevo` \
             yourself, add to your shell profile:\n  export PATH=\"$HOME/.local/bin:$PATH\"\n"
            )),
        "{said}"
    );
    let installed_path = installed.display().to_string();
    assert_eq!(
        std::fs::read_to_string(&ran).unwrap(),
        format!("mcp get relevo\nmcp add --scope user relevo -- {installed_path} mcp\n")
    );
    assert!(home.join(".claude/commands/relevo.md").exists());
    let config: Value =
        serde_json::from_slice(&std::fs::read(home.join(".config/relevo/config.json")).unwrap())
            .unwrap();
    assert_eq!(config["url"], url.as_str());
    let token = config["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("rlv_"));
    assert!(
        config["cert"]
            .as_str()
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );

    // Once: the server's answer is a script that says why, and fails.
    let out = run("");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("relevo: This invitation was already used. Invitations work once"),
        "{err}"
    );

    drop(server);
    let said = log_of(&log);
    assert!(said.contains("joined with an invitation"), "{said}");
    for secret in [code.as_str(), &code.replace('-', ""), &token, &token[4..]] {
        assert!(!said.contains(secret), "the log has a secret: {said}");
    }
}
