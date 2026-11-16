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

#[tokio::test]
async fn clients_lists_each_sessions_client_against_the_servers_version_outdated_first() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    let now = db::now_unix();
    let server = env!("CARGO_PKG_VERSION");
    let conn = db::open(path).unwrap();
    let ana = db::invite(&conn, "ana", "Ana", None, "Claude Code", now).unwrap();
    let bob = db::invite(&conn, "bob", "Bob", None, "pi", now).unwrap();
    let carl = db::invite(&conn, "carl", "Carl", None, "Hermes", now).unwrap();
    let app = App::new(conn, Clock::system(), 5);
    let current = format!("pi/{server}");
    // (token, session, context, Relevo-Client, User-Agent), in the order they are seen.
    for (token, session, context, client, agent) in [
        (
            &bob.secret,
            Some("pi-term-01"),
            None,
            Some(current.as_str()),
            None,
        ),
        (
            &ana.secret,
            Some("mcp_new001"),
            Some("pagos-api"),
            Some("mcp/999.0.0"),
            None,
        ),
        (
            &carl.secret,
            Some("hermes-gw-1"),
            None,
            None,
            Some("python-httpx/0.28.1"),
        ),
        (&ana.secret, None, None, None, Some("relevo/0.0.1")),
        (
            &bob.secret,
            Some("pi-term-02"),
            None,
            Some("pi/0.0.2"),
            None,
        ),
    ] {
        let mut req = Request::get("/api/inbox").header("authorization", format!("Bearer {token}"));
        for (name, value) in [
            ("relevo-session", session),
            ("relevo-context", context),
            ("relevo-client", client),
            ("user-agent", agent),
            // One session says its agent (the last column), the others do not.
            (
                "relevo-agent",
                (session == Some("mcp_new001")).then_some("Gemini CLI"),
            ),
        ] {
            if let Some(value) = value {
                req = req.header(name, value);
            }
        }
        let res = api::router(app.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(res.status().is_success());
    }

    let out = relevo(path, &["clients"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 6, "{out}");
    let rows: Vec<Vec<&str>> = lines[..5].iter().map(|l| l.split('\t').collect()).collect();
    let without_time: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| {
            assert!(r[6].ends_with('Z'), "{out}");
            [&r[..6], &r[7..]].concat()
        })
        .collect();
    assert_eq!(
        without_time,
        vec![
            vec![
                "ana",
                "Claude Code",
                "default",
                "general",
                "mcp",
                "0.0.1?",
                "outdated",
                "-"
            ],
            vec![
                "bob",
                "pi",
                "pi-term-02",
                "general",
                "pi",
                "0.0.2",
                "outdated",
                "-"
            ],
            vec![
                "carl",
                "Hermes",
                "hermes-gw-1",
                "general",
                "unknown",
                "-",
                "unknown",
                "-"
            ],
            vec![
                "ana",
                "Claude Code",
                "mcp_new001",
                "pagos-api",
                "mcp",
                "999.0.0",
                "newer",
                "Gemini CLI"
            ],
            vec![
                "bob",
                "pi",
                "pi-term-01",
                "general",
                "pi",
                server,
                "current",
                "-"
            ],
        ],
        "{out}"
    );
    assert_eq!(
        lines[5],
        format!("1 session on {server}, 2 outdated, 1 unknown, 1 newer")
    );

    // Only what is outdated or unknown; the summary still counts them all.
    let out = relevo(path, &["clients", "--outdated"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert!(
        lines[..3]
            .iter()
            .all(|l| l.contains("\toutdated\t") || l.contains("\tunknown\t"))
    );
    assert_eq!(
        lines[3],
        format!("1 session on {server}, 2 outdated, 1 unknown, 1 newer")
    );
    // One person's.
    let out = relevo(path, &["clients", "bob"]);
    assert_eq!(out.lines().count(), 3, "{out}");
    assert!(
        out.ends_with(&format!("1 session on {server}, 1 outdated, 0 unknown\n")),
        "{out}"
    );

    // `sessions` says the client too, then the agent.
    let out = relevo(path, &["sessions", "ana"]);
    let mut clients: Vec<(&str, &str)> = out
        .lines()
        .map(|l| {
            let l: Vec<&str> = l.split('\t').collect();
            (l[5], l[6])
        })
        .collect();
    clients.sort();
    assert_eq!(
        clients,
        [("mcp/0.0.1?", "-"), ("mcp/999.0.0", "Gemini CLI")],
        "{out}"
    );
    let out = relevo(path, &["sessions", "carl"]);
    assert!(out.trim_end().ends_with("\tunknown\t-"), "{out}");
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
        &["invite", "ana", "--name", "Ana", "--token"][..],
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
        &["invite", "ana", "--name", "Ana", "--token"],
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
fn only_serve_migrates_a_layout_5_to_9_database_and_check_db_says_so() {
    for (dump, made_by) in [
        (
            include_str!("fixtures/layout5.sql"),
            "relevo 0.1.0 to 0.2.x",
        ),
        (include_str!("fixtures/layout6.sql"), "relevo 0.3.x"),
        (include_str!("fixtures/layout7.sql"), "relevo 0.4.x"),
        (
            include_str!("fixtures/layout8.sql"),
            "a development build of relevo 0.5.0",
        ),
        (include_str!("fixtures/layout9.sql"), "relevo 0.5.x"),
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
            &["invite", "dan", "--name", "Dan", "--token"][..],
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
        relevo(path, &["invite", h, "--name", h, "--token"]);
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
    fails(
        &["invite", "backend", "--name", "B", "--token"],
        "name of a team",
    );
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
    relevo(path, &["invite", "backend", "--name", "B", "--token"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn relevo_is_the_servers_own_name_and_in_no_list() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    {
        let conn = db::open(path).unwrap();
        db::invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('announced', '0.0.1')
             ON CONFLICT (key) DO UPDATE SET value = '0.0.1'",
            [],
        )
        .unwrap();
        let release = format!("## {} (1-jan-2026)\n\nNews.\n", env!("CARGO_PKG_VERSION"));
        relevo::release::announce(&conn, env!("CARGO_PKG_VERSION"), &release, true, 0).unwrap();
    }
    for args in [
        &["invite", "relevo", "--name", "Mine", "--token"][..],
        &["team", "add", "relevo"][..],
        &["token", "add", "relevo", "--label", "x"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_relevo"))
            .arg("--db")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("the server's own name"), "{args:?}: {err}");
    }
    assert!(!relevo(path, &["people"]).contains("relevo"));
    assert!(!relevo(path, &["clients"]).contains("relevo"));
    let out = Command::new(env!("CARGO_BIN_EXE_relevo"))
        .args(["--db", path, "clients", "relevo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("the server's own name"));
    assert!(!relevo(path, &["token", "list"]).contains("relevo"));
    // `serve` takes the switch.
    assert!(relevo(path, &["serve", "--help"]).contains("--no-release-notes"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `relevo serve` with the release notes off, by the switch or by RELEVO_RELEASE_NOTES, records
/// its version without posting; on, it posts. Either way only if this build is a release: one
/// between releases records nothing.
#[test]
fn serve_records_or_posts_the_release_notes_once_it_listens() {
    let version = env!("CARGO_PKG_VERSION");
    let released = relevo::release::release_section(relevo::release::CHANGELOG, version).is_some();
    for (flag, env, posts) in [
        (true, None, false),
        (false, Some("0"), false),
        (false, Some("off"), false),
        (false, None, released),
    ] {
        let dir = std::env::temp_dir().join(db::random_id("relevo-cli-"));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("relevo.db");
        let path = path.to_str().unwrap();
        {
            let conn = db::open(path).unwrap();
            db::invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('announced', '0.0.1')
             ON CONFLICT (key) DO UPDATE SET value = '0.0.1'",
                [],
            )
            .unwrap();
        }
        let log = dir.join("serve.log");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_relevo"));
        cmd.arg("--db")
            .arg(path)
            .args(["serve", "--addr", "127.0.0.1:0"])
            .env_remove("RELEVO_RELEASE_NOTES")
            .env("RUST_LOG", "info")
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::process::Stdio::null());
        if flag {
            cmd.arg("--no-release-notes");
        }
        if let Some(env) = env {
            cmd.env("RELEVO_RELEASE_NOTES", env);
        }
        let mut child = cmd.spawn().unwrap();
        // It says what it did once it listens.
        let said = (0..200)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let out = std::fs::read_to_string(&log).unwrap_or_default();
                (out.contains("relevo listening") && out.contains("release notes")).then_some(out)
            })
            .unwrap_or_else(|| std::fs::read_to_string(&log).unwrap_or_default());
        child.kill().unwrap();
        child.wait().unwrap();
        let listening = said.find("relevo listening").expect(&said);
        let release = said.find("release notes").expect(&said);
        assert!(listening < release, "{said}");
        let conn = rusqlite::Connection::open(path).unwrap();
        let announced: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let board: i64 = conn
            .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
            .unwrap();
        let case = format!("flag {flag}, env {env:?}");
        if !released {
            // Not a release: nothing recorded, nothing posted, the notes on or off.
            assert_eq!(announced, "0.0.1", "{case}");
            assert!(said.contains("not a release"), "{case}: {said}");
        } else {
            assert_eq!(announced, version, "{case}");
        }
        assert_eq!(board, i64::from(posts), "{case}");
        if released && (flag || env.is_some()) {
            assert!(said.contains("release notes are off"), "{case}: {said}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
