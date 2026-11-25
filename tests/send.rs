//! `relevo send`: the binary against a real server on loopback. It opens a thread or answers one,
//! says its agent (`via`), and leaves no session behind, whether the send worked or not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::Arc;

use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

struct Server {
    app: Arc<App>,
    url: String,
    tokens: HashMap<String, String>,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn server(handles: &[&str]) -> Server {
    let app = App::new(db::open(":memory:").unwrap(), Clock::system(), 5).unwrap();
    let mut tokens = HashMap::new();
    for h in handles {
        let name = h[..1].to_uppercase() + &h[1..];
        let t = db::invite(&app.db(), h, &name, None, "joined", db::now_unix()).unwrap();
        tokens.insert(h.to_string(), t.secret);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(relevo::serve::serve(
        listener,
        api::router(app.clone()),
        None,
        relevo::serve::Limits::default(),
    ));
    let dir = std::env::temp_dir().join(db::random_id("relevo-send-"));
    std::fs::create_dir(&dir).unwrap();
    Server {
        app,
        url,
        tokens,
        dir,
    }
}

/// A request as `who`'s session `agent-one`, through the router.
async fn call(s: &Server, who: &str, method: &str, uri: &str, body: Option<Value>) -> Value {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", s.tokens[who]))
        .header("relevo-mentions", "1")
        .header("relevo-session", "agent-one")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            body.map(|b| b.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let res = api::router(s.app.clone()).oneshot(req).await.unwrap();
    let ok = res.status().is_success();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(ok, "{v}");
    v
}

async fn thread(s: &Server, who: &str, id: &str) -> Value {
    call(s, who, "GET", &format!("/api/threads/{id}"), None).await
}

/// The sessions `who` has on the server.
fn sessions(s: &Server, who: &str) -> Vec<db::SessionInfo> {
    db::list_sessions(&s.app.db(), Some(who), db::now_unix()).unwrap()
}

/// `relevo send` as `who`, with a home of its own and `stdin` as its input.
async fn send(s: &Server, who: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = command(s, who, args).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).await.unwrap();
    }
    drop(input);
    child.wait_with_output().await.unwrap()
}

fn command(s: &Server, who: &str, args: &[&str]) -> Command {
    let home = s.dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_relevo"));
    cmd.arg("send")
        .args(args)
        .current_dir(&home)
        .env("HOME", &home)
        .env("RELEVO_CONFIG", home.join("config.json"))
        .env("RELEVO_URL", &s.url)
        .env("RELEVO_TOKEN", &s.tokens[who])
        .env_remove("RELEVO_CERT")
        .env_remove("RELEVO_CONTEXT")
        .env("RELEVO_AUTO_UPDATE", "0")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// The notes on the server.
fn notes(s: &Server) -> i64 {
    s.app
        .db()
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap()
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn opens_a_thread_via_cli_and_prints_its_id() {
    let s = server(&["oscar", "ana"]).await;
    let out = send(&s, "ana", &["@oscar main is red since 13:40"], None).await;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let id = text(&out.stdout).trim_end().to_string();
    assert!(id.starts_with("r_"), "{id}");
    assert_eq!(text(&out.stdout), format!("{id}\n"));
    assert_eq!(text(&out.stderr), "Sent to @oscar (1 person).\n");
    let t = thread(&s, "oscar", &id).await;
    assert_eq!(t["notes"][0]["text"], "@oscar main is red since 13:40");
    assert_eq!(t["notes"][0]["via"], "CLI");
    assert_eq!(t["pending"], true);
    assert!(sessions(&s, "ana").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_the_note_from_stdin() {
    let s = server(&["oscar", "ana"]).await;
    let out = send(&s, "ana", &["-"], Some("Weekly digest\n\nAll green.\n")).await;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let id = text(&out.stdout).trim_end().to_string();
    assert!(
        text(&out.stderr).starts_with("Posted on the global board (1 person)"),
        "{}",
        text(&out.stderr)
    );
    let t = thread(&s, "oscar", &id).await;
    assert_eq!(t["kind"], "global");
    assert_eq!(t["notes"][0]["text"], "Weekly digest\n\nAll green.\n");
    assert!(sessions(&s, "ana").is_empty());
    // Nothing on stdin is misuse, and sends nothing.
    let out = send(&s, "ana", &["-"], Some(" \n")).await;
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("the note is empty"));
    assert!(out.stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_via_its_name_and_handles_the_thread() {
    let s = server(&["oscar", "ana"]).await;
    let opened = call(
        &s,
        "oscar",
        "POST",
        "/api/send",
        Some(json!({ "text": "@ana is main green again?" })),
    )
    .await;
    let id = opened["thread"].as_str().unwrap();
    let out = send(
        &s,
        "ana",
        &[
            "--reply",
            id,
            "--via",
            "GitHub Actions",
            "Green again after #128.",
        ],
        None,
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("{id}\n"));
    assert_eq!(text(&out.stderr), format!("Replied in thread {id}.\n"));
    // The session that wrote it is gone, and holds the thread no more.
    assert!(sessions(&s, "ana").is_empty());
    // Read before answering: Oscar's note is handled, not left pending for Ana.
    let t = thread(&s, "ana", id).await;
    assert_eq!(t["pending"], false, "{t}");
    assert_eq!(t["notes"][1]["via"], "GitHub Actions");
    let ana = t["delivery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["handle"] == "ana")
        .unwrap();
    assert!(ana["claimed_by"].is_null(), "{ana}");
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_exit_1_with_the_servers_code() {
    let s = server(&["oscar", "ana"]).await;
    // A mention of nobody: the people who can be named.
    let out = send(&s, "ana", &["@nobody hi"], None).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.contains("UNKNOWN_PERSON"), "{err}");
    assert_eq!(err.matches("othing was sent").count(), 1, "{err}");
    assert!(err.contains("- @oscar (Oscar)"), "{err}");
    assert!(sessions(&s, "ana").is_empty());
    // A thread that is not there (or not hers): nothing is written, no session stays.
    let out = send(&s, "ana", &["--reply", "r_nope", "hi"], None).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr).contains("NOT_FOUND"),
        "{}",
        text(&out.stderr)
    );
    assert!(sessions(&s, "ana").is_empty());
    // A token the server does not know.
    let mut s = s;
    s.tokens.insert("ana".into(), "rlv_nottherightone".into());
    let out = send(&s, "ana", &["@oscar hi"], None).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    // One line: the session it never had is not ended, nor reported.
    let err = text(&out.stderr);
    assert!(err.starts_with("Error: UNAUTHORIZED: "), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    // A server that is not there: one line, which says what to check, and no wait to end the
    // session.
    s.url = "http://127.0.0.1:9".into();
    let out = send(&s, "ana", &["@oscar hi"], None).await;
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.starts_with("Error: could not reach"), "{err}");
    assert!(err.contains("check with `relevo web`"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert_eq!(notes(&s), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_note_may_start_with_a_hyphen() {
    let s = server(&["oscar", "ana"]).await;
    let out = send(&s, "ana", &["-5 tests fail on main, @oscar"], None).await;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let id = text(&out.stdout).trim_end().to_string();
    let t = thread(&s, "oscar", &id).await;
    assert_eq!(t["notes"][0]["text"], "-5 tests fail on main, @oscar");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reader_gone_is_no_error_once_the_note_is_written() {
    let s = server(&["oscar", "ana"]).await;
    let mut child = command(&s, "ana", &["@oscar main is red"]).spawn().unwrap();
    // Nobody reads the id: writing it fails with a broken pipe.
    drop(child.stdout.take());
    drop(child.stdin.take());
    let out = child.wait_with_output().await.unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "Sent to @oscar (1 person).\n");
    assert_eq!(notes(&s), 1);
    assert!(sessions(&s, "ana").is_empty());
}
