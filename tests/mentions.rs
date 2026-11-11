//! Who a note is for, written in it (docs/protocol.md, "Mentions"): `@person`, `@team-name`, and
//! the global board, through the HTTP router on an in-memory database.

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

/// A server with these people, each with one token, on a fixed clock, and these teams (name,
/// members).
fn server(handles: &[&str], teams: &[(&str, &[&str])]) -> Server {
    let app = App::new(db::open(":memory:").unwrap(), Clock::fixed(T0), 5);
    let mut tokens = HashMap::new();
    {
        let conn = app.db();
        for h in handles {
            let about = format!("{h} things");
            let token =
                db::invite(&conn, h, &h.to_uppercase(), Some(&about), "laptop", T0).unwrap();
            tokens.insert(h.to_string(), token.secret);
        }
        for (name, members) in teams {
            db::add_team(&conn, name, Some(&name.to_uppercase()), T0).unwrap();
            let members: Vec<String> = members.iter().map(|m| m.to_string()).collect();
            if !members.is_empty() {
                db::join_team(&conn, name, &members).unwrap();
            }
        }
    }
    Server { app, tokens }
}

impl Server {
    /// One of `who`'s agents: the session `id` in `context` (`""`: general).
    fn agent(&self, who: &str, id: &str, context: &str) -> Agent {
        Agent {
            app: self.app.clone(),
            token: self.tokens[who].clone(),
            headers: [("relevo-session", id), ("relevo-context", context)]
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// `who`'s default session.
    fn of(&self, who: &str) -> Agent {
        self.agent(who, "", "")
    }

    async fn send(&self, who: &str, body: Value) -> (StatusCode, Value) {
        self.of(who).send(body).await
    }

    /// Sends and expects success; returns the result.
    async fn ok(&self, who: &str, body: Value) -> Value {
        let (status, v) = self.send(who, body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    /// Sends and expects success; returns the thread id.
    async fn open(&self, who: &str, text: &str) -> String {
        self.ok(who, json!({ "text": text })).await["thread"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn inbox(&self, who: &str) -> Value {
        self.of(who).inbox().await
    }

    async fn thread(&self, who: &str, id: &str) -> Value {
        self.of(who).thread(id).await
    }

    fn notes(&self) -> i64 {
        self.app
            .db()
            .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
            .unwrap()
    }

    fn sessions(&self) -> i64 {
        self.app
            .db()
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }
}

#[derive(Clone)]
struct Agent {
    app: Arc<App>,
    token: String,
    headers: Vec<(String, String)>,
}

impl Agent {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json");
        for (name, value) in &self.headers {
            req = req.header(name, value);
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

    async fn send(&self, body: Value) -> (StatusCode, Value) {
        self.call("POST", "/api/send", Some(body)).await
    }

    async fn ok(&self, body: Value) -> Value {
        let (status, v) = self.send(body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
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
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn message(v: &Value) -> &str {
    v["error"]["message"].as_str().unwrap_or("")
}

fn listed<'a>(inbox: &'a Value, id: &str) -> Option<&'a Value> {
    inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
}

fn delivery_of<'a>(th: &'a Value, who: &str) -> &'a Value {
    th["delivery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["handle"] == who)
        .unwrap_or_else(|| panic!("{who} not in {th}"))
}

fn id(v: &Value) -> String {
    v["thread"].as_str().unwrap().to_string()
}

// ---------- opening a thread ----------

#[tokio::test]
async fn people_mentioned_open_a_direct_thread_that_names_them_all() {
    let s = server(&["ana", "bob", "carl", "dan"], &[]);
    let v = s
        .ok(
            "ana",
            json!({ "text": "# Refunds\n@Bob and @carl/Pagos-API: is it live? @bob" }),
        )
        .await;
    assert_eq!(v["kind"], "direct");
    assert_eq!(v["to"], json!({ "people": ["bob", "carl"], "teams": [] }));
    assert_eq!(v["added"], json!([]));
    assert_eq!(v["reached"], 2);
    assert_eq!(v["claimed"], Value::Null);
    let th = s.thread("bob", &id(&v)).await;
    assert_eq!(th["title"], "Refunds");
    assert_eq!(th["participants"], json!(["ana", "bob", "carl"]));
    assert_eq!(th["teams"], json!([]));
    assert_eq!(th["named"], true);
    assert_eq!(th["pending"], true);
    for who in ["ana", "bob", "carl"] {
        assert_eq!(delivery_of(&th, who)["named"], true, "{who}");
    }
    // Carl's context is the one he was mentioned with.
    assert_eq!(s.thread("carl", &id(&v)).await["context"], "pagos-api");
    // Dan was not mentioned: not in it.
    let (status, _) = s
        .of("dan")
        .call("GET", &format!("/api/threads/{}", id(&v)), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_note_without_mentions_goes_to_the_global_board() {
    let s = server(&["ana", "bob", "carl", "old"], &[("backend", &["bob"])]);
    db::deactivate(&s.app.db(), "old", T0).unwrap();
    // Mentions in code, emails and URLs are no mentions; nor is yourself.
    let v = s
        .ok(
            "ana",
            json!({ "text": "Who knows `@bob`, pedro@empresa.com or https://x.com/@carl? @ana\n```\n@backend\n```" }),
        )
        .await;
    assert_eq!(v["kind"], "global");
    assert_eq!(v["to"], json!({ "people": [], "teams": [] }));
    assert_eq!(v["reached"], 2);
    let th = s.thread("ana", &id(&v)).await;
    assert_eq!(th["participants"], json!(["ana", "bob", "carl"]));
    assert_eq!(th["named"], false);
    assert!(
        th["delivery"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["named"] == false)
    );
    // Nobody has to answer it: new for each, never pending, gone once fetched.
    let inbox = s.inbox("bob").await;
    let t = listed(&inbox, &id(&v)).unwrap();
    assert_eq!(t["kind"], "global");
    assert_eq!(t["pending"], false);
    assert_eq!(t["named"], false);
    assert!(listed(&s.inbox("bob").await, &id(&v)).is_none());
}

#[tokio::test]
async fn a_team_mentioned_reaches_its_active_members_who_need_not_answer() {
    let s = server(
        &["ana", "bob", "carl", "dan", "eve", "old"],
        &[
            ("backend", &["bob", "carl", "old", "ana"]),
            ("front", &["eve"]),
        ],
    );
    db::deactivate(&s.app.db(), "old", T0).unwrap();
    let v = s
        .ok("ana", json!({ "text": "@backend @dan who owns billing?" }))
        .await;
    assert_eq!(v["kind"], "team");
    assert_eq!(v["to"], json!({ "people": ["dan"], "teams": ["backend"] }));
    assert_eq!(v["reached"], 3);
    let th = s.thread("ana", &id(&v)).await;
    // The writer, the people named, then the team's other active members.
    assert_eq!(th["participants"], json!(["ana", "dan", "bob", "carl"]));
    assert_eq!(th["teams"], json!(["backend"]));
    assert_eq!(th["named"], false);
    assert_eq!(delivery_of(&th, "dan")["named"], true);
    assert_eq!(delivery_of(&th, "bob")["named"], false);
    // Dan, named, has it pending; Bob, reached through the team, has it new, not pending.
    let t = s.inbox("dan").await;
    let t = listed(&t, &id(&v)).unwrap();
    assert_eq!((&t["pending"], &t["named"]), (&json!(true), &json!(true)));
    assert_eq!(t["teams"], json!(["backend"]));
    let inbox = s.inbox("bob").await;
    let t = listed(&inbox, &id(&v)).unwrap();
    assert_eq!((&t["pending"], &t["named"]), (&json!(false), &json!(false)));
    assert!(listed(&s.inbox("bob").await, &id(&v)).is_none());
    assert!(listed(&s.inbox("dan").await, &id(&v)).is_some());
    // An answer from the team is not pending for the opener, who is not named.
    s.ok("bob", json!({ "thread": id(&v), "text": "I do." }))
        .await;
    let t = s.inbox("ana").await;
    let t = listed(&t, &id(&v)).unwrap();
    assert_eq!(t["pending"], false);
    assert_eq!(t["new_notes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_person_in_a_team_mentioned_too_is_reached_once_and_named() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob", "carl"])]);
    let v = s
        .ok("ana", json!({ "text": "@backend, @bob mostly: deploy?" }))
        .await;
    assert_eq!(v["reached"], 2);
    let th = s.thread("ana", &id(&v)).await;
    assert_eq!(th["participants"], json!(["ana", "bob", "carl"]));
    assert_eq!(delivery_of(&th, "bob")["named"], true);
    assert_eq!(delivery_of(&th, "carl")["named"], false);
    assert_eq!(s.thread("bob", &id(&v)).await["pending"], true);
    assert_eq!(s.thread("carl", &id(&v)).await["pending"], false);
}

#[tokio::test]
async fn at_team_is_your_one_team() {
    let s = server(
        &["ana", "bob", "carl", "dan"],
        &[("backend", &["ana", "bob"]), ("front", &["carl", "dan"])],
    );
    for text in [
        "@team deploy?",
        "@Equipo ¿despliegue?",
        "@team @backend once",
    ] {
        let v = s.ok("ana", json!({ "text": text })).await;
        assert_eq!(v["kind"], "team", "{text}");
        assert_eq!(v["to"]["teams"], json!(["backend"]), "{text}");
        assert_eq!(v["reached"], 1, "{text}");
    }
    // In several teams, or none: name one.
    db::join_team(&s.app.db(), "front", &["ana".to_string()]).unwrap();
    let (status, v) = s.send("ana", json!({ "text": "@team deploy?" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "BAD_REQUEST");
    assert!(message(&v).contains("@backend, @front"), "{v}");
    db::leave_team(&s.app.db(), "backend", &["ana".to_string()]).unwrap();
    db::leave_team(&s.app.db(), "front", &["ana".to_string()]).unwrap();
    let (_, v) = s.send("ana", json!({ "text": "@equipo deploy?" })).await;
    assert_eq!(code(&v), "BAD_REQUEST");
    assert!(message(&v).contains("you are in none"), "{v}");
    // No context on it.
    let (_, v) = s
        .send("bob", json!({ "text": "@team/pagos deploy?" }))
        .await;
    assert_eq!(code(&v), "BAD_REQUEST");
}

#[tokio::test]
async fn nobody_but_you_is_no_recipients() {
    let s = server(&["ana", "bob"], &[("solo", &["ana"]), ("empty", &[])]);
    for text in ["@solo anyone?", "@empty anyone?", "@team anyone?"] {
        let (status, v) = s.send("ana", json!({ "text": text })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(code(&v), "NO_RECIPIENTS", "{text}: {v}");
    }
    // A team that reaches nobody, next to a person, still names the team.
    let v = s.ok("ana", json!({ "text": "@solo @bob deploy?" })).await;
    assert_eq!(v["kind"], "team");
    assert_eq!(v["reached"], 1);
    assert_eq!(s.thread("bob", &id(&v)).await["teams"], json!(["solo"]));
    // Alone on the server: the global board reaches nobody.
    let alone = server(&["ana"], &[]);
    let (_, v) = alone.send("ana", json!({ "text": "hello?" })).await;
    assert_eq!(code(&v), "NO_RECIPIENTS");
    let (_, v) = alone.send("ana", json!({ "text": "@ana hello?" })).await;
    assert_eq!(code(&v), "NO_RECIPIENTS");
    assert_eq!(alone.notes(), 0);
}

#[tokio::test]
async fn team_and_global_threads_share_the_daily_limit() {
    let s = server(&["ana", "bob"], &[("backend", &["bob"])]);
    for i in 0..3 {
        s.ok("ana", json!({ "text": format!("q{i}") })).await;
    }
    s.ok("ana", json!({ "text": "@backend q3" })).await;
    s.ok("ana", json!({ "to": ["@team"], "text": "q4" })).await;
    for text in ["q5", "@backend q5"] {
        let (status, v) = s.send("ana", json!({ "text": text })).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
        assert_eq!(code(&v), "TEAM_LIMIT");
        assert!(message(&v).contains("@handle"), "{v}");
    }
    let (_, v) = s
        .send("ana", json!({ "to": ["@team"], "text": "q5" }))
        .await;
    assert_eq!(code(&v), "TEAM_LIMIT");
    // Naming the people is never limited.
    s.ok("ana", json!({ "text": "@bob q5" })).await;
    // A day later, again.
    s.app.clock.advance(24 * 3600);
    s.ok("ana", json!({ "text": "@backend q6" })).await;
}

#[tokio::test]
async fn an_unknown_name_fails_and_never_falls_back_to_the_global_board() {
    let s = server(&["ana", "bob", "old"], &[("backend", &["bob"])]);
    db::deactivate(&s.app.db(), "old", T0).unwrap();
    let agent = s.agent("ana", "term-0001", "");
    let (status, v) = agent
        .send(json!({ "text": "@bobb @backend @old @zed: deploy? @bobb" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    assert_eq!(v["error"]["unknown"], json!(["bobb", "old", "zed"]));
    for name in ["@bobb", "@old", "@zed"] {
        assert!(message(&v).contains(name), "{v}");
    }
    assert_eq!(
        v["error"]["valid"],
        json!([
            { "handle": "ana", "name": "ANA", "about": "ana things", "contexts": [], "teams": [] },
            { "handle": "bob", "name": "BOB", "about": "bob things", "contexts": [], "teams": ["backend"] },
        ])
    );
    assert_eq!(
        v["error"]["teams"],
        json!([{ "name": "backend", "title": "BACKEND", "members": ["bob"] }])
    );
    // Nothing was written; the session was seen, as past its own checks.
    assert_eq!(s.notes(), 0);
    assert_eq!(s.sessions(), 1);
    // In the deprecated `to`, a team's name is no person.
    let (_, v) = s
        .send("ana", json!({ "to": ["backend"], "text": "deploy?" }))
        .await;
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    assert_eq!(v["error"]["unknown"], json!(["backend"]));
}

#[tokio::test]
async fn malformed_mentions_are_bad_requests_before_the_session_counts() {
    let s = server(&["ana", "bob"], &[("backend", &["bob"])]);
    let twenty_one: String = (0..21).map(|i| format!("@p{i:02} ")).collect();
    let agent = s.agent("ana", "term-0001", "");
    for text in [
        "@bob/pagos @bob",
        "@bob/pagos @bob/front",
        "@bob/Pagos@ deploy",
        "@bob/.x",
        "@team/x",
        twenty_one.as_str(),
    ] {
        let (status, v) = agent.send(json!({ "text": text })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(code(&v), "BAD_REQUEST", "{text}: {v}");
    }
    assert_eq!(s.sessions(), 0);
    // A context on a team is known only once the name is: past the session's checks.
    let (_, v) = agent
        .send(json!({ "text": "@backend/pagos deploy?" }))
        .await;
    assert_eq!(code(&v), "BAD_REQUEST");
    assert!(message(&v).contains("team"), "{v}");
    assert_eq!(s.sessions(), 1);
    assert_eq!(s.notes(), 0);
}

#[tokio::test]
async fn the_deprecated_to_opens_as_0_3_did_and_reads_no_mentions() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["carl"])]);
    let v = s
        .ok(
            "ana",
            json!({ "to": ["@Bob/pagos-api"], "text": "@carl @backend hi" }),
        )
        .await;
    assert_eq!(v["kind"], "direct");
    assert_eq!(v["to"], json!({ "people": ["bob"], "teams": [] }));
    assert_eq!(v["reached"], 1);
    assert_eq!(
        s.thread("bob", &id(&v)).await["participants"],
        json!(["ana", "bob"])
    );
    let v = s
        .ok("ana", json!({ "to": ["@team"], "text": "@bob anyone?" }))
        .await;
    assert_eq!(v["kind"], "global");
    assert_eq!(v["to"], json!({ "people": [], "teams": [] }));
    assert_eq!(s.thread("bob", &id(&v)).await["named"], false);
}

// ---------- replies ----------

#[tokio::test]
async fn a_reply_names_participants_and_pending_starts_at_that_note() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob", "carl"])]);
    let thread = s.open("ana", "@backend who owns billing?").await;
    s.ok("bob", json!({ "thread": thread, "text": "Not me." }))
        .await;
    // Carl, reached through the team, read it all and said nothing: nothing pending.
    let carl = s.agent("carl", "carl-term-1", "");
    carl.inbox().await;
    assert_eq!(carl.thread(&thread).await["pending"], false);
    // Bob names him: from that note on, it is pending for him.
    let v = s
        .ok(
            "bob",
            json!({ "thread": thread, "text": "@carl/x you did the invoices, no?" }),
        )
        .await;
    assert_eq!(v["kind"], "team");
    assert_eq!(v["to"], json!({ "people": ["carl"], "teams": [] }));
    assert_eq!(v["added"], json!([]));
    assert_eq!(v["reached"], 2);
    let inbox = carl.inbox().await;
    let t = listed(&inbox, &thread).unwrap();
    assert_eq!((&t["pending"], &t["named"]), (&json!(true), &json!(true)));
    assert_eq!(t["new_notes"].as_array().unwrap().len(), 1);
    // His context in the thread is the one it had: the mention's is for people it adds.
    assert_eq!(t["context"], Value::Null);
    // It stays until he handles it.
    assert!(listed(&carl.inbox().await, &thread).is_some());
    // A new session of his starts at the note that named him, not before: what came before
    // was optional for him.
    let other = s.agent("carl", "carl-term-2", "");
    let inbox = other.inbox().await;
    let t = listed(&inbox, &thread).unwrap();
    let new: Vec<&str> = t["new_notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap())
        .collect();
    assert_eq!(new, ["@carl/x you did the invoices, no?"]);
    carl.ok(json!({ "thread": thread, "done": true })).await;
    assert!(listed(&carl.inbox().await, &thread).is_none());
    assert_eq!(
        delivery_of(&s.thread("ana", &thread).await, "carl")["named"],
        true
    );
    // Bob, who named him, is still not named.
    assert_eq!(s.thread("bob", &thread).await["named"], false);
}

#[tokio::test]
async fn a_reply_adds_people_who_were_not_in_the_thread() {
    let s = server(&["ana", "bob", "dan"], &[]);
    let thread = s.open("ana", "@bob deploy at 5?").await;
    s.ok("bob", json!({ "thread": thread, "text": "Fine by me." }))
        .await;
    let v = s
        .ok(
            "ana",
            json!({ "thread": thread, "text": "@dan/Infra @bob/x can you watch it?" }),
        )
        .await;
    assert_eq!(v["kind"], "direct");
    assert_eq!(v["to"], json!({ "people": ["dan", "bob"], "teams": [] }));
    assert_eq!(v["added"], json!(["dan"]));
    assert_eq!(v["reached"], 2);
    let th = s.thread("ana", &thread).await;
    assert_eq!(th["participants"], json!(["ana", "bob", "dan"]));
    assert_eq!(delivery_of(&th, "dan")["named"], true);
    // Dan gets the whole thread, addressed to his context, pending; Bob's context stays.
    let dan = s.agent("dan", "dan-term-1", "infra");
    let inbox = dan.inbox().await;
    let t = listed(&inbox, &thread).unwrap();
    assert_eq!(t["context"], "infra");
    assert_eq!(t["pending"], true);
    assert_eq!(t["new_notes"].as_array().unwrap().len(), 3);
    assert_eq!(s.thread("bob", &thread).await["context"], Value::Null);
    // Named again: nothing changes.
    let v = s
        .ok("bob", json!({ "thread": thread, "text": "@dan thanks" }))
        .await;
    assert_eq!(v["added"], json!([]));
    assert_eq!(v["reached"], 2);
}

#[tokio::test]
async fn someone_invited_after_a_global_thread_joins_it_when_named() {
    let s = server(&["ana", "bob"], &[]);
    let thread = s.open("ana", "Who knows the VPN setup?").await;
    let token = db::invite(&s.app.db(), "zoe", "Zoe", None, "laptop", T0).unwrap();
    let v = s
        .ok(
            "bob",
            json!({ "thread": thread, "text": "@zoe set it up." }),
        )
        .await;
    assert_eq!(v["kind"], "global");
    assert_eq!(v["added"], json!(["zoe"]));
    assert_eq!(v["reached"], 2);
    let zoe = Agent {
        app: s.app.clone(),
        token: token.secret,
        headers: vec![],
    };
    let inbox = zoe.inbox().await;
    let t = listed(&inbox, &thread).unwrap();
    assert_eq!((&t["pending"], &t["named"]), (&json!(true), &json!(true)));
}

#[tokio::test]
async fn a_reply_cannot_mention_a_team_and_a_failed_send_writes_nothing() {
    let s = server(&["ana", "bob", "dan", "old"], &[("backend", &["bob"])]);
    let thread = s.open("ana", "@bob deploy?").await;
    db::deactivate(&s.app.db(), "old", T0).unwrap();
    let before = s.thread("ana", &thread).await;
    for (text, expected) in [
        ("@backend thoughts?", "BAD_REQUEST"),
        ("@team thoughts?", "BAD_REQUEST"),
        ("@equipo thoughts?", "BAD_REQUEST"),
        ("@dan @backend thoughts?", "BAD_REQUEST"),
        ("@dan @zed thoughts?", "UNKNOWN_PERSON"),
        ("@dan @old thoughts?", "UNKNOWN_PERSON"),
        ("@dan/pagos @dan thoughts?", "BAD_REQUEST"),
    ] {
        let (status, v) = s
            .send(
                "ana",
                json!({ "thread": thread, "text": text, "close": true }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(code(&v), expected, "{text}: {v}");
    }
    let (_, v) = s
        .send("ana", json!({ "thread": thread, "text": "@backend?" }))
        .await;
    assert!(message(&v).contains("open a new thread"), "{v}");
    // Not the note, not the people it would have added, not the close.
    let after = s.thread("ana", &thread).await;
    assert_eq!(after["participants"], before["participants"]);
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(after["status"], "open");
    // Someone deactivated stays in the thread, and the others may still write there, but a
    // mention of them fails.
    let thread = s.open("ana", "@bob @dan deploy?").await;
    db::deactivate(&s.app.db(), "dan", T0).unwrap();
    s.ok("bob", json!({ "thread": thread, "text": "Dan is away." }))
        .await;
    let (_, v) = s
        .send("bob", json!({ "thread": thread, "text": "@dan?" }))
        .await;
    assert_eq!(code(&v), "UNKNOWN_PERSON");
}

#[tokio::test]
async fn replays_return_the_first_result_whom_it_reached_included() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob"])]);
    let body = json!({ "text": "@backend deploy?", "key": "k1" });
    let first = s.ok("ana", body.clone()).await;
    assert_eq!(first["to"]["teams"], json!(["backend"]));
    // Bob adds Carl: the replay still says what the first send did.
    s.ok(
        "bob",
        json!({ "thread": id(&first), "text": "@carl too", "key": "k1" }),
    )
    .await;
    let again = s.ok("ana", body).await;
    assert_eq!(again, first);
    let reply = json!({ "thread": id(&first), "text": "@carl hi", "key": "k2" });
    let first = s.ok("ana", reply.clone()).await;
    assert_eq!(first["added"], json!([]));
    assert_eq!(first["reached"], 2);
    let again = s.ok("ana", reply).await;
    assert_eq!(again, first);
    assert_eq!(s.notes(), 3);
}

#[tokio::test]
async fn only_the_opener_closes_a_team_or_global_thread() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob", "carl"])]);
    for text in [
        "@backend deploy?",
        "Anyone deploying?",
        "@backend @carl deploy?",
    ] {
        let thread = s.open("ana", text).await;
        for who in ["bob", "carl"] {
            let (status, v) = s
                .send(
                    who,
                    json!({ "thread": thread, "text": "Done.", "close": true }),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
            assert_eq!(code(&v), "FORBIDDEN");
            assert!(message(&v).contains("opened"), "{v}");
        }
        s.ok("ana", json!({ "thread": thread, "close": true }))
            .await;
    }
    // A direct thread: anyone in it.
    let thread = s.open("ana", "@bob @carl deploy?").await;
    s.ok("carl", json!({ "thread": thread, "close": true }))
        .await;
}

#[tokio::test]
async fn a_closed_team_thread_leaves_who_it_does_not_name_but_shows_its_last_note_to_who_it_names()
{
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob", "carl"])]);
    let thread = s.open("ana", "@backend @carl deploy?").await;
    s.ok(
        "ana",
        json!({ "thread": thread, "text": "Never mind, done.", "close": true }),
    )
    .await;
    // Bob, reached through the team: resolved, gone, even unread.
    assert!(listed(&s.inbox("bob").await, &thread).is_none());
    // Carl, named: as a closed direct thread, it shows until he fetched it, pending no more.
    let inbox = s.inbox("carl").await;
    let t = listed(&inbox, &thread).unwrap();
    assert_eq!(t["pending"], false);
    assert_eq!(t["new_notes"].as_array().unwrap().len(), 2);
    assert!(listed(&s.inbox("carl").await, &thread).is_none());
}

// ---------- teams in the lists ----------

#[tokio::test]
async fn inbox_and_people_list_the_teams_and_each_persons() {
    let s = server(
        &["ana", "bob", "old"],
        &[
            ("front", &["ana"]),
            ("backend", &["bob", "ana", "old"]),
            ("empty", &[]),
        ],
    );
    db::deactivate(&s.app.db(), "old", T0).unwrap();
    let expected_teams = json!([
        { "name": "backend", "title": "BACKEND", "members": ["ana", "bob"] },
        { "name": "empty", "title": "EMPTY", "members": [] },
        { "name": "front", "title": "FRONT", "members": ["ana"] },
    ]);
    let inbox = s.inbox("ana").await;
    assert_eq!(inbox["teams"], expected_teams);
    assert_eq!(inbox["team"][0]["teams"], json!(["backend", "front"]));
    assert_eq!(inbox["team"][1]["teams"], json!(["backend"]));
    let (_, people) = s.of("bob").call("GET", "/api/people", None).await;
    assert_eq!(people["teams"], expected_teams);
    assert_eq!(people["team"], inbox["team"]);
}

#[tokio::test]
async fn a_thread_keeps_its_participants_and_team_names_whatever_the_teams_do_later() {
    let s = server(&["ana", "bob", "carl"], &[("backend", &["bob"])]);
    let thread = s.open("ana", "@backend deploy?").await;
    db::join_team(&s.app.db(), "backend", &["carl".to_string()]).unwrap();
    db::leave_team(&s.app.db(), "backend", &["bob".to_string()]).unwrap();
    db::remove_team(&s.app.db(), "backend").unwrap();
    let th = s.thread("bob", &thread).await;
    assert_eq!(th["participants"], json!(["ana", "bob"]));
    assert_eq!(th["teams"], json!(["backend"]));
    // A later team may take the name; the old thread is not its.
    db::add_team(&s.app.db(), "backend", None, T0).unwrap();
    db::join_team(&s.app.db(), "backend", &["carl".to_string()]).unwrap();
    let v = s.ok("ana", json!({ "text": "@backend again?" })).await;
    assert_ne!(id(&v), thread);
    assert_eq!(
        s.thread("carl", &id(&v)).await["participants"],
        json!(["ana", "carl"])
    );
}

#[tokio::test]
async fn a_person_added_by_a_reply_is_woken() {
    let s = server(&["ana", "bob", "dan"], &[]);
    let thread = s.open("ana", "@bob deploy?").await;
    let dan = s.agent("dan", "dan-term-1", "");
    dan.inbox().await;
    let waiter = tokio::spawn({
        let dan = dan.clone();
        async move { dan.call("GET", "/api/inbox?wait=30", None).await }
    });
    let start = std::time::Instant::now();
    while s.app.waiting() != 1 {
        assert!(start.elapsed().as_secs() < 5);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    s.ok("ana", json!({ "thread": thread, "text": "@dan too" }))
        .await;
    let (_, v) = waiter.await.unwrap();
    assert!(listed(&v, &thread).is_some(), "{v}");
    assert!(start.elapsed().as_secs() < 10);
}
