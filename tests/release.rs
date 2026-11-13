//! Release notes on the global board (docs/protocol.md, "Release notes"): posted once per newer
//! version by the person `relevo`, then read through the HTTP router like any board thread.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::release::{self, Outcome};
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use tower::ServiceExt;

const T0: i64 = 1_794_134_460;

const LOG: &str = "# Changelog\n\n## 0.6.0 (unreleased)\n\n**Big thing.** It works.\n\n## 0.5.0 (13-nov-2026)\n\nSmaller.\n\n## 0.4.4 (12-nov-2026)\n\nOld.\n";

struct Server {
    app: Arc<App>,
    tokens: HashMap<String, String>,
}

/// A server with these people, on a database that last announced `announced` (`None`: nothing,
/// as one migrated from layout 7).
fn server(handles: &[&str], announced: Option<&str>) -> Server {
    let conn = db::open(":memory:").unwrap();
    let mut tokens = HashMap::new();
    for h in handles {
        let token = db::invite(&conn, h, &h.to_uppercase(), None, "laptop", T0).unwrap();
        tokens.insert(h.to_string(), token.secret);
    }
    match announced {
        Some(v) => conn
            .execute("UPDATE meta SET value = ?1 WHERE key = 'announced'", [v])
            .unwrap(),
        None => conn
            .execute("DELETE FROM meta WHERE key = 'announced'", [])
            .unwrap(),
    };
    Server {
        app: App::new(conn, Clock::fixed(T0 + 60), 5),
        tokens,
    }
}

impl Server {
    fn announce(&self, version: &str, post: bool) -> Outcome {
        release::announce(&self.app.db(), version, LOG, post, T0 + 30).unwrap()
    }

    fn announced(&self) -> Option<String> {
        self.app
            .db()
            .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
                r.get(0)
            })
            .ok()
    }

    fn board_threads(&self) -> i64 {
        self.app
            .db()
            .query_row(
                "SELECT count(*) FROM threads WHERE kind = 'global'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    async fn call(
        &self,
        who: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.tokens[who]))
            .header("relevo-session", format!("{who}-session-1"))
            .header("relevo-mentions", "1")
            .header("content-type", "application/json");
        req = req.header("relevo-context", "web");
        let body = body.map_or(Body::empty(), |b| Body::from(b.to_string()));
        let res = api::router(self.app.clone())
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, who: &str, uri: &str) -> Value {
        let (status, v) = self.call(who, "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {v}");
        v
    }
}

