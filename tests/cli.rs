//! The admin CLI, run as the real binary against a database the server wrote.

use std::process::Command;

use axum::body::Body;
use axum::http::Request;
use relevo::{App, Clock, api, db};
use tower::ServiceExt;

fn relevo(db: &str, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_relevo"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn sessions_lists_label_session_context_and_last_seen() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    let now = db::now_unix();
    let conn = db::open(path).unwrap();
    let ana = db::invite(&conn, "ana", "Ana", None, "Claude Code", now).unwrap();
    let bob = db::invite(&conn, "bob", "Bob", None, "bot", now).unwrap();
    let app = App::new(conn, Clock::system(), 5);
    for (token, session, context) in [
        (&ana.secret, Some("term-0001"), Some("pagos-api")),
        (&ana.secret, None, None),
        (&bob.secret, None, None),
    ] {
        let mut req = Request::get("/api/inbox").header("authorization", format!("Bearer {token}"));
        if let Some(session) = session {
            req = req.header("relevo-session", session);
        }
        if let Some(context) = context {
            req = req.header("relevo-context", context);
        }
        let res = api::router(app.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(res.status().is_success());
    }

    let all = relevo(path, &["sessions"]);
    let lines: Vec<Vec<&str>> = all.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(lines.len(), 3, "{all}");
    let mut ana_rows: Vec<&[&str]> = lines[..2].iter().map(|l| &l[..4]).collect();
    ana_rows.sort();
    assert_eq!(
        ana_rows,
        vec![
            &["ana", "Claude Code", "default", "general"][..],
            &["ana", "Claude Code", "term-0001", "pagos-api"][..],
        ]
    );
    assert_eq!(lines[2][..4], ["bob", "bot", "default", "general"]);
    assert!(lines.iter().all(|l| l[4].ends_with('Z')), "{all}");

    let only_bob = relevo(path, &["sessions", "bob"]);
    assert_eq!(only_bob.lines().count(), 1, "{only_bob}");
    drop(app);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_database_from_an_older_block_is_refused_with_a_clear_message() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    // What block 1b left: a sessions table without `claims`, and no schema version.
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, token_id TEXT NOT NULL,
                 client_id TEXT NOT NULL, person_id INTEGER NOT NULL, context TEXT,
                 last_seen INTEGER NOT NULL, UNIQUE (token_id, client_id));",
        )
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_relevo"))
        .args(["--db", path, "people"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("older version of relevo"), "{err}");
    assert!(err.contains("delete it"), "{err}");

    // A fresh database opens, and opens again.
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir(&dir).unwrap();
    relevo(path, &["people"]);
    relevo(path, &["people"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_relevo_db_an_admin_command_never_creates_a_database_here() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_relevo"))
            .current_dir(&dir)
            .env_remove("RELEVO_DB")
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        &["invite", "ana", "--name", "Ana"][..],
        &["people"][..],
        &["backup", "copy.db"][..],
    ] {
        let out = run(args);
        assert!(!out.status.success(), "{args:?} succeeded");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("RELEVO_DB is not set"), "{err}");
        assert!(
            !dir.join("relevo.db").exists(),
            "{args:?} created relevo.db"
        );
    }

    // One that is already there is used, as before.
    relevo(
        dir.join("relevo.db").to_str().unwrap(),
        &["invite", "ana", "--name", "Ana"],
    );
    let out = run(&["people"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ana\tAna"));
    std::fs::remove_dir_all(&dir).unwrap();
}
