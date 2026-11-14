//! Release notes on the global board (docs/protocol.md, "Release notes"): posted once per newer
//! version by the person `relevo`, then read through the HTTP router like any board thread.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::release::{self, Outcome};
use relevo::{App, Clock, api, db};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tower::ServiceExt;

const T0: i64 = 1_794_134_460;

/// The CHANGELOG of the release `top`: its section first, and dated.
fn log(top: &str) -> String {
    format!(
        "# Changelog\n\n## {top} (13-nov-2026)\n\nNotes of {top}.\n\n## 0.4.4 (12-nov-2026)\n\nOld.\n"
    )
}

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
            .execute(
                "INSERT INTO meta (key, value) VALUES ('announced', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = ?1",
                [v],
            )
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
        self.announce_with(version, &log(version), post)
    }

    fn announce_with(&self, version: &str, changelog: &str, post: bool) -> Outcome {
        release::announce(&self.app.db(), version, changelog, post, T0 + 30).unwrap()
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
    // Marked as the server's, and the server's person is in no participants or delivery list.
    assert_eq!(th["new_notes"][0]["system"], true);
    assert!(
        th["participants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p != "relevo"),
        "{th}"
    );
    let whole = s.get("bob", &format!("/api/threads/{thread}")).await;
    assert_eq!(whole["notes"][0]["system"], true);
    assert!(
        whole["participants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p != "relevo")
    );
    assert!(
        whole["delivery"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["handle"] != "relevo"),
        "{whole}"
    );
    assert_eq!(
        th["new_notes"][0]["text"],
        "Relevo 0.5.0\n\nNotes of 0.5.0."
    );
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
    assert_eq!(t["first_note"]["system"], true);
    // A reply is anyone's own: not marked.
    assert!(t["last_note"].get("system").is_none(), "{t}");
    let v = s
        .get("ana", &format!("/api/timeline/threads/{thread}"))
        .await;
    assert_eq!(v["names"]["relevo"], "Relevo");

    // A newer version posts again, in a thread of its own.
    assert!(matches!(s.announce("0.6.0", true), Outcome::Posted(t) if t != thread));
    assert_eq!(s.announced().as_deref(), Some("0.6.0"));
}

#[tokio::test]
async fn the_release_note_counts_against_no_threads_hourly_guard() {
    let s = server(&["ana"], Some("0.4.4"));
    let Outcome::Posted(thread) = s.announce("0.5.0", true) else {
        panic!("not posted");
    };
    // Twenty replies in the hour go in: the release note is not one of the twenty.
    let send = |i| {
        s.call(
            "ana",
            "POST",
            "/api/send",
            Some(json!({ "thread": thread, "text": format!("Reply {i}") })),
        )
    };
    for i in 0..20 {
        let (status, v) = send(i).await;
        assert_eq!(status, StatusCode::OK, "{i}: {v}");
    }
    let (status, v) = send(20).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], "THREAD_BUSY", "{v}");
}

#[tokio::test]
async fn a_new_database_a_downgrade_or_the_same_version_posts_nothing() {
    // A database made by this version starts at it when this build is a release; made between
    // releases, it records nothing and posts nothing, so the release still posts.
    let conn = db::open(":memory:").unwrap();
    let recorded: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
            r.get(0)
        })
        .optional()
        .unwrap();
    let version = env!("CARGO_PKG_VERSION");
    let released = release::release_section(release::CHANGELOG, version).is_some();
    assert_eq!(recorded.as_deref(), released.then_some(version));
    assert_eq!(
        release::announce(&conn, version, release::CHANGELOG, true, T0).unwrap(),
        if released {
            Outcome::Announced
        } else {
            Outcome::NotReleased
        }
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
async fn a_build_that_is_not_a_release_posts_and_records_nothing() {
    let s = server(&["ana"], Some("0.4.4"));
    // Between releases: a 0.5.0 binary while the CHANGELOG is topped by 0.6.0, still being
    // written; and a 0.6.0 build before its release.
    let dev = "# Changelog\n\n## 0.6.0 (unreleased)\n\nNext.\n\n## 0.5.0 (13-nov-2026)\n\nNow.\n";
    assert_eq!(s.announce_with("0.5.0", dev, true), Outcome::NotReleased);
    assert_eq!(s.announce_with("0.6.0", dev, true), Outcome::NotReleased);
    // No section at all.
    assert_eq!(s.announce_with("0.5.1", dev, true), Outcome::NotReleased);
    // With the notes off too: recording 0.6.0 now would keep its release from posting.
    assert_eq!(s.announce_with("0.6.0", dev, false), Outcome::NotReleased);
    assert_eq!(s.announce_with("0.5.0", dev, false), Outcome::NotReleased);
    assert_eq!(s.announced().as_deref(), Some("0.4.4"));
    assert_eq!(s.board_threads(), 0);
    // The release itself posts.
    let released = dev.replace("(unreleased)", "(20-nov-2026)");
    assert!(matches!(
        s.announce_with("0.6.0", &released, true),
        Outcome::Posted(_)
    ));
}

#[tokio::test]
async fn an_unreadable_announced_version_is_taken_as_older_and_written_over() {
    let s = server(&["ana"], Some("not a version"));
    assert!(matches!(s.announce("0.5.0", true), Outcome::Posted(_)));
    assert_eq!(s.announced().as_deref(), Some("0.5.0"));
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
            "INSERT INTO meta (key, value) VALUES ('announced', '0.4.4')
             ON CONFLICT (key) DO UPDATE SET value = '0.4.4'",
            [],
        )
        .unwrap();
    }
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let conn = db::open(&path).unwrap();
                release::announce(&conn, "0.5.0", &log("0.5.0"), true, T0).unwrap()
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
    let Outcome::Posted(thread) = s.announce("0.5.0", true) else {
        panic!("not posted");
    };
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
        json!({ "thread": thread, "text": "@relevo thanks" }),
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

