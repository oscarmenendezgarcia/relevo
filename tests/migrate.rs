//! Databases of layout 5, made by relevo 0.2.1 itself (`tests/fixtures/make-layout5.sh`), of
//! layout 6, made by relevo 0.3.0 (`tests/fixtures/make-layout6.sh`), and of layout 7, made by
//! relevo 0.4.4 (`tests/fixtures/make-layout7.sh`), migrated in place when this version first
//! opens them, then used through the HTTP router.

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
const LAYOUT6: &str = include_str!("fixtures/layout6.sql");
const FIXTURE6: &str = include_str!("fixtures/layout6-tokens.json");
const LAYOUT7: &str = include_str!("fixtures/layout7.sql");
const FIXTURE7: &str = include_str!("fixtures/layout7-tokens.json");

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
    made_by(path, LAYOUT5, 5)
}

/// The layout 6 database, as a file, and when its sessions were last seen.
fn layout6(path: &str) -> i64 {
    made_by(path, LAYOUT6, 6)
}

/// The layout 7 database, as a file, and when its sessions were last seen.
fn layout7(path: &str) -> i64 {
    made_by(path, LAYOUT7, 7)
}

fn made_by(path: &str, dump: &str, layout: i64) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(dump).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, layout);
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
async fn a_layout_5_database_migrates_and_what_waited_for_a_reply_stays_pending() {
    let dir = TempDir::new();
    let path = dir.db();
    let seen = layout5(&path);
    let app = App::new(
        db::open_migrating(&path).unwrap(),
        Clock::fixed(seen + 60),
        5,
    );
    assert_eq!(user_version(&path), 8);
    let f = Fixture {
        app,
        v: serde_json::from_str(FIXTURE).unwrap(),
    };
    let ana = f.agent("ana", "ana-cc-0001", "pagos-api");
    let term1 = f.agent("bob", "bob-term-01", "pagos-api");
    let term2 = f.agent("bob", "bob-term-02", "");

    // Read by Bob, not answered: it waited for his reply in layout 5, so it still does. It
    // reached him (seen by pi), but reaching is not handling: Ana is not told it was handled.
    let th = ana.thread(&f.thread("fetched")).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["by"], json!({ "label": "pi", "context": "pagos-api" }));
    assert!(bob["delivered_at"].is_string(), "{bob}");
    assert_eq!(bob["handled_at"], Value::Null);
    assert_eq!(bob["handled_by"], Value::Null);
    assert_eq!(term1.thread(&f.thread("fetched")).await["pending"], true);
    // Answered: handled when Bob wrote, through that token (its session's context then is not
    // known).
    let th = ana.thread(&f.thread("replied")).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["handled_at"], th["notes"][1]["created_at"]);
    assert_eq!(bob["handled_by"], json!({ "label": "pi", "context": null }));
    // Answered without reading: nothing by Ana reached him, so there is nothing to tell Ana, but
    // in layout 5 the answer ended the wait, so it is not pending.
    let bob = delivery_of(&ana.thread(&f.thread("blind")).await, "bob");
    assert_eq!(bob["note"], Value::Null);
    let th = term2.thread(&f.thread("blind")).await;
    assert_eq!(th["pending"], false);
    // Closed by Ana after Bob read it: open now, and handled by Bob up to its latest note (when
    // that was written: layout 5 did not keep when it was closed), from no known session, so
    // never pending.
    let th = ana.thread(&f.thread("closed")).await;
    assert_eq!(th["status"], "open");
    let bob = delivery_of(&th, "bob");
    assert!(bob["delivered_at"].is_string(), "{bob}");
    assert_eq!(
        bob["handled_at"],
        th["notes"].as_array().unwrap().last().unwrap()["created_at"]
    );
    assert_eq!(bob["handled_by"], Value::Null);
    assert_eq!(term1.thread(&f.thread("closed")).await["pending"], false);
    // Claimed and read, not answered: still pending, and still claimed.
    let th = f
        .agent("carl", "carl-hermes1", "")
        .thread(&f.thread("claimed"))
        .await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["handled_at"], Value::Null);
    assert_eq!(bob["claimed_by"]["label"], "pi");

    // What waited for Bob in layout 5 is pending in his sessions: the note that reached none
    // (new), and the two read but not answered.
    let v = term1.inbox().await;
    let mut pending: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["pending"] == true)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    pending.sort();
    let mut expected = vec![f.thread("unread"), f.thread("fetched"), f.thread("claimed")];
    expected.sort();
    assert_eq!(pending, expected, "{v}");
    assert_eq!(new_count(&v, &f.thread("unread")), 1);
    assert_eq!(new_count(&v, &f.thread("fetched")), 0);
    // A new session of Bob's gets the notes still pending as new, once: the read one too, as a
    // note that reached a session which never handled it. The claimed one stays with the
    // session that holds it.
    let fresh = f.agent("bob", "bob-term-03", "");
    let v = fresh.inbox().await;
    let ids: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![f.thread("unread"), f.thread("fetched")], "{v}");
    assert_eq!(new_count(&v, &f.thread("fetched")), 1);
    let v = fresh.inbox().await;
    assert_eq!(new_count(&v, &f.thread("fetched")), 0);
    assert_eq!(listed(&v, &f.thread("fetched")).unwrap()["pending"], true);
    // Bob's answers waited for Ana in layout 5 (they reached her, and she did nothing about
    // them), so they still do; her team question is never pending.
    let v = ana.inbox().await;
    let mut pending: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["pending"] == true)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    pending.sort();
    let mut expected = vec![f.thread("replied"), f.thread("blind")];
    expected.sort();
    assert_eq!(pending, expected, "{v}");
    // Its team question is a question to everyone, on the global board now, naming nobody.
    let th = ana.thread(&f.thread("team")).await;
    assert_eq!(th["pending"], false);
    assert_eq!(th["kind"], "global");
    assert_eq!(th["named"], false);
    assert!(
        th["delivery"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["named"] == false)
    );
    let th = ana.thread(&f.thread("fetched")).await;
    assert_eq!(th["kind"], "direct");
    assert!(
        th["delivery"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["named"] == true)
    );

    // And it works on: Bob's `done` handles a thread he had read, and a new note by Ana makes
    // it pending again.
    let fetched = f.thread("fetched");
    term1.send(json!({ "thread": fetched, "done": true })).await;
    assert!(listed(&term1.inbox().await, &fetched).is_none());
    let bob = delivery_of(&ana.thread(&fetched).await, "bob");
    assert_eq!(
        bob["handled_by"],
        json!({ "label": "pi", "context": "pagos-api" })
    );
    ana.send(json!({ "thread": fetched, "text": "Any news?" }))
        .await;
    let v = term1.inbox().await;
    assert_eq!(listed(&v, &fetched).unwrap()["pending"], true);
    term1.send(json!({ "thread": fetched, "done": true })).await;
    assert!(listed(&term1.inbox().await, &fetched).is_none());

    // Opening it again changes nothing: Bob's unread and claimed threads, and Ana's two answers,
    // still wait.
    drop(f);
    let conn = db::open(&path).unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM participants WHERE waiting = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 4);
    assert_eq!(user_version(&path), 8);
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
    let new = dir.0.join("new.db");
    let new = new.to_str().unwrap();
    db::open(new).unwrap();
    for (name, make) in [
        ("5.db", layout5 as fn(&str) -> i64),
        ("6.db", layout6),
        ("7.db", layout7),
    ] {
        let migrated = dir.0.join(name);
        let migrated = migrated.to_str().unwrap();
        make(migrated);
        db::open_migrating(migrated).unwrap();
        assert_eq!(layout(migrated), layout(new), "{name}");
        // Every reference holds: the threads table was made again.
        let conn = rusqlite::Connection::open(migrated).unwrap();
        let broken: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(broken, 0, "{name}");
    }
}

