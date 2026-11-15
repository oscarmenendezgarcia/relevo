//! The timeline (docs/protocol.md, "timeline"): what `relevo web` reads, read only, through the
//! HTTP router on an in-memory database.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use tower::ServiceExt;

const T0: i64 = 1_794_134_460; // 2026-11-08T10:41:00Z

struct Server {
    app: Arc<App>,
    tokens: HashMap<String, String>,
}

/// A server with these people (name: the handle in capitals), each with one token, on a fixed
/// clock, and these teams (name, members).
fn server(handles: &[&str], teams: &[(&str, &[&str])]) -> Server {
    let app = App::new(db::open(":memory:").unwrap(), Clock::fixed(T0), 50);
    let mut tokens = HashMap::new();
    {
        let conn = app.db();
        for h in handles {
            let token = db::invite(&conn, h, &h.to_uppercase(), None, "laptop", T0).unwrap();
            tokens.insert(h.to_string(), token.secret);
        }
        for (name, members) in teams {
            db::add_team(&conn, name, None, T0).unwrap();
            let members: Vec<String> = members.iter().map(|m| m.to_string()).collect();
            db::join_team(&conn, name, &members).unwrap();
        }
    }
    Server { app, tokens }
}

async fn call(
    app: &Arc<App>,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = api::router(app.clone())
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

impl Server {
    /// `send` from `who`'s session `session` in `context` (`""`: general).
    async fn send_in(&self, who: &str, context: &str, body: Value) -> Value {
        let mut headers = vec![("relevo-mentions", "1"), ("relevo-session", "agent-one")];
        if !context.is_empty() {
            headers.push(("relevo-context", context));
        }
        let (status, v) = call(
            &self.app,
            "POST",
            "/api/send",
            Some(&self.tokens[who]),
            &headers,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn send(&self, who: &str, body: Value) -> Value {
        self.send_in(who, "", body).await
    }

    async fn open(&self, who: &str, text: &str) -> String {
        self.send(who, json!({ "text": text })).await["thread"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn reply(&self, who: &str, thread: &str, text: &str) {
        self.send(who, json!({ "thread": thread, "text": text }))
            .await;
    }

    async fn get(&self, who: &str, uri: &str) -> (StatusCode, Value) {
        call(&self.app, "GET", uri, Some(&self.tokens[who]), &[], None).await
    }

    async fn timeline(&self, who: &str, query: &str) -> Value {
        let (status, v) = self.get(who, &format!("/api/timeline?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {v}");
        v
    }

    async fn bad(&self, who: &str, query: &str) -> String {
        let (status, v) = self.get(who, &format!("/api/timeline?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {v}");
        assert_eq!(v["error"]["code"], "BAD_REQUEST", "{v}");
        v["error"]["message"].as_str().unwrap().to_string()
    }

    /// Every row of every table, to compare before and after reading.
    fn snapshot(&self) -> Vec<String> {
        let conn = self.app.db();
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
            let rows = stmt
                .query_map([], |r| {
                    let cells: Vec<String> = (0..n)
                        .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                        .collect();
                    Ok(format!("{table}: {}", cells.join(" | ")))
                })
                .unwrap();
            for row in rows {
                out.push(row.unwrap());
            }
        }
        out
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

/// The threads of the scenario most tests read, as Ana sees them.
struct Scene {
    s: Server,
    /// Bob to Ana: pending for her.
    asked: String,
    /// Ana to Bob, answered by Bob: pending for her.
    answered_to_her: String,
    /// Carol to @backend, which Ana is in, not naming her; Ana wrote in it.
    team_q: String,
    /// Bob to Carol: not Ana's.
    bob_carol: String,
    /// Carol to @front, which Ana is not in.
    front: String,
    /// Dan on the board; Bob's reply names Ana.
    board: String,
    /// Ana to Bob, answered by Bob with a 0.4 close (which closes nothing), then marked seen by
    /// Ana: handled.
    seen_direct: String,
    /// Carol to @backend, which she marked seen with a 0.4 close: open, pending for nobody.
    seen_team: String,
}

async fn scene() -> Scene {
    let s = server(
        &["ana", "bob", "carol", "dan"],
        &[("backend", &["ana", "bob"]), ("front", &["carol", "dan"])],
    );
    let seen_direct = s.open("ana", "@bob old question").await;
    s.reply("bob", &seen_direct, "old answer").await;
    s.send(
        "bob",
        json!({ "thread": seen_direct, "close": true, "text": "closing" }),
    )
    .await;
    // Ana reads it, then marks it seen: that handles it.
    call(
        &s.app,
        "GET",
        &format!("/api/threads/{seen_direct}"),
        Some(&s.tokens["ana"]),
        &[("relevo-session", "agent-one")],
        None,
    )
    .await;
    s.send("ana", json!({ "thread": seen_direct, "seen": true }))
        .await;
    let seen_team = s.open("carol", "@backend anyone? (seen)").await;
    s.send("carol", json!({ "thread": seen_team, "close": true }))
        .await;
    let front = s.open("carol", "@front front only").await;
    let bob_carol = s.open("bob", "@carol between us").await;
    let board = s.open("dan", "The VPN is down tomorrow").await;
    s.reply("bob", &board, "@ana does it clash with your demo?")
        .await;
    let team_q = s.open("carol", "@backend who owns the banner?").await;
    // Ana's agent reads it, then answers: that handles it.
    call(
        &s.app,
        "GET",
        &format!("/api/threads/{team_q}"),
        Some(&s.tokens["ana"]),
        &[
            ("relevo-session", "agent-one"),
            ("relevo-context", "web@main"),
        ],
        None,
    )
    .await;
    s.send_in(
        "ana",
        "web@main",
        json!({ "thread": team_q, "text": "It is in `web/src/banner.ts`" }),
    )
    .await;
    let answered_to_her = s.open("ana", "@bob is embeddings in production?").await;
    s.reply("bob", &answered_to_her, "Yes, since October").await;
    let asked = s.open("bob", "@ana can you review the contract?").await;
    Scene {
        s,
        asked,
        answered_to_her,
        team_q,
        bob_carol,
        front,
        board,
        seen_direct,
        seen_team,
    }
}

#[tokio::test]
async fn the_board_lists_the_global_threads_and_threads_what_is_for_the_person() {
    let sc = scene().await;
    let v = sc.s.timeline("ana", "tab=board").await;
    assert_eq!(v["you"], "ana");
    assert_eq!(v["teams"], json!(["backend"]));
    assert_eq!(ids(&v), vec![sc.board.clone()]);
    let t = &v["threads"][0];
    assert_eq!(t["kind"], "global");
    assert_eq!(t["from"], "dan");
    // A global thread does not list everyone.
    assert_eq!(t["participants"], json!([]));
    assert_eq!(t["teams"], json!([]));
    assert_eq!(t["notes"], 2);
    assert_eq!(t["named"], true);
    assert_eq!(t["pending"], true);
    assert_eq!(t["first_note"]["author"], "dan");
    assert_eq!(t["first_note"]["text"], "The VPN is down tomorrow");
    assert_eq!(t["last_note"]["author"], "bob");
    assert_eq!(t["handled_by_you"], false);
    assert_eq!(t["status"], "open");
    assert!(t.get("closed_by").is_none(), "{t}");
    assert_eq!(v["cursor"], Value::Null);
    assert_eq!(v["names"]["dan"], "DAN");
    assert_eq!(v["names"]["bob"], "BOB");
    assert_eq!(v["names"]["ana"], "ANA");

    let v = sc.s.timeline("ana", "tab=threads").await;
    // Newest first, by latest note; never Bob and Carol's direct thread, nor @front's. The board
    // thread is in it too: Bob's reply names Ana.
    assert_eq!(
        ids(&v),
        vec![
            sc.asked.clone(),
            sc.answered_to_her.clone(),
            sc.team_q.clone(),
            sc.board.clone(),
            sc.seen_team.clone(),
            sc.seen_direct.clone(),
        ]
    );
    // Every thread is open; none says who closed it.
    for t in v["threads"].as_array().unwrap() {
        assert_eq!(t["status"], "open", "{t}");
        assert!(t.get("closed_by").is_none(), "{t}");
    }
    assert_eq!(v["threads"][5]["handled_by_you"], true);
    let first = &v["threads"][0];
    assert_eq!(first["kind"], "direct");
    assert_eq!(first["participants"], json!(["bob", "ana"]));
    assert_eq!(first["last_note"], Value::Null, "one note: no last_note");
    assert_eq!(first["notes"], 1);
    let team = &v["threads"][2];
    assert_eq!(team["kind"], "team");
    assert_eq!(team["teams"], json!(["backend"]));
    assert_eq!(team["participants"], json!(["carol", "ana", "bob"]));
    assert_eq!(team["named"], false);
    assert_eq!(team["pending"], false);
    assert_eq!(team["last_note"]["author"], "ana");
    assert_eq!(team["last_note"]["context"], "web@main");
    assert_eq!(team["first_note"]["context"], Value::Null);
    assert_eq!(team["first_note"]["via"], "laptop");
    assert_eq!(team["handled_by_you"], true);
    // The people Ana shares a direct or team thread with, and every name the answer uses.
    assert_eq!(v["people"], json!(["bob", "carol"]));
    assert_eq!(v["names"]["carol"], "CAROL");
    assert_eq!(v["names"]["dan"], "DAN");

    // A board thread that does not name the person is not for them: not Carol's, nor its
    // opener's.
    for who in ["carol", "dan"] {
        let v = sc.s.timeline(who, "tab=threads").await;
        assert!(!ids(&v).contains(&sc.board), "{who}");
    }
}

#[tokio::test]
async fn show_team_and_with_filter_the_threads_tab() {
    let sc = scene().await;
    let s = &sc.s;
    let v = s.timeline("ana", "tab=threads&show=waiting").await;
    assert_eq!(
        ids(&v),
        vec![
            sc.asked.clone(),
            sc.answered_to_her.clone(),
            sc.board.clone()
        ]
    );
    // Opened by someone else, and Ana wrote in it.
    let v = s.timeline("ana", "tab=threads&show=answered").await;
    assert_eq!(ids(&v), vec![sc.team_q.clone()]);
    // Accepted for pages of 0.4.x, it lists nothing: threads no longer close.
    let v = s.timeline("ana", "tab=threads&show=closed").await;
    assert_eq!(ids(&v), Vec::<String>::new());
    assert_eq!(v["cursor"], Value::Null);
    let v = s.timeline("ana", "tab=threads&show=all").await;
    assert_eq!(ids(&v).len(), 6);

    let v = s.timeline("ana", "tab=threads&team=backend").await;
    assert_eq!(ids(&v), vec![sc.team_q.clone(), sc.seen_team.clone()]);
    // A team Ana is not in: only threads she takes part in, so none.
    let v = s.timeline("ana", "tab=threads&team=front").await;
    assert_eq!(ids(&v), Vec::<String>::new());
    let v = s
        .timeline("ana", "tab=threads&team=backend&show=closed")
        .await;
    assert_eq!(ids(&v), Vec::<String>::new());

    // With Carol: the threads Carol is in (everyone is in a board thread), never Bob and Carol's
    // direct thread.
    let v = s.timeline("ana", "tab=threads&with=carol").await;
    assert_eq!(
        ids(&v),
        vec![sc.team_q.clone(), sc.board.clone(), sc.seen_team.clone()]
    );
    let v = s.timeline("ana", "tab=threads&with=BOB&show=waiting").await;
    assert_eq!(
        ids(&v),
        vec![
            sc.asked.clone(),
            sc.answered_to_her.clone(),
            sc.board.clone()
        ]
    );
    // Dan shares no direct or team thread with Ana: only the board thread.
    let v = s.timeline("ana", "tab=threads&with=dan").await;
    assert_eq!(ids(&v), vec![sc.board.clone()]);
    // Nor does the board answer for Bob and Carol.
    let v = s.timeline("carol", "tab=threads&with=bob").await;
    assert!(ids(&v).contains(&sc.bob_carol));
    assert!(ids(&v).contains(&sc.team_q));
}

#[tokio::test]
async fn counts_count_what_each_tab_show_and_team_would_list() {
    let sc = scene().await;
    let v = sc.s.timeline("ana", "tab=board").await;
    // The board thread names Ana: it counts in `threads`, and in `waiting`, pending for her.
    assert_eq!(v["threads"][0]["pending"], true);
    assert_eq!(
        v["counts"],
        json!({ "board": 1, "threads": 6, "waiting": 3, "answered": 1,
                "teams": { "backend": 2 } })
    );
    // The same whatever the filters.
    let v =
        sc.s.timeline("ana", "tab=threads&show=closed&with=carol&limit=1")
            .await;
    assert_eq!(v["counts"]["threads"], 6);
    assert_eq!(v["counts"]["waiting"], 3);

    // Carol: her team threads, @front included; a team with no thread counts 0.
    let v = sc.s.timeline("carol", "tab=threads").await;
    assert_eq!(
        v["counts"],
        json!({ "board": 1, "threads": 4, "waiting": 1, "answered": 0,
                "teams": { "front": 1 } })
    );
    let v = sc.s.timeline("dan", "tab=board").await;
    assert_eq!(v["counts"]["teams"], json!({ "front": 1 }));
    assert_eq!(v["counts"]["threads"], 1);
    // Bob's board thread does not name Dan: nothing waits for him there.
    assert_eq!(v["counts"]["waiting"], 0);
}

/// `last_other_note` is the latest note by someone else: the answer, even when the person wrote
/// after it. Null when nobody else wrote, or when it is the first note.
#[tokio::test]
async fn last_other_note_is_the_latest_by_someone_else() {
    let s = server(&["ana", "bob"], &[]);
    let asked = s.open("ana", "@bob is embeddings in production?").await;
    s.reply("bob", &asked, "Yes, since October").await;
    s.reply("ana", &asked, "Thanks, closing it.").await;
    let alone = s.open("ana", "@bob anyone there?").await;
    let theirs = s.open("bob", "@ana can you review the contract?").await;
    s.reply("ana", &theirs, "Done").await;
    let find = |v: &Value, id: &str| {
        v["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };

    let v = s.timeline("ana", "tab=threads").await;
    let t = find(&v, &asked);
    assert_eq!(t["last_note"]["text"], "Thanks, closing it.");
    assert_eq!(t["last_other_note"]["author"], "bob");
    assert_eq!(t["last_other_note"]["text"], "Yes, since October");
    assert_eq!(find(&v, &alone)["last_other_note"], Value::Null);
    // Bob's note is the first one, already in `first_note`.
    assert_eq!(find(&v, &theirs)["last_other_note"], Value::Null);

    let v = s.timeline("bob", "tab=threads").await;
    assert_eq!(
        find(&v, &asked)["last_other_note"]["text"],
        "Thanks, closing it."
    );
    assert_eq!(find(&v, &theirs)["last_other_note"]["text"], "Done");
    // One thread in full has every note instead.
    let (_, t) = s
        .get("ana", &format!("/api/timeline/threads/{asked}"))
        .await;
    assert!(t.get("last_other_note").is_none(), "{t}");
}

/// What the page shows next to each filter is what clicking it lists, for everyone.
#[tokio::test]
async fn every_count_is_the_length_of_its_list() {
    let sc = scene().await;
    for who in ["ana", "bob", "carol", "dan"] {
        let v = sc.s.timeline(who, "tab=board").await;
        let counts = v["counts"].clone();
        let len = |v: &Value| v["threads"].as_array().unwrap().len() as i64;
        assert_eq!(counts["board"], len(&v), "{who} board");
        for (count, query) in [
            ("threads", "tab=threads&limit=50"),
            ("waiting", "tab=threads&show=waiting&limit=50"),
            ("answered", "tab=threads&show=answered&limit=50"),
        ] {
            let v = sc.s.timeline(who, query).await;
            assert_eq!(counts[count], len(&v), "{who} {query}");
        }
        // No count for `show=closed`, which lists nothing.
        assert!(counts.get("closed").is_none(), "{counts}");
        let v = sc.s.timeline(who, "tab=threads&show=closed&limit=50").await;
        assert_eq!(len(&v), 0, "{who}");
        for (team, n) in counts["teams"].as_object().unwrap() {
            let v =
                sc.s.timeline(who, &format!("tab=threads&team={team}&limit=50"))
                    .await;
            assert_eq!(*n, len(&v), "{who} {team}");
        }
    }
}

#[tokio::test]
async fn pages_follow_the_cursor_to_the_end() {
    let sc = scene().await;
    let s = &sc.s;
    let all = ids(&s.timeline("ana", "tab=threads").await);
    let mut seen = Vec::new();
    let mut query = "tab=threads&limit=2".to_string();
    loop {
        let v = s.timeline("ana", &query).await;
        let page = ids(&v);
        assert!(page.len() <= 2);
        seen.extend(page);
        match v["cursor"].as_str() {
            Some(cursor) => {
                assert_eq!(v["threads"][1]["cursor"], cursor);
                query = format!("tab=threads&limit=2&before={cursor}");
            }
            None => break,
        }
    }
    assert_eq!(seen, all);
    // A page that ends exactly at the last thread says so.
    let v = s.timeline("ana", "tab=threads&limit=6").await;
    assert_eq!(ids(&v).len(), 6);
    assert_eq!(v["cursor"], Value::Null);
    let v = s.timeline("ana", "tab=threads&limit=5").await;
    assert!(v["cursor"].is_string());
    // A new note brings a thread back to the top.
    s.reply("bob", &sc.team_q, "late").await;
    let v = s.timeline("ana", "tab=threads&limit=1").await;
    assert_eq!(ids(&v), vec![sc.team_q.clone()]);
}

#[tokio::test]
async fn parameters_are_checked() {
    let sc = scene().await;
    let s = &sc.s;
    assert!(s.bad("ana", "").await.contains("'tab' is required"));
    assert!(s.bad("ana", "tab=mine").await.contains("board or threads"));
    assert!(s.bad("ana", "tab=board&tab=board").await.contains("twice"));
    assert!(s.bad("ana", "tab=threads&nope=1").await.contains("unknown"));
    for q in [
        "tab=board&show=all",
        "tab=board&team=backend",
        "tab=board&with=bob",
    ] {
        assert!(s.bad("ana", q).await.contains("tab=threads only"), "{q}");
    }
    assert!(
        s.bad("ana", "tab=threads&show=open")
            .await
            .contains("'show'")
    );
    assert!(
        s.bad("ana", "tab=threads&team=nope")
            .await
            .contains("no team")
    );
    assert!(
        s.bad("ana", "tab=threads&with=nobody")
            .await
            .contains("no person")
    );
    assert!(s.bad("ana", "tab=threads&team=").await.contains("empty"));
    for q in [
        "limit=0",
        "limit=51",
        "limit=x",
        "before=0",
        "before=x",
        "before=-3",
    ] {
        s.bad("ana", &format!("tab=threads&{q}")).await;
    }
    s.timeline("ana", "tab=threads&limit=50&before=1").await;
    s.timeline("ana", "tab=threads&limit=1").await;

    let (status, v) = call(&s.app, "GET", "/api/timeline?tab=board", None, &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    let (status, _) = call(
        &s.app,
        "GET",
        "/api/timeline?tab=board",
        Some("rlv_nope"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Only GET.
    let (status, v) = call(
        &s.app,
        "POST",
        "/api/timeline?tab=board",
        Some(&s.tokens["ana"]),
        &[],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
}

#[tokio::test]
async fn one_thread_in_full_only_for_its_participants() {
    let sc = scene().await;
    let s = &sc.s;
    let (status, t) = s
        .get("ana", &format!("/api/timeline/threads/{}", sc.team_q))
        .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["id"], sc.team_q.as_str());
    assert_eq!(t["kind"], "team");
    let notes = t["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["author"], "carol");
    assert_eq!(notes[0]["context"], Value::Null);
    assert_eq!(notes[1]["author"], "ana");
    assert_eq!(notes[1]["context"], "web@main");
    assert_eq!(notes[1]["truncated"], false);
    assert!(t.get("first_note").is_none() && t.get("last_note").is_none());
    assert_eq!(t["names"]["carol"], "CAROL");
    assert_eq!(t["names"]["ana"], "ANA");

    // The board thread: participants left out, the notes say enough.
    let (_, t) = s
        .get("ana", &format!("/api/timeline/threads/{}", sc.board))
        .await;
    assert_eq!(t["participants"], json!([]));
    assert_eq!(t["notes"].as_array().unwrap().len(), 2);

    // Never another person's direct thread, nor a team thread one is not in.
    for (who, id) in [
        ("ana", &sc.bob_carol),
        ("ana", &sc.front),
        ("dan", &sc.asked),
        ("dan", &sc.team_q),
    ] {
        let (status, v) = s.get(who, &format!("/api/timeline/threads/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{who} {id}: {v}");
        assert_eq!(v["error"]["code"], "NOT_FOUND");
    }
    for id in ["r_nope", "%ZZ", "a%2Fb"] {
        let (status, _) = s.get("ana", &format!("/api/timeline/threads/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }
    let (status, _) = call(
        &s.app,
        "GET",
        &format!("/api/timeline/threads/{}", sc.team_q),
        None,
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn first_and_last_notes_are_cut_to_1000_characters() {
    let s = server(&["ana", "bob"], &[]);
    let long = format!("@bob {}", "é".repeat(1500));
    let id = s.open("ana", &long).await;
    let v = s.timeline("bob", "tab=threads").await;
    let first = &v["threads"][0]["first_note"];
    assert_eq!(first["truncated"], true);
    assert_eq!(first["text"].as_str().unwrap().chars().count(), 1000);
    // Exactly 1,000 is whole.
    let exact = format!("@bob {}", "x".repeat(995));
    s.reply("ana", &id, &exact).await;
    let v = s.timeline("bob", "tab=threads").await;
    let last = &v["threads"][0]["last_note"];
    assert_eq!(last["truncated"], false);
    assert_eq!(last["text"].as_str().unwrap(), exact);
    // The thread itself has every note in full.
    let (_, t) = s.get("bob", &format!("/api/timeline/threads/{id}")).await;
    assert_eq!(t["notes"][0]["text"], long.as_str());
    assert_eq!(t["notes"][0]["truncated"], false);
}

#[tokio::test]
async fn handled_by_you_follows_the_latest_note_by_someone_else() {
    let s = server(&["ana", "bob"], &[]);
    let id = s.open("bob", "@ana a question").await;
    let handled = |v: &Value| v["threads"][0]["handled_by_you"].clone();
    let v = s.timeline("ana", "tab=threads").await;
    assert_eq!(handled(&v), false);
    assert_eq!(v["threads"][0]["pending"], true);
    // Seen through the inbox is not handled.
    call(
        &s.app,
        "GET",
        "/api/inbox",
        Some(&s.tokens["ana"]),
        &[("relevo-session", "agent-one")],
        None,
    )
    .await;
    let v = s.timeline("ana", "tab=threads").await;
    assert_eq!(handled(&v), false);
    s.send("ana", json!({ "thread": id, "done": true })).await;
    let v = s.timeline("ana", "tab=threads").await;
    assert_eq!(handled(&v), true);
    assert_eq!(v["threads"][0]["pending"], false);
    // Bob opened it and nobody else wrote: nothing for him to handle.
    let v = s.timeline("bob", "tab=threads").await;
    assert_eq!(handled(&v), false);
    // A 0.4 close by Ana is her `seen`: nothing closes, nothing changes for Bob.
    s.send("ana", json!({ "thread": id, "close": true })).await;
    let v = s.timeline("bob", "tab=threads").await;
    assert_eq!(v["threads"][0]["status"], "open");
    assert!(v["threads"][0].get("closed_by").is_none());
    // Bob writes again: it is Ana's turn again, and not handled by her.
    s.reply("bob", &id, "@ana and another thing").await;
    let v = s.timeline("ana", "tab=threads").await;
    assert_eq!(handled(&v), false);
    assert_eq!(v["threads"][0]["pending"], true);
}

/// Reading the timeline changes nothing at all: no delivery, nothing handled, no claim, no
/// session created or seen, whatever session headers the request carries (they are ignored,
/// even malformed ones).
#[tokio::test]
async fn reading_the_timeline_changes_nothing() {
    let sc = scene().await;
    let s = &sc.s;
    // A clock that moved: a request that counted as seen would record it.
    s.app.clock.advance(3600);
    let before = s.snapshot();
    let sessions: i64 = s
        .app
        .db()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    for who in ["ana", "bob", "carol", "dan"] {
        for headers in [
            vec![],
            vec![
                ("relevo-session", "web-session-1"),
                ("relevo-context", "web"),
            ],
            vec![
                ("relevo-session", "x"),
                ("relevo-context", "Not A Context!"),
            ],
            vec![("relevo-mentions", "2")],
        ] {
            for uri in [
                "/api/timeline?tab=board".to_string(),
                "/api/timeline?tab=threads".to_string(),
                "/api/timeline?tab=threads&show=waiting&limit=1".to_string(),
                format!("/api/timeline/threads/{}", sc.asked),
                format!("/api/timeline/threads/{}", sc.board),
                format!("/api/timeline/threads/{}", sc.team_q),
            ] {
                let (status, v) =
                    call(&s.app, "GET", &uri, Some(&s.tokens[who]), &headers, None).await;
                assert!(
                    status == StatusCode::OK || status == StatusCode::NOT_FOUND,
                    "{who} {uri}: {status} {v}"
                );
            }
        }
    }
    assert_eq!(s.snapshot(), before);
    // And the agents see what they would have seen: Ana's next session gets Bob's question as
    // new, and nobody's delivery moved.
    let (_, inbox) = call(
        &s.app,
        "GET",
        "/api/inbox",
        Some(&s.tokens["ana"]),
        &[("relevo-session", "fresh-session")],
        None,
    )
    .await;
    let asked = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == sc.asked.as_str())
        .expect("the question is still new for Ana's session");
    assert_eq!(asked["new_notes"].as_array().unwrap().len(), 1);
    let after: i64 = s
        .app
        .db()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, sessions + 1, "only the inbox made a session");
}

#[tokio::test]
async fn a_person_invited_later_and_one_deactivated_keep_to_their_own_threads() {
    let s = server(&["ana", "bob"], &[]);
    let board = s.open("ana", "Before Eve").await;
    {
        let eve = db::invite(&s.app.db(), "eve", "Eve", None, "laptop", T0).unwrap();
        let (status, v) = call(
            &s.app,
            "GET",
            "/api/timeline?tab=board",
            Some(&eve.secret),
            &[],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // Eve was not on the server when it was posted: it is not hers.
        assert!(!ids(&v).contains(&board));
        assert_eq!(v["counts"]["board"], 0);
    }
    // A person deactivated reads nothing.
    db::deactivate(&s.app.db(), "bob", T0).unwrap();
    let (status, _) = s.get("bob", "/api/timeline?tab=board").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A layout-7 database made before the timeline's tables (by relevo 0.4.0) gets them when the
/// server moves it to layout 8, which reads `thread_closes` only once `SCHEMA` made it (and on to
/// layouts 9 and 10).
#[test]
fn a_layout_7_database_without_the_timeline_tables_gets_them_when_it_migrates() {
    let dir = std::env::temp_dir().join(db::random_id("relevo-timeline-"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("relevo.db");
    let path = path.to_str().unwrap();
    {
        let conn = db::open(path).unwrap();
        conn.execute_batch(
            "DROP INDEX participants_timeline; DROP INDEX notes_by_thread_author;
             DROP TABLE note_contexts; DROP TABLE thread_closes;
             ALTER TABLE sessions DROP COLUMN agent;
             ALTER TABLE sessions DROP COLUMN client_inferred;
             ALTER TABLE sessions DROP COLUMN client;
             PRAGMA user_version = 7;",
        )
        .unwrap();
    }
    let conn = db::open_migrating(path).unwrap();
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('note_contexts', 'thread_closes',
             'participants_timeline', 'notes_by_thread_author')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 4);
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 10);
    drop(conn);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `handled_since` counts the threads the person has handled (answered, or marked seen) since
/// then, by when they last did; without it there is no such count.
#[tokio::test]
async fn handled_since_counts_the_threads_handled_since_then() {
    let s = server(&["ana", "bob"], &[]);
    let first = s.open("bob", "@ana first").await;
    let second = s.open("bob", "@ana second").await;
    s.open("bob", "@ana third, not handled").await;
    let inbox = || {
        call(
            &s.app,
            "GET",
            "/api/inbox",
            Some(&s.tokens["ana"]),
            &[("relevo-session", "agent-one")],
            None,
        )
    };
    inbox().await;
    s.reply("ana", &first, "@bob an answer").await;
    s.app.clock.advance(100);
    s.send("ana", json!({ "thread": second, "seen": true }))
        .await;
    let handled = |v: Value| v["counts"]["handled"].clone();
    let v = s.timeline("ana", "tab=threads&show=waiting").await;
    assert!(v["counts"].get("handled").is_none(), "{v}");
    assert_eq!(v["counts"]["waiting"], 1);
    for (since, n) in [(0, 2), (T0, 2), (T0 + 50, 1), (T0 + 100, 1), (T0 + 101, 0)] {
        let q = format!("tab=threads&show=waiting&limit=1&handled_since={since}");
        assert_eq!(handled(s.timeline("ana", &q).await), n, "{since}");
    }
    // Any tab; Bob has handled nothing (nobody else wrote in his threads before Ana's answer,
    // which he has not answered).
    let v = s.timeline("bob", "tab=board&handled_since=0").await;
    assert_eq!(handled(v), 0);
    for q in ["handled_since=x", "handled_since=-1", "handled_since="] {
        s.bad("ana", &format!("tab=threads&{q}")).await;
    }
}
