//! `relevo statusline` (docs/agents.md, "The status line (Claude Code)"): the line in each
//! state against a real server on loopback, the cache, a server that does not answer, and what
//! `relevo connect` does to Claude Code's settings.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use relevo::config::Config;
use relevo::statusline::{Cache, FRESH, GRACE, Shown, line, render};
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
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
    let app = App::new(db::open(":memory:").unwrap(), Clock::system(), 5);
    let mut tokens = HashMap::new();
    for h in handles {
        let name = h[..1].to_uppercase() + &h[1..];
        let t = db::invite(&app.db(), h, &name, None, "Claude Code", db::now_unix()).unwrap();
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
    let dir = std::env::temp_dir().join(db::random_id("relevo-statusline-"));
    std::fs::create_dir(&dir).unwrap();
    Server {
        app,
        url,
        tokens,
        dir,
    }
}

fn config(url: &str, token: &str) -> Config {
    Config {
        url: url.into(),
        token: token.into(),
        cert: None,
        adapters: vec![],
    }
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(db::random_id("relevo-statusline-"));
    std::fs::create_dir(&dir).unwrap();
    dir
}

async fn post(s: &Server, who: &str, body: Value) -> Value {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/send")
        .header("authorization", format!("Bearer {}", s.tokens[who]))
        .header("relevo-mentions", "1")
        .header("relevo-session", "agent-one")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let res = api::router(s.app.clone()).oneshot(req).await.unwrap();
    let ok = res.status().is_success();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(ok, "{v}");
    v
}

/// `GET /api/inbox` as `who`'s session `agent-one`: what is new reaches it.
async fn inbox(s: &Server, who: &str) {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .uri("/api/inbox")
        .header("authorization", format!("Bearer {}", s.tokens[who]))
        .header("relevo-session", "agent-one")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = api::router(s.app.clone()).oneshot(req).await.unwrap();
    assert!(res.status().is_success());
}

/// Every row of every table, to compare before and after reading.
fn snapshot(s: &Server) -> Vec<String> {
    let conn = s.app.db();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut out = Vec::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .unwrap();
        let n = stmt.column_count();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                let cells: Vec<String> = (0..n)
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect();
                Ok(format!("{table}: {}", cells.join(" | ")))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        out.extend(rows);
    }
    out
}

fn sessions(s: &Server) -> i64 {
    s.app
        .db()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap()
}

const GREY: &str = "\x1b[38;5;245mrelevo\x1b[0m";

fn amber(n: u64) -> String {
    format!("{GREY} \x1b[38;5;221m● {n}\x1b[0m")
}

fn offline(since: i64, now: i64) -> String {
    render(&Shown::Offline(since), now)
}

/// A port nothing listens on.
fn closed_url() -> String {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", free.local_addr().unwrap());
    drop(free);
    url
}

/// A server that takes connections, counts them, and never answers.
async fn silent() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            c.fetch_add(1, Ordering::SeqCst);
            held.push(conn);
        }
    });
    (url, count)
}

fn write_cache(path: &Path, cache: &Cache) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(cache).unwrap()).unwrap();
}