/// A database of 0.4.x where someone already has the name `relevo`, a person (`person`) or a
/// team: opened by this version, it gets no person of the server's own.
fn legacy(dir: &std::path::Path, person: bool) -> String {
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap().to_string();
    {
        let conn = db::open(&path).unwrap();
        // As a 0.4.x database: no person of the server's own yet.
        conn.execute_batch(
            "DELETE FROM tokens WHERE person_id = (SELECT CAST(value AS INTEGER) FROM meta
                                                  WHERE key = 'system_person');
             DELETE FROM people WHERE handle = 'relevo';
             DELETE FROM meta;",
        )
        .unwrap();
        db::invite(&conn, "ana", "Ana", None, "laptop", T0).unwrap();
        if person {
            db::invite(&conn, "relevo", "Rel Evo", None, "laptop", T0).unwrap();
        } else {
            db::add_team(&conn, "relevo", None, T0).unwrap();
        }
    }
    path
}

#[tokio::test]
async fn a_person_who_had_the_name_keeps_it_and_nothing_is_posted() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-release-"));
    std::fs::create_dir(&dir).unwrap();
    let path = legacy(&dir, true);
    let conn = db::open(&path).unwrap();
    let made: i64 = conn
        .query_row(
            "SELECT count(*) FROM meta WHERE key = 'system_person'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(made, 0);
    assert_eq!(
        release::announce(&conn, "0.5.0", &log("0.5.0"), true, T0).unwrap(),
        Outcome::NameTaken
    );
    let threads: i64 = conn
        .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(threads, 0);
    // They are a person like any other: the admin manages them.
    let token = db::add_token(&conn, "relevo", "phone", T0).unwrap();
    db::set_about(&conn, "relevo", "release engineering").unwrap();
    db::add_team(&conn, "backend", None, T0).unwrap();
    db::join_team(&conn, "backend", &["relevo".to_string()]).unwrap();
    assert!(
        db::list_people(&conn)
            .unwrap()
            .iter()
            .any(|p| p.handle == "relevo")
    );
    // Their notes are theirs, not release notes, and they can be mentioned.
    let s = Server {
        app: App::new(conn, Clock::fixed(T0 + 60), 5),
        tokens: HashMap::from([
            ("relevo".to_string(), token.secret),
            (
                "ana".to_string(),
                db::add_token(&db::open(&path).unwrap(), "ana", "x", T0)
                    .unwrap()
                    .secret,
            ),
        ]),
    };
    let (status, v) = s
        .call(
            "relevo",
            "POST",
            "/api/send",
            Some(json!({ "text": "@ana hi" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let inbox = s.get("ana", "/api/inbox").await;
    let note = &inbox["threads"][0]["new_notes"][0];
    assert_eq!(note["author"], "relevo");
    assert!(note.get("system").is_none(), "{note}");
    let thread = inbox["threads"][0]["id"].as_str().unwrap().to_string();
    let v = s
        .get("ana", &format!("/api/timeline/threads/{thread}"))
        .await;
    assert!(v["notes"][0].get("system").is_none(), "{v}");
    let (status, v) = s
        .call(
            "ana",
            "POST",
            "/api/send",
            Some(json!({ "text": "@relevo thanks" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    {
        let conn = s.app.db();
        db::deactivate(&conn, "relevo", T0).unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_team_that_had_the_name_counts_as_taken() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-release-"));
    std::fs::create_dir(&dir).unwrap();
    let path = legacy(&dir, false);
    let conn = db::open(&path).unwrap();
    assert_eq!(
        release::announce(&conn, "0.5.0", &log("0.5.0"), true, T0).unwrap(),
        Outcome::NameTaken
    );
    let announced: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(announced, "0.5.0");
    let threads: i64 = conn
        .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(threads, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}