#[tokio::test]
async fn an_upgrade_posts_the_versions_notes_once_on_the_board() {
    let s = server(&["ana", "bob"], Some("0.4.4"));
    let Outcome::Posted(thread) = s.announce("0.5.0", true) else {
        panic!("not posted");
    };
    assert_eq!(s.announced().as_deref(), Some("0.5.0"));
    // A restart, or another process right after, posts nothing.
    assert_eq!(s.announce("0.5.0", true), Outcome::Announced);
    assert_eq!(s.board_threads(), 1);

    // A plain board thread: new for everyone once, pending for nobody, its author Relevo.
    let inbox = s.get("ana", "/api/inbox").await;
    let th = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == thread.as_str())
        .unwrap()
        .clone();
    assert_eq!(th["kind"], "global");
    assert_eq!(th["from"], "relevo");
    assert_eq!(th["title"], "Relevo 0.5.0");
    assert_eq!(th["named"], false);
    assert_eq!(th["pending"], false);
    assert_eq!(th["new_notes"][0]["author"], "relevo");
    assert_eq!(th["new_notes"][0]["via"], "release notes");
    assert_eq!(th["new_notes"][0]["text"], "Relevo 0.5.0\n\nSmaller.");
    let again = s.get("ana", "/api/inbox").await;
    assert!(again["threads"].as_array().unwrap().is_empty(), "{again}");
    // Anyone may reply, as on any board thread; it names nobody, so nothing becomes pending.
    let (status, v) = s
        .call(
            "bob",
            "POST",
            "/api/send",
            Some(json!({ "thread": thread, "text": "Nice." })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["kind"], "global");
    // It counts against nobody's daily limit of board threads: Ana can still open five.
    for i in 0..5 {
        let (status, v) = s
            .call(
                "ana",
                "POST",
                "/api/send",
                Some(json!({ "text": format!("Board {i}") })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    // The timeline names its author.
    let v = s.get("ana", "/api/timeline?tab=board").await;
    assert_eq!(v["names"]["relevo"], "Relevo");
    let t = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == thread.as_str())
        .unwrap();
    assert_eq!(t["from"], "relevo");
    let v = s
        .get("ana", &format!("/api/timeline/threads/{thread}"))
        .await;
    assert_eq!(v["names"]["relevo"], "Relevo");

    // A newer version posts again, in a thread of its own.
    assert!(matches!(s.announce("0.6.0", true), Outcome::Posted(t) if t != thread));
    assert_eq!(s.announced().as_deref(), Some("0.6.0"));
}

#[tokio::test]
async fn a_new_database_a_downgrade_or_the_same_version_posts_nothing() {
    // A database made by this version starts at it.
    let conn = db::open(":memory:").unwrap();
    let recorded: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(recorded, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        release::announce(
            &conn,
            env!("CARGO_PKG_VERSION"),
            release::CHANGELOG,
            true,
            T0
        )
        .unwrap(),
        Outcome::Announced
    );
    let s = server(&["ana"], Some("0.6.0"));
    assert_eq!(s.announce("0.5.0", true), Outcome::Announced);
    assert_eq!(s.announce("0.6.0", true), Outcome::Announced);
    assert_eq!(s.announced().as_deref(), Some("0.6.0"));
    assert_eq!(s.board_threads(), 0);
}

#[tokio::test]
async fn turned_off_it_records_the_version_without_posting() {
    let s = server(&["ana"], Some("0.4.4"));
    assert_eq!(s.announce("0.5.0", false), Outcome::Off);
    assert_eq!(s.announced().as_deref(), Some("0.5.0"));
    // Turned on again later, the old version is not posted.
    assert_eq!(s.announce("0.5.0", true), Outcome::Announced);
    assert_eq!(s.board_threads(), 0);
    assert!(release::enabled(false, None));
    assert!(!release::enabled(true, None));
    assert!(!release::enabled(false, Some("0")));
}

#[tokio::test]
async fn a_version_without_a_section_posts_and_records_nothing() {
    let s = server(&["ana"], Some("0.4.4"));
    assert_eq!(s.announce("0.5.1", true), Outcome::NoSection);
    assert_eq!(s.announced().as_deref(), Some("0.4.4"));
    assert_eq!(s.board_threads(), 0);
    // The release that has one posts it.
    assert!(matches!(s.announce("0.5.0", true), Outcome::Posted(_)));
}

#[tokio::test]
async fn a_database_that_announced_nothing_posts_as_after_an_upgrade() {
    let s = server(&["ana"], None);
    assert!(matches!(s.announce("0.5.0", true), Outcome::Posted(_)));
}

#[test]
fn several_processes_starting_at_once_post_once() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-release-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap().to_string();
    {
        let conn = db::open(&path).unwrap();
        db::invite(&conn, "ana", "Ana", None, "laptop", T0).unwrap();
        conn.execute(
            "UPDATE meta SET value = '0.4.4' WHERE key = 'announced'",
            [],
        )
        .unwrap();
    }
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let conn = db::open(&path).unwrap();
                release::announce(&conn, "0.5.0", LOG, true, T0).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<Outcome> = runs.into_iter().map(|r| r.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Posted(_)))
            .count(),
        1,
        "{outcomes:?}"
    );
    let conn = db::open(&path).unwrap();
    let board: i64 = conn
        .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(board, 1);
    drop(conn);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn relevo_is_nobody_anyone_can_be_or_name() {
    let s = server(&["ana", "bob"], Some("0.4.4"));
    assert!(matches!(s.announce("0.5.0", true), Outcome::Posted(_)));
    {
        let conn = s.app.db();
        for err in [
            db::invite(&conn, "relevo", "Mine", None, "laptop", T0).map(drop),
            db::add_token(&conn, "relevo", "laptop", T0).map(drop),
            db::add_team(&conn, "relevo", None, T0),
            db::set_about(&conn, "relevo", "me"),
            db::deactivate(&conn, "relevo", T0).map(drop),
        ] {
            let err = format!("{:#}", err.unwrap_err());
            assert!(err.contains("'relevo' is the server's own name"), "{err}");
        }
        db::add_team(&conn, "backend", None, T0).unwrap();
        let Err(err) = db::join_team(&conn, "backend", &["relevo".to_string()]) else {
            panic!("joined");
        };
        assert!(format!("{err:#}").contains("server's own name"));
        // In no people list.
        let people: Vec<String> = db::list_people(&conn)
            .unwrap()
            .into_iter()
            .map(|p| p.handle)
            .collect();
        assert_eq!(people, ["ana", "bob"]);
        assert!(
            db::list_tokens(&conn, None)
                .unwrap()
                .iter()
                .all(|t| t.handle != "relevo")
        );
    }
    let v = s.get("ana", "/api/people").await;
    let handles: Vec<&str> = v["team"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles, ["ana", "bob"]);
    let inbox = s.get("ana", "/api/inbox").await;
    assert!(
        inbox["team"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["handle"] != "relevo")
    );
    // A mention of it names nobody, in a new thread, a reply, or the old `to`.
    for body in [
        json!({ "text": "@relevo hello" }),
        json!({ "to": ["relevo"], "text": "hello" }),
    ] {
        let (status, v) = s.call("ana", "POST", "/api/send", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        assert_eq!(v["error"]["code"], "UNKNOWN_PERSON");
        assert_eq!(v["error"]["unknown"], json!(["relevo"]));
    }
    // And a board thread someone else opens does not have it among its participants.
    let (_, v) = s
        .call(
            "ana",
            "POST",
            "/api/send",
            Some(json!({ "text": "Lunch?" })),
        )
        .await;
    assert_eq!(v["reached"], 1);
}

#[tokio::test]
async fn a_person_who_took_the_handle_before_keeps_it_and_nothing_is_posted() {
    let s = server(&["ana"], Some("0.4.4"));
    s.app
        .db()
        .execute(
            "INSERT INTO people (handle, name, created_at) VALUES ('relevo', 'Old', 0)",
            [],
        )
        .unwrap();
    assert_eq!(s.announce("0.5.0", true), Outcome::HandleTaken);
    assert_eq!(s.announced().as_deref(), Some("0.5.0"));
    assert_eq!(s.board_threads(), 0);
}
