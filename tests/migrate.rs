//! A database of layout 5, made by relevo 0.2.1 itself (`tests/fixtures/make-layout5.sh`),
//! migrated in place when this version first opens it, then used through the HTTP router.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use tower::ServiceExt;

const LAYOUT5: &str = include_str!("fixtures/layout5.sql");
const FIXTURE: &str = include_str!("fixtures/layout5-tokens.json");

/// A directory of its own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(db::random_id("relevo-migrate-"));
        std::fs::create_dir(&dir).unwrap();
        TempDir(dir)
    }

    fn db(&self) -> String {
        self.0.join("relevo.db").to_str().unwrap().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The layout 5 database, as a file, and when its sessions were last seen.
fn layout5(path: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(LAYOUT5).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 5);
    conn.query_row("SELECT max(last_seen) FROM sessions", [], |r| r.get(0))
        .unwrap()
}

fn user_version(path: &str) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

/// One session of a person in the fixture: its token, session id and context.
struct Agent {
    app: Arc<App>,
    token: String,
    session: &'static str,
    context: &'static str,
}

impl Agent {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.token))
            .header("relevo-session", self.session)
            .header("content-type", "application/json");
        if !self.context.is_empty() {
            req = req.header("relevo-context", self.context);
        }
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

    async fn inbox(&self) -> Value {
        let (status, v) = self.call("GET", "/api/inbox", None).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn thread(&self, id: &str) -> Value {
        let (status, v) = self.call("GET", &format!("/api/threads/{id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn send(&self, body: Value) -> Value {
        let (status, v) = self.call("POST", "/api/send", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }
}

/// The fixture's tokens and thread ids.
struct Fixture {
    app: Arc<App>,
    v: Value,
}

impl Fixture {
    fn agent(&self, who: &str, session: &'static str, context: &'static str) -> Agent {
        Agent {
            app: self.app.clone(),
            token: self.v["tokens"][who].as_str().unwrap().to_string(),
            session,
            context,
        }
    }

    fn thread(&self, name: &str) -> String {
        self.v["threads"][name].as_str().unwrap().to_string()
    }
}

fn listed<'a>(inbox: &'a Value, id: &str) -> Option<&'a Value> {
    inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
}

fn new_count(inbox: &Value, id: &str) -> usize {
    listed(inbox, id).unwrap()["new_notes"]
        .as_array()
        .unwrap()
        .len()
}

fn delivery_of(th: &Value, who: &str) -> Value {
    th["delivery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["handle"] == who)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn a_layout_5_database_migrates_and_only_notes_that_reached_nobody_are_pending() {
    let dir = TempDir::new();
    let path = dir.db();
    let seen = layout5(&path);
    let app = App::new(db::open(&path).unwrap(), Clock::fixed(seen + 60), 5);
    assert_eq!(user_version(&path), 6);
    let f = Fixture {
        app,
        v: serde_json::from_str(FIXTURE).unwrap(),
    };
    let ana = f.agent("ana", "ana-cc-0001", "pagos-api");
    let term1 = f.agent("bob", "bob-term-01", "pagos-api");
    let term2 = f.agent("bob", "bob-term-02", "");

    // Read by Bob, not answered: it waited for him in layout 5; now it reached him, so it is
    // handled, when and where it reached him.
    let th = ana.thread(&f.thread("fetched")).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["by"], json!({ "label": "pi", "context": "pagos-api" }));
    assert_eq!(bob["handled_at"], bob["delivered_at"]);
    assert_eq!(bob["handled_by"], bob["by"]);
    assert_eq!(term1.thread(&f.thread("fetched")).await["pending"], false);
    // Answered: handled when Bob wrote, through that token (its session's context then is not
    // known).
    let th = ana.thread(&f.thread("replied")).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["handled_at"], th["notes"][1]["created_at"]);
    assert_eq!(bob["handled_by"], json!({ "label": "pi", "context": null }));
    // Answered without reading: nothing by Ana reached him, so there is nothing to tell Ana, but
    // the answer handled it.
    let bob = delivery_of(&ana.thread(&f.thread("blind")).await, "bob");
    assert_eq!(bob["note"], Value::Null);
    let th = term2.thread(&f.thread("blind")).await;
    assert_eq!(th["pending"], false);
    // Claimed and read: handled, and still claimed.
    let th = f
        .agent("carl", "carl-hermes1", "")
        .thread(&f.thread("claimed"))
        .await;
    let bob = delivery_of(&th, "bob");
    assert!(bob["handled_at"].is_string(), "{bob}");
    assert_eq!(bob["claimed_by"]["label"], "pi");

    // Only the note that reached no session of Bob's is pending, in every session of his.
    let v = term1.inbox().await;
    let pending: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["pending"] == true)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(pending, vec![f.thread("unread")], "{v}");
    assert_eq!(new_count(&v, &f.thread("unread")), 1);
    // A new session of Bob's starts with what he had: only that note is new for it.
    let fresh = f.agent("bob", "bob-term-03", "");
    let v = fresh.inbox().await;
    let ids: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![f.thread("unread")], "{v}");
    // Ana's team question is never pending; neither is anything of hers.
    let v = ana.inbox().await;
    assert!(
        v["threads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["pending"] == false),
        "{v}"
    );

    // And it works on: a new note by Ana in a handled thread is pending again, and Bob's
    // `done` handles it.
    let fetched = f.thread("fetched");
    ana.send(json!({ "thread": fetched, "text": "Any news?" }))
        .await;
    let v = term1.inbox().await;
    assert_eq!(listed(&v, &fetched).unwrap()["pending"], true);
    term1.send(json!({ "thread": fetched, "done": true })).await;
    assert!(listed(&term1.inbox().await, &fetched).is_none());

    // Opening it again changes nothing.
    drop(f);
    let conn = db::open(&path).unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM participants WHERE waiting = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);
    assert_eq!(user_version(&path), 6);
}

/// The columns of every table and the definition of every index, as `db::open` leaves them.
fn layout(path: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(path).unwrap();
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
            .prepare(&format!("SELECT * FROM pragma_table_info('{table}')"))
            .unwrap();
        let columns = stmt
            .query_map([], |r| {
                Ok(format!(
                    "{table}.{}: {} notnull={} default={:?} pk={}",
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?
                ))
            })
            .unwrap();
        for column in columns {
            out.push(column.unwrap());
        }
    }
    let indexes = conn
        .prepare(
            "SELECT name || ': ' || coalesce(sql, '') FROM sqlite_master
             WHERE type = 'index' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    out.extend(indexes);
    out
}

#[test]
fn a_migrated_database_has_the_layout_of_a_new_one() {
    let dir = TempDir::new();
    let migrated = dir.db();
    layout5(&migrated);
    db::open(&migrated).unwrap();
    let new = dir.0.join("new.db");
    let new = new.to_str().unwrap();
    db::open(new).unwrap();
    assert_eq!(layout(&migrated), layout(new));
}

#[test]
fn other_layouts_are_refused_and_left_as_they_are() {
    for (version, says) in [
        (4, "older version of relevo"),
        (7, "newer version of relevo"),
    ] {
        let dir = TempDir::new();
        let path = dir.db();
        layout5(&path);
        rusqlite::Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", version)
            .unwrap();
        let err = format!("{:#}", db::open(&path).unwrap_err());
        assert!(err.contains(says), "{err}");
        assert_eq!(user_version(&path), version);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let columns: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('participants') WHERE name = 'handled_seq'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(columns, 0);
    }
}