fn read_cache(path: &Path) -> Cache {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_line_counts_what_waits_on_the_person_and_changes_nothing() {
    let s = server(&["oscar", "ana", "pedro"]).await;
    let oscar = config(&s.url, &s.tokens["oscar"]);
    let cache = s.dir.join("cache/statusline.json");
    let now = db::now_unix();
    // Nothing waits: `relevo`, grey.
    assert_eq!(line(Some(&oscar), Some(&cache), now).await, GREY);
    let a = post(&s, "ana", json!({ "text": "@oscar is the copy final?" })).await;
    let p = post(
        &s,
        "pedro",
        json!({ "text": "@oscar can you review /refunds?" }),
    )
    .await;
    // A thread that does not name Oscar does not wait on him.
    post(&s, "ana", json!({ "text": "@pedro lunch?" })).await;
    let before = snapshot(&s);
    let n_sessions = sessions(&s);
    // The cache is fresh: still 0, from it.
    assert_eq!(
        line(Some(&oscar), Some(&cache), now + FRESH - 1).await,
        GREY
    );
    // Stale: asked again, 2 wait, amber.
    let later = now + FRESH;
    assert_eq!(line(Some(&oscar), Some(&cache), later).await, amber(2));
    assert_eq!(line(Some(&oscar), Some(&cache), later + 1).await, amber(2));
    // Read only: nothing delivered, handled or claimed, no session made or seen.
    assert_eq!(snapshot(&s), before);
    assert_eq!(sessions(&s), n_sessions);
    // The cache says neither the server nor the token, and only its owner reads it.
    let text = std::fs::read_to_string(&cache).unwrap();
    assert!(
        !text.contains(&s.tokens["oscar"]) && !text.contains("127.0.0.1"),
        "{text}"
    );
    let mode = std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Both handled (an agent of Oscar's fetched them, then marked them seen): grey again once
    // the cache is stale.
    inbox(&s, "oscar").await;
    for t in [&a, &p] {
        post(&s, "oscar", json!({ "thread": t["thread"], "seen": true })).await;
    }
    assert_eq!(line(Some(&oscar), Some(&cache), later + 2).await, amber(2));
    assert_eq!(line(Some(&oscar), Some(&cache), later + FRESH).await, GREY);
    // Not configured: nothing at all.
    assert_eq!(line(None, Some(&cache), later).await, "");
    // A refused token says so.
    let wrong = config(&s.url, "rlv_wrong");
    let red = line(Some(&wrong), None, now).await;
    assert_eq!(red, "\x1b[38;5;203mrelevo ● token refused\x1b[0m");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_cache_answers_without_asking_a_stale_one_asks() {
    let (url, asked) = silent().await;
    let dir = temp_dir();
    let cache = dir.join("statusline.json");
    let now = db::now_unix();
    write_cache(
        &cache,
        &Cache {
            at: now,
            shown: Shown::Waiting(4),
            last_count: Some((now, 4)),
            offline_since: None,
        },
    );
    let c = config(&url, "rlv_x");
    assert_eq!(
        line(Some(&c), Some(&cache), now + FRESH - 1).await,
        amber(4)
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    // A clock gone back makes it stale too.
    let t = std::time::Instant::now();
    assert_eq!(line(Some(&c), Some(&cache), now - 1).await, amber(4));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    // It never waits much longer than the timeout.
    assert!(
        t.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        t.elapsed()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_out_of_reach_is_offline_since_it_was_first_seen_so() {
    let dir = temp_dir();
    let cache = dir.join("statusline.json");
    let c = config(&closed_url(), "rlv_x");
    let t0 = db::now_unix();
    // Without a cache.
    assert_eq!(line(Some(&c), Some(&cache), t0).await, offline(t0, t0));
    // Later, still since then.
    let t1 = t0 + 10 * FRESH;
    assert_eq!(line(Some(&c), Some(&cache), t1).await, offline(t0, t1));
    // With a count in the cache: a refused connection is offline all the same.
    write_cache(
        &cache,
        &Cache {
            at: t0,
            shown: Shown::Waiting(3),
            last_count: Some((t0, 3)),
            offline_since: None,
        },
    );
    let t2 = t0 + FRESH;
    assert_eq!(line(Some(&c), Some(&cache), t2).await, offline(t2, t2));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timeout_shows_the_last_count_while_recent_else_offline() {
    let (url, _) = silent().await;
    let dir = temp_dir();
    let cache = dir.join("statusline.json");
    let c = config(&url, "rlv_x");
    let now = db::now_unix();
    // No count to show: offline.
    assert_eq!(line(Some(&c), Some(&cache), now).await, offline(now, now));
    // A recent count: shown, and the cache keeps when the server stopped answering.
    write_cache(
        &cache,
        &Cache {
            at: now - FRESH,
            shown: Shown::Waiting(3),
            last_count: Some((now - FRESH, 3)),
            offline_since: None,
        },
    );
    assert_eq!(line(Some(&c), Some(&cache), now).await, amber(3));
    assert_eq!(read_cache(&cache).offline_since, Some(now));
    // Too old: offline since the first timeout.
    let later = now - FRESH + GRACE;
    assert_eq!(
        line(Some(&c), Some(&cache), later).await,
        offline(now, later)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The binary, with a home of its own, Relevo's config and Claude Code's folder in it.
fn relevo(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_relevo"));
    cmd.env("HOME", home)
        .env("RELEVO_CONFIG", home.join("config.json"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env_remove("RELEVO_URL")
        .env_remove("RELEVO_TOKEN")
        .env_remove("RELEVO_CERT")
        .env_remove("XDG_CACHE_HOME")
        .env("RELEVO_AUTO_UPDATE", "0")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_prints_the_line_or_nothing() {
    let s = server(&["oscar", "ana"]).await;
    let home = s.dir.join("home");
    std::fs::create_dir(&home).unwrap();
    // Not configured: nothing, and success (a failing command blanks Claude Code's line).
    let out = relevo(&home).arg("statusline").output().await.unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"");
    post(&s, "ana", json!({ "text": "@oscar ping" })).await;
    let out = relevo(&home)
        .arg("statusline")
        .env("RELEVO_URL", &s.url)
        .env("RELEVO_TOKEN", &s.tokens["oscar"])
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{}\n", amber(1))
    );
    assert_eq!(out.stderr, b"");
    let cached: Vec<_> = std::fs::read_dir(home.join(".cache/relevo"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        cached.len() == 1 && cached[0].starts_with("statusline-"),
        "{cached:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_sets_claude_codes_status_line_only_when_there_is_none() {
    let s = server(&["ana"]).await;
    let home = s.dir.join("home");
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let connect = |args: &[&str], env_dir: Option<&Path>| {
        let mut cmd = relevo(&home);
        cmd.args(["connect", &s.url, &s.tokens["ana"], "--no-command"])
            .args(args);
        if let Some(dir) = env_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        async move {
            let out = cmd.output().await.unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        }
    };
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_relevo")).unwrap();
    let ours = format!("{} statusline", exe.display());
    let original =
        "{\n  \"model\": \"opus\",\n  \"permissions\": {\n    \"allow\": [\"Bash(ls)\"]\n  }\n}\n";
    std::fs::write(&settings, original).unwrap();
    // No terminal to ask: skipped, saying how.
    let out = connect(&[], None).await;
    assert!(
        out.contains("skipped Claude Code's status line (no terminal to ask): run relevo connect again with --statusline to set it"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), original);
    let out = connect(&["--no-statusline"], None).await;
    assert!(!out.contains("status line"), "{out}");
    // Added, every other key as it was.
    let out = connect(&["--statusline"], None).await;
    assert!(out.contains("set Claude Code's status line to"), "{out}");
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(
        text.starts_with(original.trim_end().trim_end_matches('}').trim_end()),
        "{text}"
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["model"], "opus");
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(
        v["statusLine"],
        json!({ "type": "command", "command": ours, "refreshInterval": 30 })
    );
    let out = connect(&["--statusline"], None).await;
    assert!(out.contains("already shows Relevo"), "{out}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), text);
    // Someone else's status line: never changed, and it says how to add Relevo to it.
    let foreign = "{\"statusLine\": {\"type\": \"command\", \"command\": \"~/.claude/line.sh\"}}";
    std::fs::write(&settings, foreign).unwrap();
    let out = connect(&["--statusline"], None).await;
    assert!(
        out.contains("already has a status line") && out.contains(&format!("run `{ours}`")),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), foreign);
    // Settings that are not valid JSON: never changed.
    let broken = "{ \"model\": \"opus\", }";
    std::fs::write(&settings, broken).unwrap();
    let out = connect(&["--statusline"], None).await;
    assert!(out.contains("it is not valid JSON"), "{out}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), broken);
    // CLAUDE_CONFIG_DIR names Claude Code's folder; with --statusline it is made.
    let other = home.join("claude-other");
    let out = connect(&["--statusline"], Some(&other)).await;
    assert!(out.contains("set Claude Code's status line"), "{out}");
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(other.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(v["statusLine"]["command"], json!(ours));
    // No Claude Code folder and no --statusline: nothing said, nothing made.
    let none = home.join("no-claude");
    let out = connect(&[], Some(&none)).await;
    assert!(!out.contains("status line"), "{out}");
    assert!(!none.exists());
}
