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

#[test]
fn only_serve_migrates_a_layout_5_6_or_7_database_and_check_db_says_so() {
    for (dump, made_by) in [
        (
            include_str!("fixtures/layout5.sql"),
            "relevo 0.1.0 to 0.2.x",
        ),
        (include_str!("fixtures/layout6.sql"), "relevo 0.3.x"),
        (include_str!("fixtures/layout7.sql"), "relevo 0.4.x"),
    ] {
        let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("relevo.db");
        let path = path.to_str().unwrap();
        rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(dump)
            .unwrap();
        let before = std::fs::read(path).unwrap();
        let run = |args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_relevo"))
                .arg("--db")
                .arg(path)
                .args(args)
                .output()
                .unwrap()
        };
        let out = run(&["check-db"]);
        assert!(out.status.success());
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            said.contains("`relevo serve` migrates it in place") && said.contains(made_by),
            "{said}"
        );
        for args in [
            &["people"][..],
            &["invite", "dan", "--name", "Dan"][..],
            &["team", "add", "backend"][..],
            &["teams"][..],
        ] {
            let out = run(args);
            assert!(!out.status.success(), "{args:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("only `relevo serve`")
                    && err.contains("relevo backup")
                    && err.contains(made_by),
                "{err}"
            );
        }
        // Nothing changed, and a backup of it can still be taken.
        assert_eq!(std::fs::read(path).unwrap(), before);
        let copy = dir.join("copy.db");
        relevo(path, &["backup", copy.to_str().unwrap()]);
        // A newer layout: refused, with exit 1.
        rusqlite::Connection::open(path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        let out = run(&["check-db"]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("newer version of relevo"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn teams_are_kept_from_the_cli() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    db::open(path).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_relevo"))
            .arg("--db")
            .arg(path)
            .args(args)
            .output()
            .unwrap()
    };
    let fails = |args: &[&str], says: &str| {
        let out = run(args);
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(says), "{args:?}: {err}");
    };
    for h in ["ana", "bob", "old"] {
        relevo(path, &["invite", h, "--name", h]);
    }
    relevo(path, &["deactivate", "old"]);
    let said = relevo(path, &["team", "add", "backend", "--title", "Backend"]);
    assert!(said.contains("added team backend"), "{said}");
    relevo(path, &["team", "add", "front"]);
    // A team takes no handle, active or not, nor `team` or `equipo`; a handle takes no team's
    // name.
    fails(&["team", "add", "ana"], "handle of a person");
    fails(&["team", "add", "old"], "handle of a person (inactive)");
    fails(&["team", "add", "team"], "reserved");
    fails(&["team", "add", "equipo"], "reserved");
    fails(&["team", "add", "backend"], "already");
    fails(&["invite", "backend", "--name", "B"], "name of a team");
    // Joining: active people only, nothing changed otherwise; twice is said, not an error.
    let said = relevo(path, &["team", "join", "backend", "ana", "bob"]);
    assert_eq!(said, "ana joined backend\nbob joined backend\n");
    let said = relevo(path, &["team", "join", "backend", "ana"]);
    assert_eq!(said, "ana is in backend already\n");
    fails(&["team", "join", "front", "ana", "old"], "deactivated");
    fails(&["team", "join", "front", "ana", "zed"], "no person");
    fails(&["team", "join", "nope", "ana"], "no team named 'nope'");
    fails(&["team", "join", "front"], "required");
    relevo(path, &["deactivate", "bob"]);
    let said = relevo(path, &["teams"]);
    assert_eq!(said, "backend\tBackend\tana, bob (inactive)\nfront\t\t\n");
    let said = relevo(path, &["team", "leave", "backend", "bob", "old"]);
    assert_eq!(said, "bob left backend\nold was not in backend\n");
    let said = relevo(path, &["team", "remove", "backend"]);
    assert!(said.contains("removed team backend"), "{said}");
    assert_eq!(relevo(path, &["teams"]), "front\t\t\n");
    // The name is free again, for a team or a person.
    relevo(path, &["invite", "backend", "--name", "B"]);
    std::fs::remove_dir_all(&dir).unwrap();
}