#[test]
fn a_layout_6_database_with_a_broken_thread_reference_names_the_row_and_refuses_to_start() {
    let dir = TempDir::new();
    let path = dir.db();
    layout6(&path);
    // A thread that is not there, with notes, participants and sessions still pointing at it: the
    // migration makes the `threads` table again, and these are the references it can break.
    let conn = rusqlite::Connection::open(&path).unwrap();
    // The bundled SQLite has DEFAULT_FOREIGN_KEYS=1, so FK is ON by default; turn it off to
    // delete a thread that child rows still point to — that is what this test sets up.
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute("DELETE FROM threads WHERE id = ?1", ["r_26jmvdxtfgsv"])
        .unwrap();
    let note: i64 = conn
        .query_row(
            "SELECT seq FROM notes WHERE thread_id = 'r_26jmvdxtfgsv' ORDER BY seq LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let err = format!("{:#}", db::open_migrating(&path).unwrap_err());
    // Which row, not only how many: the owner has to be able to find it.
    assert!(
        err.contains(&format!("notes rowid {note} refers to threads")),
        "{err}"
    );
    for table in ["participants", "session_threads"] {
        assert!(err.contains(table), "{table}: {err}");
    }
    // Nothing was written: the database is still layout 6, for `relevo backup` to copy.
    assert_eq!(user_version(&path), 6);
}

#[tokio::test]
async fn a_layout_7_database_gets_the_meta_table_and_this_versions_release_notes() {
    let dir = TempDir::new();
    let path = dir.db();
    let seen = layout7(&path);
    let conn = db::open_migrating(&path).unwrap();
    // The table is there, and has announced nothing: this start is an upgrade.
    let announced: i64 = conn
        .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(announced, 0);
    let version = env!("CARGO_PKG_VERSION");
    let outcome =
        relevo::release::announce(&conn, version, relevo::release::CHANGELOG, true, seen + 30)
            .unwrap();
    let relevo::release::Outcome::Posted(thread) = outcome else {
        panic!("{outcome:?}");
    };
    // Once: a restart posts nothing.
    assert_eq!(
        relevo::release::announce(&conn, version, relevo::release::CHANGELOG, true, seen + 40)
            .unwrap(),
        relevo::release::Outcome::Announced
    );
    // Everyone of the old database takes part, and nobody has it pending.
    let f = Fixture {
        app: App::new(conn, Clock::fixed(seen + 60), 5),
        v: serde_json::from_str(FIXTURE7).unwrap(),
    };
    for who in ["ana", "bob", "carl"] {
        let th = f.agent(who, "after-upgrade", "").thread(&thread).await;
        assert_eq!(th["kind"], "global", "{who}");
        assert_eq!(th["from"], "relevo", "{who}");
        assert_eq!(th["pending"], false, "{who}");
        assert_eq!(
            th["notes"][0]["text"].as_str().unwrap().lines().next(),
            Some(format!("Relevo {version}").as_str())
        );
    }
}

#[test]
fn a_failed_migration_from_layout_7_leaves_it_at_layout_7() {
    let dir = TempDir::new();
    let path = dir.db();
    layout7(&path);
    // A failure at the very end of the migration, once the inbox's indexes were dropped and made
    // again and the closed threads handled: opening them is refused.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER no_reopening BEFORE UPDATE OF status ON threads
             BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
        )
        .unwrap();
    let err = format!("{:#}", db::open_migrating(&path).unwrap_err());
    assert!(err.contains("from layout 7 to 8"), "{err}");
    assert!(err.contains("refused for the test"), "{err}");
    // Nothing of it stayed: still layout 7, its indexes as 0.4.x made them, its threads closed.
    assert_eq!(user_version(&path), 7);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let index: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'participants_inbox_claim'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(index.contains("resolved = 0"), "{index}");
    let closed: i64 = conn
        .query_row(
            "SELECT count(*) FROM threads WHERE status = 'closed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(closed, 3);
    // Once the cause is gone, it migrates.
    conn.execute_batch("DROP TRIGGER no_reopening").unwrap();
    drop(conn);
    db::open_migrating(&path).unwrap();
    assert_eq!(user_version(&path), 8);
}

#[test]
fn a_broken_reference_elsewhere_does_not_stop_the_migration() {
    let dir = TempDir::new();
    let path = dir.db();
    layout6(&path);
    // The idempotency key of a person who is not on the server: broken under 0.3 as well, and
    // nothing to do with the threads table the migration makes again. It must not lock the owner
    // out of their own server.
    let conn = rusqlite::Connection::open(&path).unwrap();
    // FK is ON by default (DEFAULT_FOREIGN_KEYS=1); turn it off to insert an orphaned key.
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute(
        "INSERT INTO send_keys (person_id, key, thread_id, note_id, status, claimed_by, created_at)
         VALUES (999, 'k-orphan', 'r_pxsxyijycdc5', NULL, 'open', NULL, 1794393850)",
        [],
    )
    .unwrap();
    drop(conn);
    db::open_migrating(&path).unwrap();
    assert_eq!(user_version(&path), 8);
    // Still broken: the check the migration makes is over the tables that refer to threads.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let broken: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_foreign_key_check('send_keys')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(broken, 1);
}

#[test]
fn other_layouts_are_refused_and_left_as_they_are() {
    for (version, says) in [
        (4, "older version of relevo"),
        (9, "newer version of relevo"),
    ] {
        let dir = TempDir::new();
        let path = dir.db();
        layout5(&path);
        rusqlite::Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", version)
            .unwrap();
        for open in [db::open, db::open_migrating] {
            let err = format!("{:#}", open(&path).unwrap_err());
            assert!(err.contains(says), "{err}");
        }
        let err = format!("{:#}", db::check(&path).unwrap_err());
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

/// The files of the database at `path` (with its `-wal` and `-shm`), with their contents.
fn files(path: &str) -> Vec<(String, Option<Vec<u8>>)> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|ext| (ext.to_string(), std::fs::read(format!("{path}{ext}")).ok()))
        .collect()
}

#[test]
fn only_serve_migrates_and_checking_writes_nothing() {
    let dir = TempDir::new();
    for (version, make, made_by) in [
        (5, layout5 as fn(&str) -> i64, "relevo 0.1.0 to 0.2.x"),
        (6, layout6, "relevo 0.3.x"),
        (7, layout7, "relevo 0.4.x"),
    ] {
        let path = dir.0.join(format!("{version}.db"));
        let path = path.to_str().unwrap();
        make(path);
        // As the server leaves it once stopped: no -wal, no -shm.
        rusqlite::Connection::open(path)
            .unwrap()
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let before = files(path);
        assert_eq!(before[1].1, None);
        assert_eq!(before[2].1, None);

        // Checking says serve migrates it, and changes nothing, leaving no -wal or -shm behind.
        let layout = db::check(path).unwrap();
        assert_eq!(layout, db::Layout::Migrates(version));
        assert_eq!(layout.made_by(), made_by);
        assert_eq!(files(path), before);
        // An admin command refuses it, says what to do, and changes nothing.
        let err = format!("{:#}", db::open(path).unwrap_err());
        for says in [
            "only `relevo serve`",
            "relevo backup",
            "start the server",
            made_by,
        ] {
            assert!(err.contains(says), "{says}: {err}");
        }
        assert_eq!(user_version(path), version);
        assert_eq!(files(path)[0], before[0]);

        // The server migrates it; then everything opens it, and checking says so, also while it
        // is open (read-only, through its -wal and -shm).
        let conn = db::open_migrating(path).unwrap();
        assert_eq!(user_version(path), 8);
        assert_eq!(db::check(path).unwrap(), db::Layout::Current);
        db::open(path).unwrap();
        drop(conn);
        assert_eq!(db::check(path).unwrap(), db::Layout::Current);
    }

    // A new file: empty. No file: an error, and none is made.
    let empty = dir.0.join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(db::check(empty.to_str().unwrap()).unwrap(), db::Layout::New);
    let missing = dir.0.join("missing.db");
    let missing = missing.to_str().unwrap();
    assert!(format!("{:#}", db::check(missing).unwrap_err()).contains("no database"));
    assert!(!std::path::Path::new(missing).exists());
}

/// What `api::refresh` keeps on each participant row, and on each session's: none of it may
/// change when layout 6 becomes 7, nor, outside the threads that were closed, when it becomes 8.
/// (`resolved`, unused since layout 8, is left out.)
fn flags(path: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut out: Vec<String> = conn
        .prepare(
            "SELECT thread_id || ' ' || person_id || ': ' || coalesce(delivered_seq, '-') || ' '
                    || last_seq || ' ' || last_other_seq || ' ' || has_new || waiting || inbox
                    || fresh || ' ' || coalesce(handled_seq, '-') || ' '
                    || coalesce(handled_at, '-') || ' ' || coalesce(claimed_by, '-')
             FROM participants ORDER BY thread_id, person_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let sessions: Vec<String> = conn
        .prepare(
            "SELECT session_id || ' ' || thread_id || ': ' || delivered_seq || ' ' || has_new
                    || waiting || person_inbox || ' ' || coalesce(claimed_by, '-')
             FROM session_threads ORDER BY session_id, thread_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    out.extend(sessions);
    out
}

#[tokio::test]
async fn a_layout_6_database_migrates_its_team_questions_to_the_global_board() {
    let dir = TempDir::new();
    let path = dir.db();
    let seen = layout6(&path);
    // Layout 8 marks what was closed as reached by everyone: those rows move, on purpose.
    let open = |rows: Vec<String>| -> Vec<String> {
        let closed = serde_json::from_str::<Value>(FIXTURE6).unwrap()["threads"]["resolved"]
            .as_str()
            .unwrap()
            .to_string();
        rows.into_iter().filter(|r| !r.contains(&closed)).collect()
    };
    let before = open(flags(&path));
    let app = App::new(
        db::open_migrating(&path).unwrap(),
        Clock::fixed(seen + 60),
        5,
    );
    assert_eq!(user_version(&path), 8);
    // Delivery, handled and claims stay as they were, and so does what is pending.
    assert_eq!(open(flags(&path)), before);
    let f = Fixture {
        app,
        v: serde_json::from_str(FIXTURE6).unwrap(),
    };
    let ana = f.agent("ana", "ana-cc-0001", "pagos-api");
    let bob = f.agent("bob", "bob-term-01", "pagos-api");
    let carl = f.agent("carl", "carl-hermes1", "");

    // Team questions were questions to everyone: global threads now, naming nobody, so answering
    // them stays optional; direct threads name everyone in them.
    for name in ["team", "unread_team", "resolved"] {
        let th = ana.thread(&f.thread(name)).await;
        assert_eq!(th["kind"], "global", "{name}");
        assert_eq!(th["teams"], json!([]), "{name}");
        assert_eq!(th["named"], false, "{name}");
        assert_eq!(th["pending"], false, "{name}");
        assert_eq!(th["participants"].as_array().unwrap().len(), 3, "{name}");
        assert!(
            th["delivery"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["named"] == false),
            "{th}"
        );
    }
    for name in ["fetched", "replied", "done", "claimed", "unread"] {
        let th = bob.thread(&f.thread(name)).await;
        assert_eq!(th["kind"], "direct", "{name}");
        assert_eq!(th["named"], true, "{name}");
    }

    // Bob's inbox, as before: what waited for him still does, and the question to everyone that
    // reached him asks nothing of him. The one Ana closed is open now, and reached him whole: her
    // last note, which he never fetched, does not come back.
    let v = bob.inbox().await;
    let mut pending: Vec<&str> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["pending"] == true)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    pending.sort();
    let mut expected = vec![f.thread("fetched"), f.thread("claimed"), f.thread("unread")];
    expected.sort();
    assert_eq!(pending, expected, "{v}");
    assert!(listed(&v, &f.thread("unread_team")).is_none(), "{v}");
    assert!(listed(&v, &f.thread("resolved")).is_none(), "{v}");
    assert!(listed(&v, &f.thread("done")).is_none(), "{v}");
    assert_eq!(v["teams"], json!([]));
    assert_eq!(v["team"][0]["teams"], json!([]));

    // A key used before the migration replays its first result, told as the thread is now.
    let replay = ana
        .send(json!({ "to": ["bob"], "text": "Fetched, not answered", "key": "k-fetched" }))
        .await;
    assert_eq!(replay["thread"], f.thread("fetched"));
    assert_eq!(replay["kind"], "direct");
    assert_eq!(replay["reached"], 1);
    assert_eq!(replay["added"], json!([]));

    // Anyone in her question to everyone may close it now, which closes nothing.
    for who in [&carl, &ana] {
        let v = who
            .send(json!({ "thread": f.thread("team"), "close": true }))
            .await;
        assert_eq!(v["status"], "open");
    }

    // And the new layout works on it: a team, and a note to it.
    {
        let conn = f.app.db();
        db::add_team(&conn, "billing", Some("Billing"), seen).unwrap();
        db::join_team(&conn, "billing", &["carl".to_string()]).unwrap();
    }
    let sent = ana
        .send(json!({ "text": "@billing Is the invoice run late?" }))
        .await;
    assert_eq!(sent["kind"], "team");
    assert_eq!(sent["to"], json!({ "people": [], "teams": ["billing"] }));
    assert_eq!(sent["reached"], 1);
    let v = carl.inbox().await;
    let th = listed(&v, sent["thread"].as_str().unwrap()).unwrap();
    assert_eq!(th["teams"], json!(["billing"]));
    assert_eq!(th["pending"], false);
}

#[tokio::test]
async fn a_layout_7_database_migrates_and_its_closed_threads_open_with_nothing_pending() {
    let dir = TempDir::new();
    let path = dir.db();
    let seen = layout7(&path);
    let waiting = |path: &str| -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM participants WHERE waiting = 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = waiting(&path);
    // Notes nobody fetched in what was closed: Bob, reached through the team and not named, never
    // fetched Ana's last note there, nor Ana Bob's answer in her direct thread.
    let unfetched = |path: &str| -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM participants p JOIN threads t ON t.id = p.thread_id
                 WHERE t.status = 'closed' AND coalesce(p.delivered_seq, 0) < p.last_other_seq",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(unfetched(&path) >= 2);
    let app = App::new(
        db::open_migrating(&path).unwrap(),
        Clock::fixed(seen + 60),
        5,
    );
    assert_eq!(user_version(&path), 8);
    // Nothing became pending, and no thread is closed.
    assert_eq!(waiting(&path), before);
    let closed: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM threads WHERE status != 'open'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(closed, 0);
    let f = Fixture {
        app,
        v: serde_json::from_str(FIXTURE7).unwrap(),
    };
    let ana = f.agent("ana", "ana-cc-0001", "pagos-api");
    let bob = f.agent("bob", "bob-term-01", "pagos-api");
    let carl = f.agent("carl", "carl-hermes1", "");
    let closed_direct = f.thread("closed_direct");
    let closed_team = f.thread("closed_team");
    let closed_board = f.thread("closed_board");

    // Nothing of what was closed comes back as new: not to Bob's session, which never fetched the
    // last note of the team thread that does not name him, nor to Ana's, nor to a new session.
    for (who, agent) in [("bob", &bob), ("ana", &ana), ("carl", &carl)] {
        let v = agent.inbox().await;
        for name in ["closed_direct", "closed_team", "closed_board"] {
            assert!(listed(&v, &f.thread(name)).is_none(), "{who} {name}: {v}");
        }
        let v = f.agent(who, "after-upgrade", "").inbox().await;
        for name in ["closed_direct", "closed_team", "closed_board"] {
            assert!(listed(&v, &f.thread(name)).is_none(), "{who} {name}: {v}");
        }
    }
    // Bob's delivery says the note reached him, from no known session.
    let bob_d = delivery_of(&ana.thread(&closed_team).await, "bob");
    assert_eq!(
        bob_d["note"],
        ana.thread(&closed_team).await["notes"][1]["id"]
    );
    assert_eq!(bob_d["by"], Value::Null);
    assert_eq!(bob_d["named"], false);

    // Each closed thread is open, and pending for nobody it names.
    for (name, who) in [
        ("closed_direct", &ana),
        ("closed_direct", &bob),
        ("closed_team", &ana),
        ("closed_team", &bob),
        ("closed_team", &carl),
        ("closed_board", &bob),
        ("closed_board", &carl),
    ] {
        let th = who.thread(&f.thread(name)).await;
        assert_eq!(th["status"], "open", "{name}");
        assert_eq!(th["pending"], false, "{name}");
    }
    // Ana closed her direct thread before Bob's answer reached her: she has it handled, as of
    // when she closed it, from no known session.
    let th = bob.thread(&closed_direct).await;
    let ana_d = delivery_of(&th, "ana");
    assert!(ana_d["handled_at"].is_string(), "{ana_d}");
    assert_eq!(ana_d["handled_by"], Value::Null);
    // Carl, named in the team thread and never reached: handled too.
    let carl_d = delivery_of(&ana.thread(&closed_team).await, "carl");
    assert!(carl_d["handled_at"].is_string(), "{carl_d}");

    // What waited for Bob still does, and nothing else does for anyone.
    let pending = |v: &Value| -> Vec<String> {
        let mut ids: Vec<String> = v["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["pending"] == true)
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let mut expected = vec![f.thread("fetched"), f.thread("unread")];
    expected.sort();
    assert_eq!(pending(&bob.inbox().await), expected);
    assert_eq!(pending(&ana.inbox().await), Vec::<String>::new());
    assert_eq!(pending(&carl.inbox().await), Vec::<String>::new());
    // Nor for a new session of anyone's.
    for who in ["ana", "carl"] {
        let v = f.agent(who, "new-session-1", "").inbox().await;
        assert_eq!(pending(&v), Vec::<String>::new(), "{who}");
    }
    // A key used to close replays as open.
    let v = ana
        .send(json!({ "thread": closed_direct, "close": true, "key": "k-close" }))
        .await;
    assert_eq!(v["status"], "open");
    assert_eq!(v["note"], Value::Null);

    // Writing works in every one of them: a note makes it pending for whom it names.
    let v = bob
        .send(json!({ "thread": closed_direct, "text": "Done, it is out." }))
        .await;
    assert_eq!(v["status"], "open");
    assert_eq!(
        listed(&ana.inbox().await, &closed_direct).unwrap()["pending"],
        true
    );
    ana.send(json!({ "thread": closed_direct, "seen": true }))
        .await;
    assert!(listed(&ana.inbox().await, &closed_direct).is_none());
    ana.send(json!({ "thread": closed_team, "text": "Down again." }))
        .await;
    let v = carl.inbox().await;
    assert_eq!(listed(&v, &closed_team).unwrap()["pending"], true);
    // Bob, reached through the team, is not named: new for him, not pending.
    let v = bob.inbox().await;
    assert_eq!(listed(&v, &closed_team).unwrap()["pending"], false);
    bob.send(json!({ "thread": closed_board, "text": "Still me tonight." }))
        .await;
    assert_eq!(
        bob.thread(&closed_board).await["notes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}
