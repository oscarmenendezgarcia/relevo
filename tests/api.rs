//! Every rule of docs/protocol.md, exercised through the HTTP router on an in-memory database.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::{App, Clock, api, db};
use serde_json::{Value, json};
use tower::ServiceExt;

const T0: i64 = 1_794_134_460; // 2026-11-08T10:41:00Z

/// A claim lapses when its session has not been seen for this long.
const LAPSE: i64 = 15 * 60;

struct Team {
    app: Arc<App>,
    tokens: HashMap<String, String>,
}

/// A team of the given handles, each with one token, on a fixed clock.
fn team(handles: &[&str]) -> Team {
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
    }
    Team { app, tokens }
}

impl Team {
    async fn raw(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Body,
    ) -> (StatusCode, Value) {
        call(&self.app, method, uri, token, &[], body).await
    }

    /// One of `who`'s agents: the session `id` (`""`: the token's default session) in
    /// `context` (`""`: general).
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

    async fn get(&self, who: &str, uri: &str) -> (StatusCode, Value) {
        self.raw("GET", uri, Some(&self.tokens[who]), Body::empty())
            .await
    }

    async fn send(&self, who: &str, body: Value) -> (StatusCode, Value) {
        self.raw(
            "POST",
            "/api/send",
            Some(&self.tokens[who]),
            Body::from(body.to_string()),
        )
        .await
    }

    /// Sends and expects success; returns the thread id.
    async fn ok(&self, who: &str, body: Value) -> String {
        let (status, v) = self.send(who, body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["thread"].as_str().unwrap().to_string()
    }

    async fn inbox(&self, who: &str) -> Value {
        let (status, v) = self.get(who, "/api/inbox").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn thread(&self, who: &str, id: &str) -> Value {
        let (status, v) = self.get(who, &format!("/api/threads/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }
}

/// One request through the router.
async fn call(
    app: &Arc<App>,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(String, String)],
    body: Body,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        req = req.header(name, value);
    }
    req = req.header("content-type", "application/json");
    let res = api::router(app.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Requests as one session of a person. Cheap to clone into a spawned task.
#[derive(Clone)]
struct Agent {
    app: Arc<App>,
    token: String,
    headers: Vec<(String, String)>,
}

impl Agent {
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        call(
            &self.app,
            "GET",
            uri,
            Some(&self.token),
            &self.headers,
            Body::empty(),
        )
        .await
    }

    async fn send(&self, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            "POST",
            "/api/send",
            Some(&self.token),
            &self.headers,
            Body::from(body.to_string()),
        )
        .await
    }

    /// Ends this session: `DELETE /api/session`.
    async fn end(&self) -> (StatusCode, Value) {
        call(
            &self.app,
            "DELETE",
            "/api/session",
            Some(&self.token),
            &self.headers,
            Body::empty(),
        )
        .await
    }

    async fn ok(&self, body: Value) -> String {
        let (status, v) = self.send(body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["thread"].as_str().unwrap().to_string()
    }

    async fn inbox(&self) -> Value {
        self.inbox_at("/api/inbox").await
    }

    async fn thread(&self, id: &str) -> Value {
        let (status, v) = self.get(&format!("/api/threads/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn inbox_at(&self, uri: &str) -> Value {
        let (status, v) = self.get(uri).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn thread_ids(inbox: &Value) -> Vec<String> {
    inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

// ---------- auth ----------

#[tokio::test]
async fn health_needs_no_token() {
    let t = team(&["ana"]);
    let (status, v) = t.raw("GET", "/health", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, json!({ "ok": true }));
}

#[tokio::test]
async fn missing_or_unknown_token_is_unauthorized() {
    let t = team(&["ana"]);
    let (status, v) = t.raw("GET", "/api/inbox", None, Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "UNAUTHORIZED");
    let (status, _) = t
        .raw("GET", "/api/people", Some("rlv_nope"), Body::empty())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = t.raw("POST", "/api/send", None, Body::from("{}")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let t = team(&["ana"]);
    for scheme in ["bearer", "BEARER", "Bearer"] {
        let req = Request::builder()
            .uri("/api/inbox")
            .header("authorization", format!("{scheme} {}", t.tokens["ana"]))
            .body(Body::empty())
            .unwrap();
        let res = api::router(t.app.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{scheme}");
    }
    let req = Request::builder()
        .uri("/api/inbox")
        .header("authorization", format!("Basic {}", t.tokens["ana"]))
        .body(Body::empty())
        .unwrap();
    let res = api::router(t.app.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_token_is_unauthorized() {
    let t = team(&["ana", "bob"]);
    let id = db::list_tokens(&t.app.db(), Some("ana")).unwrap()[0]
        .id
        .clone();
    db::revoke_token(&t.app.db(), &id, T0).unwrap();
    let (status, v) = t.get("ana", "/api/inbox").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "UNAUTHORIZED");
    // Other tokens keep working.
    assert_eq!(t.get("bob", "/api/inbox").await.0, StatusCode::OK);
}

#[tokio::test]
async fn deactivated_person_is_unauthorized_and_unreachable() {
    let t = team(&["ana", "bob", "carl"]);
    db::deactivate(&t.app.db(), "bob", T0).unwrap();
    let (status, _) = t.get("bob", "/api/inbox").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, v) = t.send("ana", json!({ "to": ["bob"], "text": "hi" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    let team_list = &t.inbox("ana").await["team"];
    assert_eq!(team_list.as_array().unwrap().len(), 2);
    let id = t
        .ok("ana", json!({ "to": ["@team"], "text": "who knows?" }))
        .await;
    assert_eq!(
        t.thread("ana", &id).await["participants"],
        json!(["ana", "carl"])
    );
}

#[tokio::test]
async fn deactivating_revokes_every_token_and_invited_back_only_the_new_one_works() {
    let t = team(&["ana", "bob"]);
    let second = db::add_token(&t.app.db(), "bob", "Codex desktop", T0).unwrap();
    db::deactivate(&t.app.db(), "bob", T0).unwrap();
    let tokens = db::list_tokens(&t.app.db(), Some("bob")).unwrap();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().all(|t| t.revoked_at == Some(T0)));

    let back = db::invite(&t.app.db(), "bob", "Bob", None, "desktop", T0).unwrap();
    for old in [t.tokens["bob"].as_str(), second.secret.as_str()] {
        let (status, v) = t.raw("GET", "/api/inbox", Some(old), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(code(&v), "UNAUTHORIZED");
    }
    let (status, _) = t
        .raw("GET", "/api/inbox", Some(&back.secret), Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    t.ok("ana", json!({ "to": ["bob"], "text": "welcome back" }))
        .await;
}

#[tokio::test]
async fn deactivated_person_stays_a_participant_of_their_old_threads() {
    let t = team(&["ana", "bob", "carl"]);
    let direct = t
        .ok("ana", json!({ "to": ["bob", "carl"], "text": "refunds?" }))
        .await;
    let asked = t
        .ok("ana", json!({ "to": ["@team"], "text": "who?" }))
        .await;
    let read = t.thread("bob", &direct).await["notes"][0]["id"].clone();
    db::deactivate(&t.app.db(), "bob", T0).unwrap();

    // Old threads still list them, with what last reached them, and still accept notes.
    let v = t.thread("ana", &direct).await;
    assert_eq!(v["participants"], json!(["ana", "bob", "carl"]));
    assert_eq!(v["delivery"][1]["handle"], "bob");
    assert_eq!(v["delivery"][1]["note"], read);
    assert_eq!(
        t.thread("ana", &asked).await["participants"],
        json!(["ana", "bob", "carl"])
    );
    t.ok("carl", json!({ "thread": direct, "text": "answer" }))
        .await;
    let inbox = t.inbox("ana").await;
    assert_eq!(
        inbox["threads"][0]["participants"],
        json!(["ana", "bob", "carl"])
    );

    // But they get no new threads: not team ones, not direct ones.
    let new_team = t
        .ok("ana", json!({ "to": ["@team"], "text": "again?" }))
        .await;
    assert_eq!(
        t.thread("ana", &new_team).await["participants"],
        json!(["ana", "carl"])
    );
    let (_, v) = t.send("ana", json!({ "to": ["bob"], "text": "hi" })).await;
    assert_eq!(code(&v), "UNKNOWN_PERSON");

    // Invited back, they find their old threads, with what they missed as new.
    let back = db::invite(&t.app.db(), "bob", "Bob", None, "desktop", T0).unwrap();
    let (_, inbox) = t
        .raw("GET", "/api/inbox", Some(&back.secret), Body::empty())
        .await;
    let ids = thread_ids(&inbox);
    assert!(ids.contains(&direct), "{inbox}");
    assert!(!ids.contains(&new_team), "{inbox}");
}

#[tokio::test]
async fn a_second_token_acts_as_the_same_person_and_records_its_label() {
    let t = team(&["ana", "bob"]);
    let second = db::add_token(&t.app.db(), "ana", "Codex desktop", T0).unwrap();
    let (status, v) = t
        .raw(
            "POST",
            "/api/send",
            Some(&second.secret),
            Body::from(json!({ "to": ["bob"], "text": "from codex" }).to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let note = &t.thread("bob", v["thread"].as_str().unwrap()).await["notes"][0];
    assert_eq!(note["author"], "ana");
    assert_eq!(note["via"], "Codex desktop");
}

// ---------- people ----------

#[tokio::test]
async fn people_lists_every_active_person_with_about() {
    let t = team(&["bob", "ana"]);
    let (status, v) = t.get("ana", "/api/people").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v,
        json!({ "team": [
            { "handle": "ana", "name": "ANA", "about": "ana things", "contexts": [] },
            { "handle": "bob", "name": "BOB", "about": "bob things", "contexts": [] },
        ], "you": "ana" })
    );
    // `you` is the caller's person, whichever of their tokens and sessions asks.
    let (_, v) = t
        .agent("bob", "term-0001", "pagos-api")
        .get("/api/people")
        .await;
    assert_eq!(v["you"], "bob");
    let codex = db::add_token(&t.app.db(), "bob", "Codex desktop", T0).unwrap();
    let (_, v) = t
        .raw("GET", "/api/people", Some(&codex.secret), Body::empty())
        .await;
    assert_eq!(v["you"], "bob");
}

// ---------- direct threads and delivery ----------

#[tokio::test]
async fn direct_thread_stays_waiting_until_you_reply() {
    let t = team(&["ana", "bob"]);
    let (status, sent) = t
        .send(
            "ana",
            json!({ "to": ["@bob"], "text": "# Refunds endpoint\nIs it live?" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent["status"], "open");
    let id = sent["thread"].as_str().unwrap();
    assert!(id.starts_with("r_") && id.len() == 14);
    assert!(sent["note"].as_str().unwrap().starts_with("n_"));

    let inbox = t.inbox("bob").await;
    let th = &inbox["threads"][0];
    assert_eq!(th["id"], id);
    assert_eq!(th["kind"], "direct");
    assert_eq!(th["title"], "Refunds endpoint");
    assert_eq!(th["from"], "ana");
    assert_eq!(th["participants"], json!(["ana", "bob"]));
    assert_eq!(th["status"], "open");
    assert_eq!(th["waiting_for_you"], true);
    let note = &th["new_notes"][0];
    assert_eq!(note["author"], "ana");
    assert_eq!(note["via"], "laptop");
    assert_eq!(note["created_at"], "2026-11-08T10:41:00Z");
    assert_eq!(note["text"], "# Refunds endpoint\nIs it live?");
    assert_eq!(note["truncated"], false);
    assert_eq!(inbox["more"], 0);

    // Delivered, but still waiting.
    let inbox = t.inbox("bob").await;
    assert_eq!(thread_ids(&inbox), vec![id.to_string()]);
    assert_eq!(inbox["threads"][0]["waiting_for_you"], true);
    assert_eq!(inbox["threads"][0]["new_notes"], json!([]));

    t.ok("bob", json!({ "thread": id, "text": "Yes, since Monday." }))
        .await;
    assert!(thread_ids(&t.inbox("bob").await).is_empty());

    let inbox = t.inbox("ana").await;
    assert_eq!(inbox["threads"][0]["waiting_for_you"], true);
    assert_eq!(
        inbox["threads"][0]["new_notes"][0]["text"],
        "Yes, since Monday."
    );
}

#[tokio::test]
async fn writers_own_notes_are_delivered_at_once() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "hello" })).await;
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
    let th = t.thread("ana", &id).await;
    assert_eq!(th["waiting_for_you"], false);
    // Delivery tells others only about notes by someone else: Ana has none yet.
    let delivery = th["delivery"].as_array().unwrap();
    assert_eq!(
        delivery[0],
        json!({ "handle": "ana", "note": null, "delivered_at": null, "by": null,
                "claimed_by": null, "seen_by": 0, "handled_at": null, "handled_by": null })
    );
    assert_eq!(
        delivery[1],
        json!({ "handle": "bob", "note": null, "delivered_at": null, "by": null, "claimed_by": null,
                "seen_by": 0, "handled_at": null, "handled_by": null })
    );
}

#[tokio::test]
async fn writing_does_not_skip_notes_that_never_reached_the_writer() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "one" })).await;
    t.ok("bob", json!({ "thread": id, "text": "two" })).await;
    // Ana answers without having fetched "two": it must still reach her, and it stays pending for
    // her (her answer did not handle a note she never saw).
    t.ok("ana", json!({ "thread": id, "text": "three" })).await;
    let inbox = t.inbox("ana").await;
    let texts: Vec<&str> = inbox["threads"][0]["new_notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["two", "three"]);
    assert_eq!(inbox["threads"][0]["waiting_for_you"], true);
}

#[tokio::test]
async fn writing_does_not_skip_a_note_of_another_session_of_the_writer() {
    let t = team(&["ana", "bob"]);
    let one = t.agent("bob", "term-0001", "");
    let two = t.agent("bob", "term-0002", "");
    one.inbox().await;
    two.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    one.inbox().await;
    two.inbox().await;
    two.ok(json!({ "thread": id, "text": "from two" })).await;
    // One answers without having fetched what two wrote: two's note must still reach it. (Two
    // holds the thread, so one finds it only with `all`.)
    one.ok(json!({ "thread": id, "text": "from one" })).await;
    t.ok("ana", json!({ "thread": id, "text": "c" })).await;
    assert_eq!(
        new_texts(&one.inbox_at("/api/inbox?all=1").await, &id),
        Some(vec![
            "from two".to_string(),
            "from one".to_string(),
            "c".to_string()
        ])
    );
}

#[tokio::test]
async fn thread_shows_whole_history_and_delivery() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "one" })).await;
    t.app.clock.advance(300);
    let th = t.thread("bob", &id).await;
    assert_eq!(th["notes"].as_array().unwrap().len(), 1);
    assert!(th.get("new_notes").is_none());
    assert_eq!(th["waiting_for_you"], true);
    assert_eq!(th["delivery"][1]["delivered_at"], "2026-11-08T10:46:00Z");
    // Delivered by `thread`: no longer new in the inbox (but still waiting).
    assert_eq!(t.inbox("bob").await["threads"][0]["new_notes"], json!([]));
    // Ana sees that Bob's agent picked it up.
    let th = t.thread("ana", &id).await;
    assert_eq!(th["delivery"][1]["handle"], "bob");
    assert_eq!(th["delivery"][1]["delivered_at"], "2026-11-08T10:46:00Z");
}

#[tokio::test]
async fn direct_thread_leaves_the_inbox_when_closed() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    t.inbox("bob").await;
    let (status, v) = t.send("ana", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::OK);
    // Opening the thread claimed nothing, and closing without text does not claim.
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "closed", "claimed": null })
    );
    assert!(thread_ids(&t.inbox("bob").await).is_empty());
    assert_eq!(t.thread("bob", &id).await["status"], "closed");
}

#[tokio::test]
async fn any_participant_closes_a_direct_thread_with_a_last_note() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let (status, v) = t
        .send(
            "bob",
            json!({ "thread": id, "text": "done, closing", "close": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "closed");
    let inbox = t.inbox("ana").await;
    let th = &inbox["threads"][0];
    assert_eq!(th["status"], "closed");
    assert_eq!(th["waiting_for_you"], false);
    assert_eq!(th["new_notes"][0]["text"], "done, closing");
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
}

#[tokio::test]
async fn closed_thread_rejects_writes() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "q", "close": true }))
        .await;
    let (status, v) = t.send("bob", json!({ "thread": id, "text": "late" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&v), "THREAD_CLOSED");
    let (status, v) = t.send("bob", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&v), "THREAD_CLOSED");
}

#[tokio::test]
async fn non_participant_gets_not_found() {
    let t = team(&["ana", "bob", "carl"]);
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "private" }))
        .await;
    let (status, v) = t.get("carl", &format!("/api/threads/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&v), "NOT_FOUND");
    let (status, v) = t
        .send("carl", json!({ "thread": id, "text": "me too" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&v), "NOT_FOUND");
    let (status, v) = t.send("carl", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&v), "NOT_FOUND");
    // Same answer as for a thread that does not exist.
    let (status, _) = t.get("carl", "/api/threads/r_aaaaaaaaaaaa").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(thread_ids(&t.inbox("carl").await).is_empty());

    // A closed thread is not revealed either (THREAD_CLOSED would tell that it exists).
    t.ok("ana", json!({ "thread": id, "close": true })).await;
    for body in [
        json!({ "thread": id, "text": "me too" }),
        json!({ "thread": id, "close": true }),
    ] {
        let (status, v) = t.send("carl", body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
        assert_eq!(code(&v), "NOT_FOUND");
    }

    // Nor a team thread asked before Dan joined (FORBIDDEN would tell that it exists).
    let team_q = t.ok("ana", json!({ "to": ["@team"], "text": "q" })).await;
    let dan = db::invite(&t.app.db(), "dan", "Dan", None, "laptop", T0).unwrap();
    for body in [
        json!({ "thread": team_q, "text": "me too" }),
        json!({ "thread": team_q, "close": true }),
    ] {
        let (status, v) = t
            .raw(
                "POST",
                "/api/send",
                Some(&dan.secret),
                Body::from(body.to_string()),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
        assert_eq!(code(&v), "NOT_FOUND");
    }
}

#[tokio::test]
async fn several_recipients_and_duplicates() {
    let t = team(&["ana", "bob", "carl"]);
    let id = t
        .ok(
            "ana",
            json!({ "to": ["bob", "@carl", "Bob"], "text": "both of you" }),
        )
        .await;
    assert_eq!(
        t.thread("ana", &id).await["participants"],
        json!(["ana", "bob", "carl"])
    );
    assert_eq!(thread_ids(&t.inbox("carl").await), vec![id.clone()]);
    t.inbox("bob").await;
    // Bob replies; Carl's latest note is not his, so it waits for Carl, not for Bob.
    t.ok("bob", json!({ "thread": id, "text": "mine" })).await;
    assert_eq!(t.inbox("carl").await["threads"][0]["waiting_for_you"], true);
    assert!(thread_ids(&t.inbox("bob").await).is_empty());
}

// ---------- team threads ----------

#[tokio::test]
async fn team_thread_leaves_inboxes_once_delivered_and_never_waits() {
    let t = team(&["ana", "bob", "carl"]);
    let (status, v) = t
        .send(
            "ana",
            json!({ "to": ["@team"], "text": "Who owns billing?" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = v["thread"].as_str().unwrap().to_string();

    let th = t.thread("ana", &id).await;
    assert_eq!(th["kind"], "team");
    assert_eq!(th["participants"], json!(["ana", "bob", "carl"]));
    assert_eq!(th["waiting_for_you"], false);
    assert!(thread_ids(&t.inbox("ana").await).is_empty());

    let inbox = t.inbox("bob").await;
    assert_eq!(inbox["threads"][0]["id"], id);
    assert_eq!(inbox["threads"][0]["waiting_for_you"], false);
    assert!(thread_ids(&t.inbox("bob").await).is_empty());

    t.ok("carl", json!({ "thread": id, "text": "I do." })).await;
    let inbox = t.inbox("ana").await;
    assert_eq!(inbox["threads"][0]["new_notes"][0]["text"], "I do.");
    assert_eq!(inbox["threads"][0]["waiting_for_you"], false);
    assert_eq!(thread_ids(&t.inbox("bob").await), vec![id.clone()]);
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
}

#[tokio::test]
async fn team_participants_are_fixed_when_asked() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["@team"], "text": "q" })).await;
    let dan = db::invite(&t.app.db(), "dan", "Dan", None, "laptop", T0).unwrap();
    let (status, _) = t
        .raw(
            "GET",
            &format!("/api/threads/{id}"),
            Some(&dan.secret),
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn equipo_is_team() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok("ana", json!({ "to": ["@equipo"], "text": "¿Quién sabe?" }))
        .await;
    assert_eq!(t.thread("bob", &id).await["kind"], "team");
}

#[tokio::test]
async fn team_and_equipo_without_at_and_repeated_are_one_team_thread() {
    let t = team(&["ana", "bob"]);
    for to in [
        json!(["team"]),
        json!(["equipo"]),
        json!(["@team", "@team"]),
        json!(["@Team", "equipo"]),
    ] {
        let (status, v) = t
            .send("ana", json!({ "to": to, "text": "who knows?" }))
            .await;
        assert_eq!(status, StatusCode::OK, "{to}: {v}");
        let id = v["thread"].as_str().unwrap();
        assert_eq!(t.thread("bob", id).await["kind"], "team", "{to}");
    }
}

#[tokio::test]
async fn team_must_be_alone_in_to() {
    let t = team(&["ana", "bob"]);
    let (status, v) = t
        .send("ana", json!({ "to": ["@team", "bob"], "text": "q" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "BAD_REQUEST");
}

#[tokio::test]
async fn only_the_asker_closes_a_team_thread() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["@team"], "text": "q" })).await;
    let (status, v) = t.send("bob", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&v), "FORBIDDEN");
    // Nothing was written by the rejected attempt.
    let (status, v) = t
        .send(
            "bob",
            json!({ "thread": id, "text": "answer", "close": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        t.thread("ana", &id).await["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Bob may still answer.
    t.ok("bob", json!({ "thread": id, "text": "answer" })).await;
    let (status, v) = t.send("ana", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "closed");
}

#[tokio::test]
async fn a_closed_team_thread_leaves_every_inbox_even_with_notes_not_fetched() {
    let t = team(&["ana", "luis", "oscar"]);
    let front = t.agent("ana", "term-front", "frontend");
    front.inbox().await;
    let id = t
        .ok(
            "oscar",
            json!({ "to": ["@team"], "text": "Who owns billing?" }),
        )
        .await;
    // Another session of Ana's fetches the question, so it is new for `front` alone too.
    t.inbox("ana").await;
    t.ok("luis", json!({ "thread": id, "text": "I do." })).await;
    t.ok(
        "oscar",
        json!({ "thread": id, "text": "Thanks, solved.", "close": true }),
    )
    .await;
    for inbox in [
        front.inbox().await,
        t.inbox("ana").await,
        t.inbox("oscar").await,
    ] {
        assert!(thread_ids(&inbox).is_empty(), "{inbox}");
        assert_eq!(inbox["more"], 0, "{inbox}");
    }
    // `all` and `thread` still show it, with what was not fetched.
    let all = front.inbox_at("/api/inbox?all=1").await;
    assert_eq!(
        new_texts(&all, &id),
        Some(vec![
            "Who owns billing?".to_string(),
            "I do.".to_string(),
            "Thanks, solved.".to_string()
        ])
    );
    assert_eq!(all["threads"][0]["status"], "closed");
    let th = t.thread("luis", &id).await;
    assert_eq!(th["status"], "closed");
    assert_eq!(th["notes"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_closed_team_thread_does_not_answer_a_waiting_inbox() {
    let t = team(&["ana", "luis", "oscar"]);
    let front = t.agent("ana", "term-front", "frontend");
    front.inbox().await;
    let id = t
        .ok(
            "oscar",
            json!({ "to": ["@team"], "text": "Who owns billing?" }),
        )
        .await;
    t.ok("luis", json!({ "thread": id, "text": "I do." })).await;
    t.ok("oscar", json!({ "thread": id, "close": true })).await;
    // Already closed: nothing is new, so the request waits.
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let front = front.clone();
        async move { front.inbox_at("/api/inbox?wait=1").await }
    });
    until_waiting(&t.app, 1).await;
    // Closed as it is asked: it wakes the request, which finds nothing and waits on.
    t.ok(
        "oscar",
        json!({ "to": ["@team"], "text": "Never mind", "close": true }),
    )
    .await;
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_millis() >= 1000);
    assert!(thread_ids(&v).is_empty(), "{v}");
}

#[tokio::test]
async fn team_of_one_has_no_recipients() {
    let t = team(&["ana"]);
    let (status, v) = t
        .send("ana", json!({ "to": ["@team"], "text": "anyone?" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "NO_RECIPIENTS");
}

#[tokio::test]
async fn sixth_team_question_in_24_hours_fails() {
    let t = team(&["ana", "bob"]);
    for i in 0..5 {
        t.ok("ana", json!({ "to": ["@team"], "text": format!("q{i}") }))
            .await;
        t.app.clock.advance(3600);
    }
    let (status, v) = t
        .send("ana", json!({ "to": ["@team"], "text": "q5" }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&v), "TEAM_LIMIT");
    // Others are not limited by Ana's questions, and Ana may still ask directly.
    t.ok("bob", json!({ "to": ["@team"], "text": "mine" }))
        .await;
    t.ok("ana", json!({ "to": ["bob"], "text": "direct" }))
        .await;
    // The window rolls: 24 hours after the first question, one more fits.
    t.app.clock.advance(24 * 3600 - 5 * 3600);
    t.ok("ana", json!({ "to": ["@team"], "text": "q5" })).await;
    let (status, _) = t
        .send("ana", json!({ "to": ["@team"], "text": "q6" }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn team_limit_is_configurable() {
    let app = App::new(db::open(":memory:").unwrap(), Clock::fixed(T0), 1);
    let ana = db::invite(&app.db(), "ana", "Ana", None, "l", T0).unwrap();
    db::invite(&app.db(), "bob", "Bob", None, "l", T0).unwrap();
    let t = Team {
        app,
        tokens: HashMap::from([("ana".to_string(), ana.secret)]),
    };
    t.ok("ana", json!({ "to": ["@team"], "text": "q" })).await;
    let (_, v) = t.send("ana", json!({ "to": ["@team"], "text": "q" })).await;
    assert_eq!(code(&v), "TEAM_LIMIT");
}

// ---------- send validation ----------

#[tokio::test]
async fn unknown_person_lists_valid_handles() {
    let t = team(&["ana", "bob"]);
    let (status, v) = t
        .send("ana", json!({ "to": ["bob", "zed"], "text": "hi" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    assert!(v["error"]["message"].as_str().unwrap().contains("zed"));
    assert_eq!(
        v["error"]["valid"],
        json!([
            { "handle": "ana", "name": "ANA", "about": "ana things", "contexts": [] },
            { "handle": "bob", "name": "BOB", "about": "bob things", "contexts": [] },
        ])
    );
    // Nothing was opened.
    assert!(thread_ids(&t.inbox("bob").await).is_empty());
}

#[tokio::test]
async fn bad_requests() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let many: Vec<String> = (0..21).map(|i| format!("p{i}")).collect();
    for body in [
        json!({ "to": ["bob"], "thread": id, "text": "both" }),
        json!({ "text": "neither" }),
        json!({ "to": ["bob"] }),
        json!({ "to": ["bob"], "close": true }),
        json!({ "thread": id }),
        json!({ "thread": id, "claim": false }),
        json!({ "thread": id, "claim": "yes" }),
        json!({ "to": ["bob"], "claim": true }),
        json!({ "to": ["bob"], "text": "x", "claim": true }),
        json!({ "thread": id, "done": false }),
        json!({ "thread": id, "done": "yes" }),
        json!({ "thread": id, "done": true, "text": "x" }),
        json!({ "thread": id, "done": true, "claim": true }),
        json!({ "to": ["bob"], "done": true }),
        json!({ "to": ["bob"], "text": "x", "done": true }),
        json!({ "to": ["bob"], "text": "" }),
        json!({ "to": ["bob"], "text": " \n " }),
        json!({ "to": [], "text": "nobody" }),
        json!({ "to": many, "text": "too many" }),
        json!({ "to": ["ana"], "text": "myself" }),
        json!({ "to": ["bob"], "text": "x", "key": "" }),
        json!({ "to": ["bob"], "text": "x", "key": "k".repeat(101) }),
        json!({ "to": ["bob"], "body": "typo" }),
        json!({ "to": "bob", "text": "not a list" }),
        json!([["bob"], null, "positional", false, null]),
    ] {
        let (status, v) = t.send("ana", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(code(&v), "BAD_REQUEST", "{body}: {v}");
    }
    let (status, v) = t
        .raw(
            "POST",
            "/api/send",
            Some(&t.tokens["ana"]),
            Body::from("not json"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "BAD_REQUEST");
}

#[tokio::test]
async fn title_is_the_first_line_cut_to_80_chars() {
    let t = team(&["ana", "bob"]);
    let text = format!("{}\nsecond line", "a".repeat(100));
    let id = t.ok("ana", json!({ "to": ["bob"], "text": text })).await;
    assert_eq!(t.thread("ana", &id).await["title"], "a".repeat(80));
}

#[tokio::test]
async fn idempotent_key_returns_the_first_result() {
    let t = team(&["ana", "bob"]);
    let body = json!({ "to": ["bob"], "text": "once", "key": "k1" });
    let (_, first) = t.send("ana", body.clone()).await;
    let (status, again) = t.send("ana", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first, again);
    let (_, again) = t
        .send(
            "ana",
            json!({ "to": ["bob"], "text": "different", "key": "k1" }),
        )
        .await;
    assert_eq!(first, again);
    assert_eq!(t.inbox("bob").await["threads"].as_array().unwrap().len(), 1);
    let id = first["thread"].as_str().unwrap();
    assert_eq!(
        t.thread("bob", id).await["notes"].as_array().unwrap().len(),
        1
    );

    // The same key from another person is another send.
    let (_, bobs) = t
        .send("bob", json!({ "thread": id, "text": "reply", "key": "k1" }))
        .await;
    assert_ne!(bobs["note"], first["note"]);

    // A close is idempotent too, and replays its first result after the thread closed.
    let close = json!({ "thread": id, "close": true, "key": "close-1" });
    let (_, closed) = t.send("ana", close.clone()).await;
    let (status, again) = t.send("ana", close).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed, again);
}

#[tokio::test]
async fn a_replay_from_another_session_is_told_the_first_holder_is_another_session() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let body = json!({ "thread": id, "text": "answer", "key": "k1" });
    let (_, first) = pagos.send(body.clone()).await;
    assert_eq!(first["claimed"], "this-session");
    // Another session retries the same send: the first result, but as that session sees it.
    let (status, again) = front.send(body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["note"], first["note"]);
    assert_eq!(again["claimed"], "other-session");
    assert_eq!(front.thread(&id).await["claimed"], "other-session");
    let (_, again) = pagos.send(body).await;
    assert_eq!(again, first);
}

#[tokio::test]
async fn a_replay_after_its_holder_was_forgotten_names_another_session() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let codex = db::add_token(&t.app.db(), "bob", "Codex desktop", T0).unwrap();
    let body = json!({ "thread": id, "text": "answer", "key": "k1" });
    // The newest session holds the thread after the first send; then it is forgotten.
    let (status, first) = call(
        &t.app,
        "POST",
        "/api/send",
        Some(&codex.secret),
        &[],
        Body::from(body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["claimed"], "this-session");
    db::revoke_token(&t.app.db(), &codex.id, T0).unwrap();
    // A session started now never takes the forgotten one's place.
    let (_, again) = t.agent("bob", "term-late", "").send(body).await;
    assert_eq!(again["note"], first["note"]);
    assert_eq!(again["claimed"], "other-session");
}

#[tokio::test]
async fn idempotency_key_expires_24_hours_after_its_send() {
    let t = team(&["ana", "bob"]);
    let body = json!({ "to": ["bob"], "text": "once", "key": "k1" });
    let (_, first) = t.send("ana", body.clone()).await;
    t.app.clock.advance(24 * 3600 - 1);
    let (_, again) = t.send("ana", body.clone()).await;
    assert_eq!(first, again);
    t.app.clock.advance(1);
    let (status, fresh) = t.send("ana", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_ne!(fresh["thread"], first["thread"]);
    // The new send uses the key up again, for another 24 hours.
    let (_, again) = t.send("ana", body).await;
    assert_eq!(fresh, again);
}

#[tokio::test]
async fn expired_keys_are_forgotten_on_send() {
    let t = team(&["ana", "bob"]);
    for key in ["a", "b"] {
        t.ok("ana", json!({ "to": ["bob"], "text": "x", "key": key }))
            .await;
    }
    t.app.clock.advance(24 * 3600);
    t.ok("bob", json!({ "to": ["ana"], "text": "no key" }))
        .await;
    let keys: i64 = t
        .app
        .db()
        .query_row("SELECT count(*) FROM send_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(keys, 0);
}

#[tokio::test]
async fn failed_send_does_not_use_up_the_key() {
    let t = team(&["ana", "bob"]);
    let (status, _) = t
        .send("ana", json!({ "to": ["zed"], "text": "x", "key": "k" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    t.ok("ana", json!({ "to": ["bob"], "text": "x", "key": "k" }))
        .await;
}

#[tokio::test]
async fn escaped_text_up_to_64_kib_fits_in_the_body_limit() {
    let t = team(&["ana", "bob"]);
    // Python's json.dumps writes "ñ" as "\u00f1": 6 bytes for 2 of text.
    let body = format!(r#"{{"to":["bob"],"text":"{}"}}"#, r"\u00f1".repeat(20_000));
    let (status, v) = t
        .raw(
            "POST",
            "/api/send",
            Some(&t.tokens["ana"]),
            Body::from(body),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // The worst case: control characters, 6 bytes each, for the whole 64 KiB.
    let text = "\u{1}".repeat(64 * 1024);
    t.ok(
        "ana",
        json!({ "to": ["bob"], "text": text, "key": "k".repeat(100) }),
    )
    .await;
}

#[tokio::test]
async fn body_over_400_kib_is_413() {
    let t = team(&["ana", "bob"]);
    let body = json!({ "to": ["bob"], "text": "a".repeat(401 * 1024) });
    let (status, v) = t.send("ana", body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&v), "TOO_LARGE");
}

#[tokio::test]
async fn text_over_64_kib_is_too_large() {
    let t = team(&["ana", "bob"]);
    let (status, v) = t
        .send(
            "ana",
            json!({ "to": ["bob"], "text": "a".repeat(64 * 1024 + 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "TOO_LARGE");
    t.ok(
        "ana",
        json!({ "to": ["bob"], "text": "a".repeat(64 * 1024) }),
    )
    .await;
}

// ---------- limits ----------

#[tokio::test]
async fn twenty_first_note_in_an_hour_is_thread_busy() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "0" })).await;
    for i in 1..20 {
        let who = if i % 2 == 0 { "ana" } else { "bob" };
        t.app.clock.advance(60);
        t.ok(who, json!({ "thread": id, "text": i.to_string() }))
            .await;
    }
    let (status, v) = t.send("ana", json!({ "thread": id, "text": "21st" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&v), "THREAD_BUSY");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("check with your person")
    );
    // A claim without text is not a note: never limited.
    let (status, v) = t.send("bob", json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["claimed"], "this-session");
    // The window rolls: an hour after the first note, one more fits.
    t.app.clock.advance(3600 - 19 * 60);
    t.ok("ana", json!({ "thread": id, "text": "21st" })).await;
    t.ok("ana", json!({ "thread": id, "close": true })).await;
}

#[tokio::test]
async fn sixty_first_note_in_a_minute_is_rate_limited() {
    let t = team(&["ana", "bob"]);
    let mut id = String::new();
    for _ in 0..3 {
        id = t.ok("ana", json!({ "to": ["bob"], "text": "start" })).await;
        for i in 1..20 {
            t.ok("ana", json!({ "thread": id, "text": i.to_string() }))
                .await;
        }
    }
    let (status, v) = t
        .send("ana", json!({ "to": ["bob"], "text": "61st" }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&v), "RATE_LIMITED");
    // Nothing was opened by the rejected send.
    assert_eq!(t.inbox("bob").await["threads"].as_array().unwrap().len(), 3);
    t.ok(
        "bob",
        json!({ "to": ["ana"], "text": "bob is not limited" }),
    )
    .await;
    // A claim or a close without text is never limited.
    let (status, v) = t.send("ana", json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["note"], Value::Null);
    let (status, v) = t.send("ana", json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    t.app.clock.advance(60);
    t.ok("ana", json!({ "to": ["bob"], "text": "61st" })).await;
}

// ---------- inbox shape ----------

#[tokio::test]
async fn long_note_is_truncated_in_inbox_but_whole_in_thread() {
    let t = team(&["ana", "bob"]);
    let text = "ñ".repeat(5000);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": text })).await;
    let note = &t.inbox("bob").await["threads"][0]["new_notes"][0];
    assert_eq!(note["truncated"], true);
    assert_eq!(note["text"].as_str().unwrap().chars().count(), 4000);
    let note = &t.thread("bob", &id).await["notes"][0];
    assert_eq!(note["truncated"], false);
    assert_eq!(note["text"], text);

    t.ok("ana", json!({ "thread": id, "text": "ñ".repeat(4000) }))
        .await;
    let note = &t.inbox("bob").await["threads"][0]["new_notes"][0];
    assert_eq!(note["truncated"], false);
}

#[tokio::test]
async fn inbox_caps_at_20_threads_and_counts_the_rest() {
    let t = team(&["ana", "bob"]);
    let mut ids = Vec::new();
    for i in 0..25 {
        ids.push(
            t.ok("ana", json!({ "to": ["bob"], "text": format!("t{i}") }))
                .await,
        );
    }
    let inbox = t.inbox("bob").await;
    assert_eq!(inbox["more"], 5);
    let shown = thread_ids(&inbox);
    assert_eq!(shown.len(), 20);
    // Newest first.
    let newest: Vec<String> = ids.iter().rev().take(20).cloned().collect();
    assert_eq!(shown, newest);
    // The 5 not shown were not delivered.
    let th = t.thread("ana", &ids[0]).await;
    assert_eq!(th["delivery"][1]["delivered_at"], Value::Null);
    let th = t.thread("ana", &ids[24]).await;
    assert_eq!(th["delivery"][1]["delivered_at"], "2026-11-08T10:41:00Z");
    // All are still waiting.
    assert_eq!(t.inbox("bob").await["more"], 5);
}

#[tokio::test]
async fn waiting_threads_come_first() {
    let t = team(&["ana", "bob", "carl"]);
    let direct = t
        .ok("ana", json!({ "to": ["bob"], "text": "direct" }))
        .await;
    let team_q = t
        .ok("carl", json!({ "to": ["@team"], "text": "team" }))
        .await;
    let inbox = t.inbox("bob").await;
    assert_eq!(thread_ids(&inbox), vec![direct, team_q]);
    let team_list = inbox["team"].as_array().unwrap();
    assert_eq!(team_list.len(), 3);
}

// ---------- routing errors ----------

#[tokio::test]
async fn unknown_route_or_wrong_method_is_json_not_found() {
    let t = team(&["ana"]);
    let token = Some(t.tokens["ana"].as_str());
    for (method, uri) in [
        ("GET", "/api/nope"),
        ("POST", "/api/inbox"),
        ("GET", "/api/send"),
        ("DELETE", "/api/threads/r_aaaaaaaaaaaa"),
    ] {
        let (status, v) = t.raw(method, uri, token, Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(code(&v), "NOT_FOUND", "{method} {uri}: {v}");
    }
}

#[tokio::test]
async fn undecodable_thread_id_is_json_not_found() {
    let t = team(&["ana"]);
    let (status, v) = t.get("ana", "/api/threads/%FF").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&v), "NOT_FOUND", "{v}");
}

#[tokio::test]
async fn waiting_threads_with_nothing_new_never_hide_new_notes() {
    let t = team(&["ana", "bob", "carl"]);
    for i in 0..20 {
        t.ok("ana", json!({ "to": ["bob"], "text": format!("t{i}") }))
            .await;
    }
    // Bob's agent reads all 20 but does not reply: they stay waiting.
    assert_eq!(t.inbox("bob").await["more"], 0);
    t.app.clock.advance(60);
    let team_q = t
        .ok("carl", json!({ "to": ["@team"], "text": "urgent" }))
        .await;
    let closed = t
        .ok(
            "carl",
            json!({ "to": ["bob"], "text": "fyi", "close": true }),
        )
        .await;
    let inbox = t.inbox("bob").await;
    let shown = thread_ids(&inbox);
    assert_eq!(shown.len(), 20);
    assert_eq!(inbox["more"], 2);
    // Threads with new notes come first, newest first, then the waiting ones.
    assert_eq!(shown[..2], [closed, team_q]);
    assert_eq!(inbox["threads"][2]["waiting_for_you"], true);
    assert_eq!(inbox["threads"][2]["new_notes"], json!([]));
}

/// What the inbox should show, worked out on the side: a thread is new while it holds notes not
/// yet delivered, waiting while it is direct, open and its latest note is not yours.
struct Expected {
    id: String,
    last: usize,
    new: bool,
    waiting: bool,
}

/// Checks one inbox call against `model` (order, flags, `more`), then applies its deliveries.
async fn inbox_matches(t: &Team, who: &str, model: &mut [Expected]) {
    let mut expected: Vec<&Expected> = model.iter().filter(|e| e.new || e.waiting).collect();
    expected.sort_by_key(|e| std::cmp::Reverse((e.new, e.waiting, e.last)));
    let inbox = t.inbox(who).await;
    let shown = thread_ids(&inbox);
    let want: Vec<String> = expected.iter().take(20).map(|e| e.id.clone()).collect();
    assert_eq!(shown, want);
    assert_eq!(inbox["more"], expected.len().saturating_sub(20));
    for (th, e) in inbox["threads"].as_array().unwrap().iter().zip(&expected) {
        assert_eq!(th["waiting_for_you"], e.waiting, "{}", e.id);
        assert_eq!(th["new_notes"].as_array().unwrap().is_empty(), !e.new);
    }
    for e in model.iter_mut().filter(|e| shown.contains(&e.id)) {
        e.new = false;
    }
}

#[tokio::test]
async fn inbox_order_and_more_hold_over_hundreds_of_threads() {
    let t = team(&["ana", "bob", "carl"]);
    let mut model = Vec::new();
    let mut last = 0;
    // 300 direct threads to bob from two people; the clock moves so no rate limit is hit.
    for i in 0..300 {
        let from = if i % 2 == 0 { "ana" } else { "carl" };
        t.app.clock.advance(1);
        let id = t
            .ok(from, json!({ "to": ["bob"], "text": format!("t{i}") }))
            .await;
        last += 1;
        model.push(Expected {
            id,
            last,
            new: true,
            waiting: true,
        });
    }
    // Bob's agent reads a few pages: the newest 60 are delivered but still wait.
    for _ in 0..3 {
        inbox_matches(&t, "bob", &mut model).await;
    }
    // Then: bob replies to some (those he had read leave his inbox; one he had not stays
    // waiting), ana writes again in some read and some unread ones, carl closes one with a last
    // note and asks the team.
    for i in [299, 250, 10] {
        t.app.clock.advance(1);
        t.ok("bob", json!({ "thread": model[i].id, "text": "done" }))
            .await;
        last += 1;
        model[i].last = last;
        model[i].waiting = model[i].new;
    }
    for i in [298, 280, 4, 0] {
        t.app.clock.advance(1);
        t.ok("ana", json!({ "thread": model[i].id, "text": "and?" }))
            .await;
        last += 1;
        model[i].last = last;
        model[i].new = true;
    }
    t.app.clock.advance(1);
    t.ok(
        "carl",
        json!({ "thread": model[297].id, "text": "never mind", "close": true }),
    )
    .await;
    last += 1;
    model[297].last = last;
    model[297].new = true;
    model[297].waiting = false;
    t.app.clock.advance(1);
    let id = t
        .ok("carl", json!({ "to": ["@team"], "text": "who knows?" }))
        .await;
    last += 1;
    model.push(Expected {
        id,
        last,
        new: true,
        waiting: false,
    });
    // Every page until nothing is new, and a few after, match the model.
    for _ in 0..20 {
        inbox_matches(&t, "bob", &mut model).await;
    }
    assert!(model.iter().all(|e| !e.new));
    // 300 threads, less the 2 bob answered after reading and the one carl closed, still wait.
    assert_eq!(t.inbox("bob").await["more"], 297 - 20);
}

// ---------- sessions ----------

#[tokio::test]
async fn malformed_session_headers_are_bad_requests() {
    let t = team(&["ana", "bob"]);
    for (session, context) in [
        ("short", ""),
        ("has space in it", ""),
        ("dots.are.not.ok", ""),
        (&*"a".repeat(65), ""),
        ("abcd1234", "-pagos"),
        ("abcd1234", "pagos/api"),
        ("abcd1234", "pagos@"),
        ("abcd1234", "@main"),
        ("abcd1234", "pagos@feat refunds"),
        ("abcd1234", "pagos@a@b"),
        ("abcd1234", &*format!("pagos@{}", "b".repeat(101))),
        ("abcd1234", &*"a".repeat(65)),
        ("", "pagos api"),
    ] {
        for uri in ["/api/inbox", "/api/people", "/api/threads/r_aaaaaaaaaaaa"] {
            let (status, v) = t.agent("ana", session, context).get(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{session} {context} {uri}");
            assert_eq!(code(&v), "BAD_REQUEST", "{v}");
        }
        let (status, v) = t
            .agent("ana", session, context)
            .send(json!({ "to": ["bob"], "text": "hi" }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{session} {context}");
        assert_eq!(code(&v), "BAD_REQUEST");
    }
    // An empty session id is malformed too; an empty context means general.
    let headers = [("relevo-session".to_string(), String::new())];
    let token = t.tokens["ana"].clone();
    let (status, _) = call(
        &t.app,
        "GET",
        "/api/inbox",
        Some(&token),
        &headers,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let headers = [("relevo-context".to_string(), String::new())];
    let (status, v) = call(
        &t.app,
        "GET",
        "/api/inbox",
        Some(&token),
        &headers,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v["session"],
        json!({ "label": "laptop", "context": null, "you": "ana" })
    );
    // A bad token is still UNAUTHORIZED, whatever the headers say.
    let (status, _) = call(
        &t.app,
        "GET",
        "/api/inbox",
        Some("rlv_nope"),
        &[("relevo-session".to_string(), "x".to_string())],
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Nothing was sent by the rejected requests.
    assert!(thread_ids(&t.inbox("bob").await).is_empty());
}

#[tokio::test]
async fn inbox_names_its_session_and_the_latest_context_wins() {
    let t = team(&["ana", "bob"]);
    let v = t.inbox("ana").await;
    assert_eq!(
        v["session"],
        json!({ "label": "laptop", "context": null, "you": "ana" })
    );
    // `you` names the caller's person among `team`.
    let v = t.agent("bob", "term-0009", "frontend").inbox().await;
    assert_eq!(v["session"]["you"], "bob");
    assert!(
        v["team"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["handle"] == "bob")
    );
    let v = t.agent("ana", "term-0001", "pagos-api").inbox().await;
    assert_eq!(
        v["session"],
        json!({ "label": "laptop", "context": "pagos-api", "you": "ana" })
    );
    // Contexts are lowercased and trimmed.
    let v = t.agent("ana", "term-0001", " Front.End ").inbox().await;
    assert_eq!(
        v["session"],
        json!({ "label": "laptop", "context": "front.end", "you": "ana" })
    );
    let v = t.agent("ana", "term-0001", "").inbox().await;
    assert_eq!(v["session"]["context"], Value::Null);
    let sessions = db::list_sessions(&t.app.db(), Some("ana"), T0).unwrap();
    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
async fn team_lists_contexts_seen_in_the_last_15_minutes() {
    let t = team(&["ana", "bob"]);
    t.agent("ana", "term-0001", "pagos-api").inbox().await;
    t.agent("ana", "term-0002", "frontend").inbox().await;
    t.agent("ana", "term-0003", "pagos-api").inbox().await;
    t.agent("ana", "term-0004", "").inbox().await;
    let (_, v) = t.get("bob", "/api/people").await;
    assert_eq!(v["team"][0]["contexts"], json!(["frontend", "pagos-api"]));
    assert_eq!(v["team"][1]["contexts"], json!([]));
    t.app.clock.advance(10 * 60);
    t.agent("ana", "term-0002", "frontend").inbox().await;
    t.app.clock.advance(5 * 60 - 1);
    assert_eq!(
        t.inbox("bob").await["team"][0]["contexts"],
        json!(["frontend", "pagos-api"])
    );
    t.app.clock.advance(1);
    assert_eq!(
        t.inbox("bob").await["team"][0]["contexts"],
        json!(["frontend"])
    );
    // The session moved to another context: only the new one shows.
    t.agent("ana", "term-0002", "billing").inbox().await;
    assert_eq!(
        t.inbox("bob").await["team"][0]["contexts"],
        json!(["billing"])
    );
}

#[tokio::test]
async fn the_same_session_id_under_two_tokens_is_two_sessions() {
    let t = team(&["ana"]);
    let second = db::add_token(&t.app.db(), "ana", "Codex desktop", T0).unwrap();
    t.agent("ana", "same-id-1", "pagos-api").inbox().await;
    let other = Agent {
        app: t.app.clone(),
        token: second.secret,
        headers: vec![
            ("relevo-session".into(), "same-id-1".into()),
            ("relevo-context".into(), "frontend".into()),
        ],
    };
    let v = other.inbox().await;
    assert_eq!(
        v["session"],
        json!({ "label": "Codex desktop", "context": "frontend", "you": "ana" })
    );
    let sessions = db::list_sessions(&t.app.db(), Some("ana"), T0).unwrap();
    let mut seen: Vec<(String, Option<String>)> = sessions
        .iter()
        .map(|s| (s.label.clone(), s.context.clone()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("Codex desktop".to_string(), Some("frontend".to_string())),
            ("laptop".to_string(), Some("pagos-api".to_string())),
        ]
    );
}

#[tokio::test]
async fn sessions_not_seen_for_7_days_are_forgotten() {
    let t = team(&["ana", "bob"]);
    t.agent("ana", "term-0001", "pagos-api").inbox().await;
    t.app.clock.advance(7 * 24 * 3600 - 1);
    t.inbox("bob").await;
    assert_eq!(
        db::list_sessions(&t.app.db(), Some("ana"), T0 + 7 * 24 * 3600 - 1)
            .unwrap()
            .len(),
        1
    );
    t.app.clock.advance(1);
    t.inbox("bob").await;
    let left: i64 = t
        .app
        .db()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 1, "only bob's default session is left");
}

#[tokio::test]
async fn failed_requests_and_replays_still_count_as_seen() {
    let t = team(&["ana", "bob"]);
    let seen = |at: i64| -> Vec<(Option<String>, i64)> {
        db::list_sessions(&t.app.db(), Some("bob"), at)
            .unwrap()
            .into_iter()
            .map(|s| (s.context, s.last_seen))
            .collect()
    };
    let pagos = t.agent("bob", "term-0001", "pagos-api");
    let (status, _) = pagos.send(json!({ "to": ["nobody"], "text": "q" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(seen(T0), vec![(Some("pagos-api".to_string()), T0)]);
    let (status, _) = pagos.get("/api/threads/r_aaaaaaaaaaaa").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A keyed send replayed later moves its session's last_seen too.
    pagos
        .ok(json!({ "to": ["ana"], "text": "q", "key": "k1" }))
        .await;
    t.app.clock.advance(3600);
    pagos
        .ok(json!({ "to": ["ana"], "text": "q", "key": "k1" }))
        .await;
    assert_eq!(seen(T0), vec![(Some("pagos-api".to_string()), T0 + 3600)]);
}

#[tokio::test]
async fn a_request_counts_as_seen_once_past_its_own_parameter_checks() {
    let t = team(&["ana", "bob"]);
    let sessions = || {
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .len()
    };
    let bob = t.agent("bob", "term-0001", "pagos-api");
    // Requests that fail their own parameter checks are not seen.
    for body in [
        json!({ "to": ["ana/-x"], "text": "q" }),
        json!({ "to": ["ana/pagos", "ana"], "text": "q" }),
        json!({ "to": ["bob"], "text": "q" }),
        json!({ "to": ["@team", "ana"], "text": "q" }),
        json!({ "to": ["@team/pagos"], "text": "q" }),
        json!({ "to": [], "text": "q" }),
        json!({ "to": ["ana"], "text": "  " }),
        json!({ "to": ["ana"], "text": "q", "key": "" }),
        json!({ "to": ["ana"], "thread": "r_aaaaaaaaaaaa", "text": "q" }),
        json!({ "to": ["ana"] }),
        json!({ "to": ["ana"], "text": "q", "extra": 1 }),
        json!(["ana"]),
    ] {
        let (status, v) = bob.send(body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(code(&v), "BAD_REQUEST", "{body}");
    }
    for uri in ["/api/inbox?all=yes", "/api/inbox?wait=61", "/api/inbox?x=1"] {
        assert_eq!(bob.get(uri).await.0, StatusCode::BAD_REQUEST, "{uri}");
    }
    assert_eq!(sessions(), 0);
    // Past them, a request is seen whatever happens next.
    let (_, v) = bob.send(json!({ "to": ["zed"], "text": "q" })).await;
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    assert_eq!(sessions(), 1);
    let waiters: Vec<_> = (0..10)
        .map(|i| {
            let bob = t.agent("bob", &format!("bridge-{i:02}"), "");
            tokio::spawn(async move { bob.get("/api/inbox?wait=60").await })
        })
        .collect();
    until_waiting(&t.app, 10).await;
    assert_eq!(sessions(), 11);
    let (status, _) = t
        .agent("bob", "bridge-10", "")
        .get("/api/inbox?wait=1")
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(sessions(), 12);
    for waiter in waiters {
        waiter.abort();
    }
}

// ---------- routing, claims and delivery per session ----------

/// The texts of the new notes of `id` in an inbox, or `None` when it does not show `id`.
fn new_texts(inbox: &Value, id: &str) -> Option<Vec<String>> {
    let th = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)?;
    Some(
        th["new_notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["text"].as_str().unwrap().to_string())
            .collect(),
    )
}

#[tokio::test]
async fn an_unaddressed_note_shows_in_every_session_until_one_answers() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    // Both sessions exist before the note lands.
    pagos.inbox().await;
    front.inbox().await;
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "refunds?" }))
        .await;
    let v = pagos.inbox().await;
    assert_eq!(new_texts(&v, &id), Some(vec!["refunds?".to_string()]));
    let th = &v["threads"][0];
    assert_eq!(th["context"], Value::Null);
    assert_eq!(th["claimed"], Value::Null);
    assert_eq!(th["waiting_for_you"], true);
    // The frontend session still gets it as new: pagos reading it did not deliver it there.
    assert_eq!(
        new_texts(&front.inbox().await, &id),
        Some(vec!["refunds?".to_string()])
    );

    // Frontend answers: it claims the thread, and pagos stops seeing it.
    front.ok(json!({ "thread": id, "text": "mine" })).await;
    assert_eq!(new_texts(&pagos.inbox().await, &id), None);
    t.ok("ana", json!({ "thread": id, "text": "thanks, and?" }))
        .await;
    assert_eq!(new_texts(&pagos.inbox().await, &id), None);
    assert!(
        thread_ids(&t.inbox("bob").await).is_empty(),
        "nor does a general one"
    );
    let v = front.inbox().await;
    assert_eq!(new_texts(&v, &id), Some(vec!["thanks, and?".to_string()]));
    assert_eq!(v["threads"][0]["claimed"], "this-session");

    // Pagos reads it whole and writes in it too: the note is written, but frontend keeps it.
    pagos.thread(&id).await;
    let (_, v) = pagos
        .send(json!({ "thread": id, "text": "I could take it" }))
        .await;
    assert_eq!(v["claimed"], "other-session");
    t.ok("ana", json!({ "thread": id, "text": "ok" })).await;
    assert_eq!(new_texts(&pagos.inbox().await, &id), None);
    assert_eq!(
        new_texts(&front.inbox().await, &id),
        Some(vec!["I could take it".to_string(), "ok".to_string()])
    );
}

#[tokio::test]
async fn a_note_to_a_context_shows_there_and_in_general_sessions_only() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("ana", "term-pagos", "pagos-api");
    let pagos2 = t.agent("ana", "term-pagos2", "pagos-api");
    let front = t.agent("ana", "term-front", "frontend");
    let general = t.agent("ana", "bot-00001", "");
    // The context needs no live session: the thread waits for one.
    let id = t
        .ok(
            "bob",
            json!({ "to": ["Ana/Pagos-API"], "text": "refunds?" }),
        )
        .await;
    assert!(thread_ids(&front.inbox().await).is_empty());
    for agent in [&pagos, &pagos2, &general] {
        let v = agent.inbox().await;
        assert_eq!(thread_ids(&v), vec![id.clone()]);
        assert_eq!(v["threads"][0]["context"], "pagos-api");
    }
    assert_eq!(
        t.inbox("ana").await["threads"][0]["id"],
        id,
        "default session is general"
    );
    // The writer sees no context of theirs on it.
    assert_eq!(t.thread("bob", &id).await["context"], Value::Null);

    // A session that moves into the context sees it.
    let moved = t.agent("ana", "term-front", "pagos-api");
    assert_eq!(thread_ids(&moved.inbox().await), vec![id.clone()]);
}

#[tokio::test]
async fn a_note_to_a_repository_reaches_it_on_any_branch_and_one_to_a_branch_only_there() {
    let t = team(&["ana", "bob"]);
    let refunds = t.agent("ana", "term-refunds", "Pagos-API@Feat/Refunds");
    let main = t.agent("ana", "term-main", "pagos-api@main");
    let repo = t.agent("ana", "term-repo", "pagos-api");
    let front = t.agent("ana", "term-front", "frontend@feat/refunds");
    let general = t.agent("ana", "bot-00001", "");
    assert_eq!(
        refunds.inbox().await["session"]["context"],
        "pagos-api@feat/refunds"
    );
    // An address splits at its first `/`: the rest, slashes and all, is the context.
    let to_branch = t
        .ok(
            "bob",
            json!({ "to": ["ana/pagos-api@feat/refunds"], "text": "branch?" }),
        )
        .await;
    assert_eq!(
        t.thread("ana", &to_branch).await["context"],
        "pagos-api@feat/refunds"
    );
    let to_repo = t
        .ok("bob", json!({ "to": ["ana/pagos-api"], "text": "repo?" }))
        .await;
    // The repository alone reaches it on any branch, and without one.
    assert_eq!(
        thread_ids(&refunds.inbox().await),
        vec![to_repo.clone(), to_branch.clone()]
    );
    for agent in [&main, &repo] {
        assert_eq!(thread_ids(&agent.inbox().await), vec![to_repo.clone()]);
    }
    // A branch reaches only itself: not another branch, nor the same branch of another repository.
    assert!(thread_ids(&front.inbox().await).is_empty());
    // General sessions see both.
    assert_eq!(
        thread_ids(&general.inbox().await),
        vec![to_repo.clone(), to_branch.clone()]
    );
    assert_eq!(
        thread_ids(&t.inbox("ana").await),
        vec![to_repo.clone(), to_branch.clone()]
    );
    // `all` shows both anywhere, and the team list names branches as sessions sent them. The
    // `main` session started after the note to the branch had reached Ana's other sessions, but
    // she has not handled it, so it is new there.
    assert_eq!(
        thread_ids(&main.inbox_at("/api/inbox?all=1").await),
        vec![to_branch, to_repo]
    );
    let (_, v) = t.get("bob", "/api/people").await;
    assert_eq!(
        v["team"][0]["contexts"],
        json!([
            "frontend@feat/refunds",
            "pagos-api",
            "pagos-api@feat/refunds",
            "pagos-api@main"
        ])
    );
}

#[tokio::test]
async fn addresses_with_contexts_are_checked_and_merged() {
    let t = team(&["ana", "bob", "carl"]);
    for to in [
        json!(["ana/pagos-api", "ana/frontend"]),
        json!(["ana", "ana/pagos-api"]),
        json!(["ana/"]),
        json!(["ana/-x"]),
        json!(["ana/a b"]),
        json!(["ana/pagos/api"]),
        json!(["ana/pagos-api@"]),
        json!(["ana/@main"]),
        json!(["ana/pagos-api@feat refunds"]),
        json!(["ana/pagos-api@main", "ana/pagos-api"]),
        json!(["ana/pagos-api@main", "ana/pagos-api@dev"]),
        json!([format!("ana/pagos-api@{}", "b".repeat(101))]),
        json!(["@team/pagos-api"]),
        json!(["bob/frontend"]),
    ] {
        let (status, v) = t.send("bob", json!({ "to": to, "text": "x" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{to}");
        assert_eq!(code(&v), "BAD_REQUEST", "{to}: {v}");
    }
    let (status, v) = t
        .send("bob", json!({ "to": ["zed/pagos-api"], "text": "x" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "UNKNOWN_PERSON");
    let id = t
        .ok(
            "bob",
            json!({ "to": ["ana/pagos-api", "@Ana/PAGOS-API", "carl/web@Feat/X"], "text": "x" }),
        )
        .await;
    assert_eq!(
        t.thread("bob", &id).await["participants"],
        json!(["bob", "ana", "carl"])
    );
    assert_eq!(t.thread("ana", &id).await["context"], "pagos-api");
    assert_eq!(t.thread("carl", &id).await["context"], "web@feat/x");
    let id = t.ok("bob", json!({ "to": ["carl"], "text": "x" })).await;
    assert_eq!(t.thread("carl", &id).await["context"], Value::Null);
}

#[tokio::test]
async fn all_shows_every_thread_of_the_person_and_marks_other_claims() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    pagos.inbox().await;
    let to_front = t
        .ok("ana", json!({ "to": ["bob/frontend"], "text": "css?" }))
        .await;
    let claimed = t
        .ok("ana", json!({ "to": ["bob"], "text": "deploy?" }))
        .await;
    front.inbox().await;
    front
        .ok(json!({ "thread": claimed, "text": "answer" }))
        .await;
    t.ok("ana", json!({ "thread": claimed, "text": "thanks" }))
        .await;
    assert!(thread_ids(&pagos.inbox().await).is_empty());

    for uri in ["/api/inbox?all=1", "/api/inbox?all=true"] {
        let v = t
            .agent("bob", "term-pagos", "pagos-api")
            .inbox_at(uri)
            .await;
        assert_eq!(
            thread_ids(&v),
            vec![claimed.clone(), to_front.clone()],
            "{uri}"
        );
        assert_eq!(v["threads"][0]["claimed"], "other-session");
        assert_eq!(v["threads"][1]["claimed"], Value::Null);
        assert_eq!(v["threads"][1]["context"], "frontend");
    }
    // The first call delivered them to pagos; the second showed them as only waiting.
    let v = pagos.inbox_at("/api/inbox?all=1").await;
    assert_eq!(new_texts(&v, &claimed), Some(vec![]));
    assert_eq!(new_texts(&v, &to_front), Some(vec![]));
    // all=0 is the routed inbox.
    assert!(thread_ids(&pagos.inbox_at("/api/inbox?all=0").await).is_empty());
    // Delivered to pagos only: frontend still gets the new note.
    assert_eq!(
        new_texts(&front.inbox().await, &claimed),
        Some(vec!["thanks".to_string()])
    );
    for uri in [
        "/api/inbox?all=yes",
        "/api/inbox?all=",
        "/api/inbox?all=1&all=1",
        "/api/inbox?al=1",
    ] {
        let (status, v) = pagos.get(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(code(&v), "BAD_REQUEST");
    }
}

#[tokio::test]
async fn thread_works_from_any_session_and_does_not_claim() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    pagos.inbox().await;
    let id = t
        .ok(
            "ana",
            json!({ "to": ["bob/pagos-api"], "text": "refunds?" }),
        )
        .await;
    // Frontend does not see it routed, but may read it whole.
    assert!(thread_ids(&front.inbox().await).is_empty());
    let th = front.thread(&id).await;
    assert_eq!(th["notes"][0]["text"], "refunds?");
    assert_eq!(th["claimed"], Value::Null);
    assert_eq!(th["context"], "pagos-api");
    // Reading did not claim it: pagos still sees it, and as new.
    assert_eq!(
        new_texts(&pagos.inbox().await, &id),
        Some(vec!["refunds?".to_string()])
    );
    // Once pagos claims it, thread shows whose it is from either session.
    pagos.ok(json!({ "thread": id, "text": "yes" })).await;
    assert_eq!(pagos.thread(&id).await["claimed"], "this-session");
    assert_eq!(front.thread(&id).await["claimed"], "other-session");
    // Frontend writing does not take it from pagos.
    front
        .ok(json!({ "thread": id, "text": "actually me" }))
        .await;
    assert_eq!(pagos.thread(&id).await["claimed"], "this-session");
    assert_eq!(front.thread(&id).await["claimed"], "other-session");
}

#[tokio::test]
async fn a_second_session_writing_gets_other_session_and_the_claim_stays() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    pagos.inbox().await;
    front.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "who?" })).await;
    // Both see it, and both write.
    pagos.inbox().await;
    front.inbox().await;
    let (_, first) = pagos.send(json!({ "thread": id, "text": "answer" })).await;
    assert_eq!(first["claimed"], "this-session");
    let (_, second) = front.send(json!({ "thread": id, "text": "answer" })).await;
    assert_eq!(second["claimed"], "other-session");
    assert!(
        second["note"].is_string(),
        "the note is written all the same"
    );
    // The first writer holds it, and writing again keeps it there.
    let (_, again) = pagos.send(json!({ "thread": id, "text": "done" })).await;
    assert_eq!(again["claimed"], "this-session");
    let (_, again) = front.send(json!({ "thread": id, "text": "me too" })).await;
    assert_eq!(again["claimed"], "other-session");
    t.ok("ana", json!({ "thread": id, "text": "thanks" })).await;
    assert_eq!(
        thread_ids(&pagos.inbox().await),
        vec![id.clone()],
        "the claimer sees it"
    );
    assert!(thread_ids(&front.inbox().await).is_empty());
    assert!(thread_ids(&t.inbox("bob").await).is_empty());
    let d = &t.thread("ana", &id).await["delivery"][1];
    assert_eq!(
        d["claimed_by"],
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:41:00Z" })
    );
    // The other person's claim is their own: Ana's session holds hers, opened by writing.
    let (_, v) = t.send("ana", json!({ "thread": id, "text": "x" })).await;
    assert_eq!(v["claimed"], "this-session");
}

#[tokio::test]
async fn send_says_nobody_holds_a_thread_once_closed() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    // Nobody of Bob's holds it: null, and closing does not claim.
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let (status, v) = front.send(json!({ "thread": id, "close": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "closed", "claimed": null })
    );
    // Another session held it: closing released it, so null too.
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    let (_, v) = front
        .send(json!({ "thread": id, "close": true, "key": "c-1" }))
        .await;
    assert_eq!(v["note"], Value::Null);
    assert_eq!(v["claimed"], Value::Null);
    // A replay returns the first result, claim included.
    let (_, again) = front
        .send(json!({ "thread": id, "close": true, "key": "c-1" }))
        .await;
    assert_eq!(again, v);
    // This session held it: null, and with a closing note too.
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    let (_, v) = pagos
        .send(json!({ "thread": id, "text": "done", "close": true }))
        .await;
    assert_eq!(v["claimed"], Value::Null);
    assert!(v["note"].is_string(), "{v}");
}

#[tokio::test]
async fn closing_without_text_does_not_claim() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.ok(json!({ "thread": id, "close": true })).await;
    assert_eq!(pagos.thread(&id).await["claimed"], Value::Null);
}

#[tokio::test]
async fn inbox_threads_say_who_else_holds_them() {
    let t = team(&["ana", "bob", "carl"]);
    let second = db::add_token(&t.app.db(), "bob", "Codex desktop", T0).unwrap();
    let ana = t.agent("ana", "term-ana", "web");
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let codex = Agent {
        app: t.app.clone(),
        token: second.secret,
        headers: vec![
            ("relevo-session".into(), "codex-front".into()),
            ("relevo-context".into(), "frontend".into()),
        ],
    };
    let id = ana.ok(json!({ "to": ["bob", "carl"], "text": "q" })).await;
    // Nobody holds it yet.
    let inbox = pagos.inbox().await;
    assert_eq!(inbox["threads"][0]["held_by"], json!([]), "{inbox}");
    // Bob's agent answers from pagos-api at 10:47: the asker sees who is on it, with the
    // session's label and context and since when, without opening the thread.
    t.app.clock.advance(6 * 60);
    pagos.ok(json!({ "thread": id, "text": "Looking." })).await;
    let bob_holds = json!({ "handle": "bob", "label": "laptop", "context": "pagos-api",
                           "since": "2026-11-08T10:47:00Z" });
    let inbox = ana.inbox().await;
    assert_eq!(thread_ids(&inbox), vec![id.clone()]);
    assert_eq!(
        inbox["threads"][0]["held_by"],
        json!([bob_holds]),
        "{inbox}"
    );
    // `thread` returns the same object.
    assert_eq!(ana.thread(&id).await["held_by"], json!([bob_holds]));
    // Carl claims too, later: in participant order, since when each holds it.
    t.app.clock.advance(60);
    t.agent("carl", "term-carl", "")
        .ok(json!({ "thread": id, "claim": true }))
        .await;
    let carl_holds = json!({ "handle": "carl", "label": "laptop", "context": null,
                            "since": "2026-11-08T10:48:00Z" });
    let inbox = ana.inbox().await;
    assert_eq!(
        inbox["threads"][0]["held_by"],
        json!([bob_holds, carl_holds]),
        "{inbox}"
    );
    // Ana writing holds it for her, which Bob's sessions see, with her session's context. Only
    // the others: Bob's own session is in `claimed`, not in `held_by`.
    ana.ok(json!({ "thread": id, "text": "Thanks." })).await;
    let ana_holds = json!({ "handle": "ana", "label": "laptop", "context": "web",
                           "since": "2026-11-08T10:48:00Z" });
    let held = &codex.inbox_at("/api/inbox?all=1").await["threads"][0];
    assert_eq!(held["claimed"], "other-session", "{held}");
    assert_eq!(held["held_by"], json!([ana_holds, carl_holds]), "{held}");
    let held = &pagos.inbox().await["threads"][0];
    assert_eq!(held["claimed"], "this-session", "{held}");
    assert_eq!(held["held_by"], json!([ana_holds, carl_holds]), "{held}");
    // Live claims only: once Ana's and Carl's sessions have not been seen for 15 minutes,
    // theirs are gone, and Bob's, seen since, stays.
    t.app.clock.advance(LAPSE - 60);
    pagos.inbox().await;
    t.app.clock.advance(60);
    let held = &pagos.inbox().await["threads"][0];
    assert_eq!(held["held_by"], json!([]), "{held}");
    let inbox = ana.inbox().await;
    assert_eq!(inbox["threads"], json!([]), "{inbox}");
    assert_eq!(ana.thread(&id).await["held_by"], json!([bob_holds]));
}

#[tokio::test]
async fn closing_releases_every_claim_on_the_thread() {
    let t = team(&["ana", "bob", "carl"]);
    let ana = t.agent("ana", "term-ana", "web");
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let carl = t.agent("carl", "term-carl", "");
    for agent in [&pagos, &front] {
        agent.inbox().await;
    }
    let id = ana.ok(json!({ "to": ["bob", "carl"], "text": "q" })).await;
    // Bob's pagos session reads it (so his frontend session keeps a row of its own) and
    // answers, Carl claims it, Ana writes again: each of them holds it.
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    carl.ok(json!({ "thread": id, "claim": true })).await;
    ana.ok(json!({ "thread": id, "text": "and?" })).await;
    assert!(thread_ids(&front.inbox().await).is_empty());
    let delivery = &t.thread("ana", &id).await["delivery"];
    for d in delivery.as_array().unwrap() {
        assert!(d["claimed_by"].is_object(), "{delivery}");
    }
    // Carl closes it, with a last note: nobody holds it any more, for any participant.
    t.app.clock.advance(60);
    let (_, v) = carl
        .send(json!({ "thread": id, "text": "Done here.", "close": true }))
        .await;
    assert_eq!(v["claimed"], Value::Null, "{v}");
    let th = ana.thread(&id).await;
    assert_eq!(th["status"], "closed");
    assert_eq!(th["claimed"], Value::Null);
    assert_eq!(th["held_by"], json!([]));
    for d in th["delivery"].as_array().unwrap() {
        assert_eq!(d["claimed_by"], Value::Null, "{d}");
    }
    assert_eq!(pagos.thread(&id).await["claimed"], Value::Null);
    // Bob's other session, which the claim kept it from, now gets its closing notes.
    assert_eq!(
        new_texts(&front.inbox().await, &id),
        Some(vec![
            "q".to_string(),
            "mine".to_string(),
            "and?".to_string(),
            "Done here.".to_string()
        ])
    );
    assert_eq!(front.thread(&id).await["claimed"], Value::Null);
}

#[tokio::test]
async fn closing_without_text_releases_the_claims_too_and_wakes_who_held_it() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    pagos.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "claim": true })).await;
    t.ok("ana", json!({ "thread": id, "text": "more" })).await;
    // Bob's frontend session waits; the thread is pagos's, so nothing there is new for it.
    let waiter = tokio::spawn({
        let front = front.clone();
        async move { front.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    // Ana closes it without a note: the claim is released, and the waiting request gets it.
    t.ok("ana", json!({ "thread": id, "close": true })).await;
    let v = tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
        .await
        .expect("closing wakes the sessions of whoever held the thread")
        .unwrap();
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["q".to_string(), "more".to_string()]),
        "{v}"
    );
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
}

// ---------- claiming without a note ----------

#[tokio::test]
async fn claim_without_text_claims_the_thread_and_writes_nothing() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.inbox().await;
    t.app.clock.advance(6 * 60);
    let (status, v) = pagos.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "open", "claimed": "this-session" })
    );
    // No note: the thread is as it was, still waiting for Bob, and nothing is new for Ana.
    let th = t.thread("ana", &id).await;
    assert_eq!(th["notes"].as_array().unwrap().len(), 1);
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
    // A state Ana sees: who holds it, and since when.
    assert_eq!(
        th["delivery"][1]["claimed_by"],
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:47:00Z" })
    );
    // The thread leaves Bob's other sessions and stays in the claiming one.
    assert!(thread_ids(&front.inbox().await).is_empty());
    assert_eq!(thread_ids(&pagos.inbox().await), vec![id.clone()]);
    assert_eq!(pagos.thread(&id).await["claimed"], "this-session");
    assert_eq!(front.thread(&id).await["claimed"], "other-session");
    // Ana's answer reaches the claiming session only.
    t.ok("ana", json!({ "thread": id, "text": "more" })).await;
    assert!(thread_ids(&front.inbox().await).is_empty());
    assert_eq!(
        new_texts(&pagos.inbox().await, &id),
        Some(vec!["more".to_string()])
    );
}

#[tokio::test]
async fn claim_with_text_writes_the_note_and_claims() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let (status, v) = pagos
        .send(json!({ "thread": id, "text": "answer", "claim": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["note"].is_string());
    assert_eq!(v["claimed"], "this-session");
    assert_eq!(
        new_texts(&t.inbox("ana").await, &id),
        Some(vec!["answer".to_string()])
    );
}

#[tokio::test]
async fn claim_never_takes_a_live_claim_and_claiming_again_keeps_since() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let (_, v) = pagos.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(v["claimed"], "this-session");
    // Another session's claim, with or without text, leaves it where it is.
    t.app.clock.advance(60);
    let (status, v) = front.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "open", "claimed": "other-session" })
    );
    let (_, v) = front
        .send(json!({ "thread": id, "text": "me?", "claim": true }))
        .await;
    assert_eq!(v["claimed"], "other-session");
    assert!(v["note"].is_string(), "the note is written all the same");
    // The holder claiming again, or writing, is not a new claim.
    t.app.clock.advance(60);
    let (_, v) = pagos.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(v["claimed"], "this-session");
    pagos.ok(json!({ "thread": id, "text": "done soon" })).await;
    let pagos_claim =
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:41:00Z" });
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        pagos_claim
    );
    // Once pagos's claim has lapsed, a claim takes it, from then.
    t.app.clock.advance(LAPSE);
    let (_, v) = front.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(v["claimed"], "this-session");
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        json!({ "label": "laptop", "context": "frontend", "since": "2026-11-08T10:58:00Z" })
    );
    let (_, v) = pagos.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(v["claimed"], "other-session");
}

#[tokio::test]
async fn a_claim_without_text_ends_with_its_session() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.ok(json!({ "thread": id, "claim": true })).await;
    assert!(thread_ids(&front.inbox().await).is_empty());
    pagos.end().await;
    assert_eq!(thread_ids(&front.inbox().await), vec![id.clone()]);
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
}

#[tokio::test]
async fn claim_without_text_is_idempotent_with_a_key() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let body = json!({ "thread": id, "claim": true, "key": "claim-1" });
    let (_, first) = pagos.send(body.clone()).await;
    assert_eq!(
        first,
        json!({ "thread": id, "note": null, "status": "open", "claimed": "this-session" })
    );
    let (status, again) = pagos.send(body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, first);
    // A retry from another session gets the first result, named for that session, and claims
    // nothing.
    let (_, again) = front.send(body).await;
    assert_eq!(again["claimed"], "other-session");
    assert_eq!(again["note"], Value::Null);
    assert_eq!(front.thread(&id).await["claimed"], "other-session");
    // The key is used up by the claim: a note with it replays the claim and writes nothing.
    let (_, again) = pagos
        .send(json!({ "thread": id, "text": "x", "key": "claim-1" }))
        .await;
    assert_eq!(again, first);
    assert_eq!(
        t.thread("ana", &id).await["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn claim_without_text_follows_the_rules_of_writing_in_a_thread() {
    let t = team(&["ana", "bob", "carl"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    // Not a participant: not found.
    let (status, v) = t.send("carl", json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    // Claiming and closing together, without text, closes: and closing releases the claim.
    let (_, v) = t
        .send("bob", json!({ "thread": id, "claim": true, "close": true }))
        .await;
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "closed", "claimed": null })
    );
    // A closed thread cannot be claimed.
    let (status, v) = t.send("bob", json!({ "thread": id, "claim": true })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "THREAD_CLOSED");
}

#[tokio::test]
async fn opening_a_thread_claims_nothing() {
    let t = team(&["ana", "oscar"]);
    let a = t.agent("oscar", "term-aaaa", "relevo");
    let (status, v) = a
        .send(json!({ "to": ["ana/pagos-api"], "text": "q" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["claimed"], Value::Null);
    let id = v["thread"].as_str().unwrap().to_string();
    let th = a.thread(&id).await;
    assert_eq!(th["claimed"], Value::Null);
    assert_eq!(th["delivery"][0]["handle"], "oscar");
    assert_eq!(th["delivery"][0]["claimed_by"], Value::Null);
    // Nor does asking the team.
    let (_, v) = a.send(json!({ "to": ["@team"], "text": "who?" })).await;
    assert_eq!(v["claimed"], Value::Null);
    let team_id = v["thread"].as_str().unwrap().to_string();
    assert_eq!(a.thread(&team_id).await["claimed"], Value::Null);
    // A replayed open says the same.
    let (_, first) = a
        .send(json!({ "to": ["ana"], "text": "k", "key": "open-1" }))
        .await;
    let (_, again) = a
        .send(json!({ "to": ["ana"], "text": "k", "key": "open-1" }))
        .await;
    assert_eq!(first["claimed"], Value::Null);
    assert_eq!(again, first);
    // Writing in the thread afterwards claims it, as any note in an existing thread does.
    let (_, v) = a.send(json!({ "thread": id, "text": "more" })).await;
    assert_eq!(v["claimed"], "this-session");
    let b = t.agent("oscar", "term-bbbb", "relevo");
    assert_eq!(b.thread(&id).await["claimed"], "other-session");
}

#[tokio::test]
async fn the_answer_to_a_question_reaches_the_askers_next_session() {
    let t = team(&["ana", "oscar"]);
    // Oscar's session A asks, and exits.
    let a = t.agent("oscar", "term-aaaa", "relevo");
    let id = a
        .ok(json!({ "to": ["ana"], "text": "Is refunds live?" }))
        .await;
    let team_id = a
        .ok(json!({ "to": ["@team"], "text": "Who owns billing?" }))
        .await;
    // Ana replies in both.
    t.ok("ana", json!({ "thread": id, "text": "Yes, since Monday." }))
        .await;
    t.ok("ana", json!({ "thread": team_id, "text": "Me." }))
        .await;
    // Oscar's new session B, right away, sees both answers in its plain inbox.
    let b = t.agent("oscar", "term-bbbb", "relevo");
    let v = b.inbox().await;
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["Yes, since Monday.".to_string()]),
        "{v}"
    );
    assert_eq!(
        new_texts(&v, &team_id),
        Some(vec!["Me.".to_string()]),
        "{v}"
    );
    for th in v["threads"].as_array().unwrap() {
        assert_eq!(th["claimed"], Value::Null, "{th}");
    }
    let direct = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|th| th["id"] == id)
        .unwrap();
    assert_eq!(direct["waiting_for_you"], true);
    // Unclaimed, it shows in his other sessions too, in any context, waiting for his reply. The
    // answer reached B, but Oscar has not handled it, so they start before it: it is new there.
    for (sid, context) in [("term-cccc", "frontend"), ("term-dddd", "")] {
        let v = t.agent("oscar", sid, context).inbox().await;
        assert_eq!(
            new_texts(&v, &id),
            Some(vec!["Yes, since Monday.".to_string()]),
            "{v}"
        );
    }
}

/// The `last_note` of thread `id` in an inbox: (author, text, truncated).
fn last_note(inbox: &Value, id: &str) -> (String, String, bool) {
    let th = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {inbox}"));
    let note = &th["last_note"];
    (
        note["author"].as_str().unwrap().to_string(),
        note["text"].as_str().unwrap().to_string(),
        note["truncated"].as_bool().unwrap(),
    )
}

#[tokio::test]
async fn a_new_session_sees_the_answer_another_session_fetched_as_last_note() {
    let t = team(&["ana", "oscar"]);
    // Oscar's session A asks Ana, Ana answers, and A fetches the answer.
    let a = t.agent("oscar", "term-aaaa", "relevo");
    let id = a
        .ok(json!({ "to": ["ana"], "text": "Is refunds live?" }))
        .await;
    t.ok("ana", json!({ "thread": id, "text": "Yes, since Monday." }))
        .await;
    assert_eq!(
        new_texts(&a.inbox().await, &id),
        Some(vec!["Yes, since Monday.".to_string()])
    );
    // Oscar's new session B gets the answer as new: Oscar has not handled it. Once B has it,
    // nothing is new, but the thread waits for him, and its last note is Ana's answer.
    let b = t.agent("oscar", "term-bbbb", "");
    assert_eq!(
        new_texts(&b.inbox().await, &id),
        Some(vec!["Yes, since Monday.".to_string()])
    );
    let v = b.inbox().await;
    assert_eq!(new_texts(&v, &id), Some(vec![]), "{v}");
    assert_eq!(v["threads"][0]["waiting_for_you"], true, "{v}");
    assert_eq!(
        last_note(&v, &id),
        ("ana".into(), "Yes, since Monday.".into(), false)
    );
    let note = &v["threads"][0]["last_note"];
    for field in ["id", "via", "created_at"] {
        assert!(note[field].is_string(), "{note}");
    }
    // Showing it delivered nothing: the answer reached Oscar through A, as before.
    let th = t.thread("ana", &id).await;
    let oscar = th["delivery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["handle"] == "oscar")
        .unwrap();
    assert_eq!(oscar["by"]["context"], "relevo", "{th}");
    // It shows each time, as long as the thread waits.
    assert_eq!(
        last_note(&b.inbox().await, &id).1,
        "Yes, since Monday.".to_string()
    );
    // Whoever wrote it: once Oscar replies, Ana's inbox shows his reply as the last note.
    a.ok(json!({ "thread": id, "text": "Thanks!" })).await;
    let v = t.inbox("ana").await;
    assert_eq!(
        last_note(&v, &id),
        ("oscar".into(), "Thanks!".into(), false)
    );
}

#[tokio::test]
async fn team_threads_carry_their_last_note() {
    let t = team(&["ana", "bob", "carl"]);
    let id = t
        .ok(
            "ana",
            json!({ "to": ["@team"], "text": "Who owns billing?" }),
        )
        .await;
    let v = t.inbox("bob").await;
    assert_eq!(
        last_note(&v, &id),
        ("ana".into(), "Who owns billing?".into(), false)
    );
    t.ok("carl", json!({ "thread": id, "text": "I do." })).await;
    t.ok("bob", json!({ "thread": id, "text": "Carl does, yes." }))
        .await;
    // Ana gets both answers as new; the last note is the latest of them, whoever wrote it.
    let v = t.inbox("ana").await;
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["I do.".to_string(), "Carl does, yes.".to_string()])
    );
    assert_eq!(
        last_note(&v, &id),
        ("bob".into(), "Carl does, yes.".into(), false)
    );
}

#[tokio::test]
async fn last_note_is_truncated_like_new_notes() {
    let t = team(&["ana", "oscar"]);
    let a = t.agent("oscar", "term-aaaa", "");
    let id = a.ok(json!({ "to": ["ana"], "text": "Logs?" })).await;
    let text = "ñ".repeat(5000);
    t.ok("ana", json!({ "thread": id, "text": text })).await;
    a.inbox().await;
    let v = t.agent("oscar", "term-bbbb", "").inbox().await;
    let (author, cut, truncated) = last_note(&v, &id);
    assert_eq!(author, "ana");
    assert!(truncated);
    assert_eq!(cut.chars().count(), 4000);
    assert!(text.starts_with(&cut));
    let v = t.agent("oscar", "term-cccc", "").inbox().await;
    assert!(last_note(&v, &id).2);
    assert_eq!(t.thread("oscar", &id).await["notes"][1]["text"], text);
}

#[tokio::test]
async fn reading_in_one_session_does_not_deliver_to_another() {
    let t = team(&["ana", "bob"]);
    let first = t.agent("bob", "term-0001", "");
    let second = t.agent("bob", "term-0002", "");
    first.inbox().await;
    second.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "one" })).await;
    t.app.clock.advance(60);
    first.thread(&id).await;
    assert_eq!(
        new_texts(&second.inbox().await, &id),
        Some(vec!["one".to_string()])
    );
    assert_eq!(new_texts(&second.inbox().await, &id), Some(vec![]));
    t.ok("ana", json!({ "thread": id, "text": "two" })).await;
    assert_eq!(
        new_texts(&first.inbox().await, &id),
        Some(vec!["two".to_string()])
    );
    assert_eq!(
        new_texts(&second.inbox().await, &id),
        Some(vec!["two".to_string()])
    );
    // A session first seen after the notes reached the person starts with what the person has
    // handled: Bob has not handled them, so they are new for it.
    let later = t.agent("bob", "term-0003", "");
    assert_eq!(
        new_texts(&later.inbox().await, &id),
        Some(vec!["one".to_string(), "two".to_string()])
    );
    assert_eq!(new_texts(&later.inbox().await, &id), Some(vec![]));
    // And notes that reached no session of the person are new for it too.
    t.ok("ana", json!({ "thread": id, "text": "three" })).await;
    let fresh = t.agent("bob", "term-0004", "");
    assert_eq!(
        new_texts(&fresh.inbox().await, &id),
        Some(vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string()
        ])
    );
    // Once Bob has handled them (he replies), a new session starts with them.
    later.ok(json!({ "thread": id, "text": "got it" })).await;
    let after = t.agent("bob", "term-0005", "");
    assert!(thread_ids(&after.inbox().await).is_empty());
}

#[tokio::test]
async fn a_persons_own_notes_are_never_new_for_their_sessions() {
    let t = team(&["ana", "bob"]);
    let one = t.agent("ana", "term-0001", "");
    let two = t.agent("ana", "term-0002", "");
    two.inbox().await;
    one.ok(json!({ "to": ["bob"], "text": "hello" })).await;
    one.ok(json!({ "to": ["@team"], "text": "anyone?" })).await;
    assert!(thread_ids(&two.inbox().await).is_empty());
    assert!(thread_ids(&one.inbox().await).is_empty());
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
}

#[tokio::test]
async fn delivery_names_the_session_that_picked_up_and_the_one_that_claimed() {
    let t = team(&["ana", "bob"]);
    let second = db::add_token(&t.app.db(), "bob", "Codex desktop", T0).unwrap();
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let codex = Agent {
        app: t.app.clone(),
        token: second.secret,
        headers: vec![("relevo-context".into(), "frontend".into())],
    };
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    t.app.clock.advance(300);
    pagos.inbox().await;
    let d = &t.thread("ana", &id).await["delivery"][1];
    assert_eq!(d["handle"], "bob");
    assert_eq!(d["delivered_at"], "2026-11-08T10:46:00Z");
    assert_eq!(
        d["by"],
        json!({ "label": "laptop", "context": "pagos-api" })
    );
    assert_eq!(d["claimed_by"], Value::Null);
    let q = d["note"].clone();
    // Another session writing claims it; its own note does not move the delivery.
    t.app.clock.advance(60);
    codex.ok(json!({ "thread": id, "text": "mine" })).await;
    let d = &t.thread("ana", &id).await["delivery"][1];
    let pagos_out = json!({ "label": "laptop", "context": "pagos-api" });
    let codex_out = json!({ "label": "Codex desktop", "context": "frontend" });
    assert_eq!(d["note"], q);
    assert_eq!(d["delivered_at"], "2026-11-08T10:46:00Z");
    assert_eq!(d["by"], pagos_out);
    assert_eq!(
        d["claimed_by"],
        json!({ "label": "Codex desktop", "context": "frontend", "since": "2026-11-08T10:47:00Z" })
    );
    // A session reading what it already had, or only Bob's own notes, moves nothing.
    t.app.clock.advance(60);
    pagos.thread(&id).await;
    assert_eq!(t.thread("ana", &id).await["delivery"][1]["by"], pagos_out);
    // A note of Ana's reaching a session of Bob's does.
    let again = {
        let (_, v) = t.send("ana", json!({ "thread": id, "text": "and?" })).await;
        v["note"].clone()
    };
    t.app.clock.advance(60);
    codex.inbox().await;
    let d = &t.thread("ana", &id).await["delivery"][1];
    assert_eq!(d["note"], again);
    assert_eq!(d["delivered_at"], "2026-11-08T10:49:00Z");
    assert_eq!(d["by"], codex_out);
}

#[tokio::test]
async fn seen_by_counts_the_sessions_the_latest_delivered_note_reached() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    pagos.inbox().await;
    front.inbox().await;
    let seen_by = |v: Value| {
        (
            v["delivery"][1]["note"].clone(),
            v["delivery"][1]["seen_by"].clone(),
        )
    };
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    assert_eq!(seen_by(t.thread("ana", &id).await), (Value::Null, json!(0)));
    // Ana's own note reached the session that wrote it, but says nothing to others.
    assert_eq!(t.thread("ana", &id).await["delivery"][0]["seen_by"], 0);
    pagos.inbox().await;
    let (q, n) = seen_by(t.thread("ana", &id).await);
    assert!(q.is_string());
    assert_eq!(n, 1);
    front.inbox().await;
    assert_eq!(seen_by(t.thread("ana", &id).await), (q.clone(), json!(2)));
    // A note that reached no session of Bob's leaves the count on the latest that did.
    t.ok("ana", json!({ "thread": id, "text": "again" })).await;
    assert_eq!(seen_by(t.thread("ana", &id).await), (q.clone(), json!(2)));
    pagos.inbox().await;
    let (again, n) = seen_by(t.thread("ana", &id).await);
    assert_ne!(again, q);
    assert_eq!(n, 1);
    // A session first seen now starts with what Bob has, so it counts too.
    t.agent("bob", "term-new", "").inbox().await;
    assert_eq!(
        seen_by(t.thread("ana", &id).await),
        (again.clone(), json!(2))
    );
    front.thread(&id).await;
    assert_eq!(
        seen_by(t.thread("ana", &id).await),
        (again.clone(), json!(3))
    );
    // Nobody has taken it: there is no claim.
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
    // A forgotten session no longer counts.
    let laptop = db::list_tokens(&t.app.db(), Some("bob")).unwrap()[0]
        .id
        .clone();
    db::revoke_token(&t.app.db(), &laptop, T0).unwrap();
    assert_eq!(seen_by(t.thread("ana", &id).await), (again, json!(0)));
}

#[tokio::test]
async fn delivery_is_about_the_latest_note_by_someone_else() {
    let t = team(&["ana", "oscar"]);
    let a1 = t.agent("ana", "term-ana1", "pagos-api");
    let a2 = t.agent("ana", "term-ana2", "frontend");
    a1.inbox().await;
    a2.inbox().await;
    let id = t.ok("oscar", json!({ "to": ["ana"], "text": "q" })).await;
    // Oscar's own note says nothing about whether he read anyone's.
    let d = &t.thread("oscar", &id).await["delivery"][0];
    assert_eq!(
        (&d["note"], &d["delivered_at"], &d["by"], &d["seen_by"]),
        (&Value::Null, &Value::Null, &Value::Null, &json!(0))
    );
    let q = t.thread("oscar", &id).await["notes"][0]["id"].clone();
    t.app.clock.advance(60);
    a1.inbox().await;
    t.app.clock.advance(60);
    a2.inbox().await;
    t.app.clock.advance(60);
    a2.ok(json!({ "thread": id, "text": "answer" })).await;
    // Ana's own note, from a session that had Oscar's, does not move her delivery.
    let d = &t.thread("oscar", &id).await["delivery"][1];
    assert_eq!(d["note"], q);
    assert_eq!(d["delivered_at"], "2026-11-08T10:42:00Z");
    assert_eq!(
        d["by"],
        json!({ "label": "laptop", "context": "pagos-api" })
    );
    assert_eq!(
        d["claimed_by"],
        json!({ "label": "laptop", "context": "frontend", "since": "2026-11-08T10:44:00Z" })
    );
    assert_eq!(d["seen_by"], 2);
    // Nor does one from a session that writes again, or fetches her own notes.
    a1.ok(json!({ "thread": id, "text": "me too" })).await;
    t.agent("ana", "term-ana3", "").thread(&id).await;
    let d = &t.thread("oscar", &id).await["delivery"][1];
    assert_eq!(d["note"], q);
    assert_eq!(d["delivered_at"], "2026-11-08T10:42:00Z");
    assert_eq!(d["seen_by"], 3);
}

#[tokio::test]
async fn claimed_by_shows_the_context_the_session_claimed_in() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let pagos = t.agent("bob", "term-0001", "pagos-api");
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    // Later the same session works elsewhere: the thread does not follow it there.
    t.app.clock.advance(10 * 60);
    t.agent("bob", "term-0001", "acquisition").inbox().await;
    let d = &t.thread("ana", &id).await["delivery"][1];
    assert_eq!(
        d["by"],
        json!({ "label": "laptop", "context": "pagos-api" })
    );
    assert_eq!(
        d["claimed_by"],
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:41:00Z" })
    );
}

#[tokio::test]
async fn the_holder_writing_from_another_context_keeps_the_claims_first_context() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let pagos = t.agent("bob", "term-0001", "pagos-api");
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    // The same session, now elsewhere, writes again: that is not a new claim, nor a later one.
    t.app.clock.advance(5 * 60);
    let (_, v) = t
        .agent("bob", "term-0001", "acquisition")
        .send(json!({ "thread": id, "text": "still mine" }))
        .await;
    assert_eq!(v["claimed"], "this-session");
    let d = &t.thread("ana", &id).await["delivery"][1];
    assert_eq!(
        d["claimed_by"],
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:41:00Z" })
    );
}

#[tokio::test]
async fn a_session_seen_within_15_minutes_keeps_its_claim_until_it_stops() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    front.inbox().await;
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    // A day of a session seen every 14 minutes: the claim holds.
    for _ in 0..(24 * 60 / 14 + 1) {
        t.app.clock.advance(LAPSE - 60);
        pagos.get("/api/people").await;
        assert!(thread_ids(&front.inbox().await).is_empty());
    }
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    assert!(thread_ids(&front.inbox().await).is_empty());
    // Pagos stops: 15 minutes later its claim has lapsed.
    t.app.clock.advance(LAPSE);
    let v = front.inbox().await;
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["mine".to_string(), "and?".to_string()])
    );
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
    // A week after it was last seen it is forgotten; coming back, it is a new session.
    t.app.clock.advance(7 * 24 * 3600 - LAPSE);
    assert_eq!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .len(),
        2
    );
    t.inbox("ana").await;
    assert_eq!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(pagos.thread(&id).await["claimed"], Value::Null);
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
}

#[tokio::test]
async fn a_claim_lapses_15_minutes_after_its_session_was_last_seen_and_the_next_writer_takes_it() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    // Others' requests do not keep it alive, nor do they lose it for pagos before 15 minutes.
    t.app.clock.advance(LAPSE - 1);
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    let (_, v) = front.send(json!({ "thread": id, "text": "me?" })).await;
    assert_eq!(v["claimed"], "other-session");
    assert!(thread_ids(&front.inbox().await).is_empty());
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        json!({ "label": "laptop", "context": "pagos-api", "since": "2026-11-08T10:41:00Z" })
    );
    // 15 minutes after pagos was last seen its claim has lapsed, and counts as unclaimed
    // everywhere: the thread shows in Bob's other sessions, nobody holds it.
    t.app.clock.advance(1);
    let v = front.inbox().await;
    assert_eq!(
        new_texts(&v, &id),
        Some(vec![
            "q".to_string(),
            "mine".to_string(),
            "and?".to_string(),
            "me?".to_string()
        ])
    );
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
    // Pagos coming back does not get it back by being seen.
    assert_eq!(pagos.thread(&id).await["claimed"], Value::Null);
    let v = pagos.inbox_at("/api/inbox?all=1").await;
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
    // The next writer takes it, and keeps it from pagos.
    let (_, v) = front
        .send(json!({ "thread": id, "text": "taking it" }))
        .await;
    assert_eq!(v["claimed"], "this-session");
    // Its claim starts then, not when pagos's did.
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        json!({ "label": "laptop", "context": "frontend", "since": "2026-11-08T10:56:00Z" })
    );
    let (_, v) = pagos.send(json!({ "thread": id, "text": "back" })).await;
    assert_eq!(v["claimed"], "other-session");
    t.ok("ana", json!({ "thread": id, "text": "ok" })).await;
    assert!(thread_ids(&pagos.inbox().await).is_empty());
    assert_eq!(thread_ids(&front.inbox().await), vec![id]);
}

#[tokio::test]
async fn a_session_that_wrote_in_its_own_question_and_exited_holds_it_only_15_minutes() {
    let t = team(&["ana", "oscar"]);
    let a = t.agent("oscar", "term-aaaa", "relevo");
    let id = a.ok(json!({ "to": ["ana"], "text": "q" })).await;
    // A follow-up is a note in an existing thread: it claims.
    let (_, v) = a.send(json!({ "thread": id, "text": "also" })).await;
    assert_eq!(v["claimed"], "this-session");
    // A exits. Ana answers; Oscar's next session does not see it while the claim lives...
    t.ok("ana", json!({ "thread": id, "text": "answer" })).await;
    let b = t.agent("oscar", "term-bbbb", "relevo");
    t.app.clock.advance(LAPSE - 1);
    assert!(thread_ids(&b.inbox().await).is_empty());
    // ...and does 15 minutes after A was last seen.
    t.app.clock.advance(1);
    let v = b.inbox().await;
    assert_eq!(new_texts(&v, &id), Some(vec!["answer".to_string()]), "{v}");
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
}

/// The ids of the threads in `who`'s inbox, as the default session of the token `secret`.
async fn inbox_of(t: &Team, secret: &str) -> Vec<String> {
    let agent = Agent {
        app: t.app.clone(),
        token: secret.to_string(),
        headers: Vec::new(),
    };
    thread_ids(&agent.inbox().await)
}

#[tokio::test]
async fn a_revoked_tokens_sessions_are_forgotten_with_their_claims() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    t.inbox("bob").await;
    t.ok("bob", json!({ "thread": id, "text": "mine" })).await;
    let laptop = db::list_tokens(&t.app.db(), Some("bob")).unwrap()[0]
        .id
        .clone();
    db::revoke_token(&t.app.db(), &laptop, T0).unwrap();
    let desktop = db::add_token(&t.app.db(), "bob", "desktop", T0).unwrap();
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    assert_eq!(inbox_of(&t, &desktop.secret).await, vec![id.clone()]);
    assert_eq!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
}

#[tokio::test]
async fn a_session_that_ends_releases_its_threads_to_the_persons_other_sessions_at_once() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let short = t.agent("bob", "term-short", "pagos-api");
    let next = t.agent("bob", "term-next", "pagos-api");
    short.inbox().await;
    let (_, v) = short.send(json!({ "thread": id, "text": "answer" })).await;
    assert_eq!(v["claimed"], "this-session");
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    // While the short session holds the thread, the next one does not see it...
    assert!(thread_ids(&next.inbox().await).is_empty());
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let next = next.clone();
        async move { next.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    // ...until it ends: then at once, without waiting for the claim to lapse.
    let (status, v) = short.end().await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "ok": true }));
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_secs() < 10);
    assert_eq!(new_texts(&v, &id), Some(vec!["and?".to_string()]), "{v}");
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"],
        Value::Null
    );
    let (_, v) = next.send(json!({ "thread": id, "text": "answer" })).await;
    assert_eq!(v["claimed"], "this-session");
    let sessions = db::list_sessions(&t.app.db(), Some("bob"), T0).unwrap();
    assert_eq!(sessions.len(), 1, "the ended session is forgotten");
}

#[tokio::test]
async fn ending_a_session_twice_or_one_never_seen_is_fine() {
    let t = team(&["ana", "bob"]);
    let bob = t.agent("bob", "term-bob1", "");
    bob.inbox().await;
    for _ in 0..2 {
        let (status, v) = bob.end().await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v, json!({ "ok": true }));
    }
    let (status, _) = t.agent("bob", "term-never", "").end().await;
    assert_eq!(status, StatusCode::OK);
    // Ending records nothing.
    assert!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .is_empty()
    );
    // Without the header it ends the token's default session.
    t.inbox("bob").await;
    assert_eq!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .len(),
        1
    );
    let (status, _) = t.agent("bob", "", "").end().await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        db::list_sessions(&t.app.db(), Some("bob"), T0)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_session_ends_only_itself() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let bob = t.agent("bob", "same-id-1", "");
    bob.inbox().await;
    bob.ok(json!({ "thread": id, "text": "mine" })).await;
    let mut ids = vec![t.ok("bob", json!({ "to": ["ana"], "text": "q2" })).await];
    let ana = t.agent("ana", "same-id-1", "");
    ana.inbox().await;
    ana.ok(json!({ "thread": ids[0], "text": "mine too" }))
        .await;
    ids.push(id.clone());
    // Another person's session with the same id, another token of Bob's with the same id, the
    // default session, a malformed id and no token at all leave Bob's session alone.
    let second = db::add_token(&t.app.db(), "bob", "desktop", T0).unwrap();
    let other_token = Agent {
        app: t.app.clone(),
        token: second.secret,
        headers: vec![("relevo-session".into(), "same-id-1".into())],
    };
    assert_eq!(ana.end().await.0, StatusCode::OK);
    assert_eq!(other_token.end().await.0, StatusCode::OK);
    assert_eq!(t.agent("bob", "", "").end().await.0, StatusCode::OK);
    let (status, v) = t.agent("bob", "bad", "").end().await;
    assert_eq!((status, code(&v)), (StatusCode::BAD_REQUEST, "BAD_REQUEST"));
    let (status, v) = t.raw("DELETE", "/api/session", None, Body::empty()).await;
    assert_eq!(
        (status, code(&v)),
        (StatusCode::UNAUTHORIZED, "UNAUTHORIZED")
    );
    let sessions = db::list_sessions(&t.app.db(), Some("bob"), T0).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "laptop");
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["claimed_by"]["label"],
        "laptop"
    );
    // Ana's ended session let go of her claim.
    assert_eq!(
        t.thread("bob", &ids[0]).await["delivery"][1]["claimed_by"],
        Value::Null
    );
}

#[tokio::test]
async fn invited_back_a_person_finds_threads_their_old_sessions_had_claimed() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    db::deactivate(&t.app.db(), "bob", T0).unwrap();
    let back = db::invite(&t.app.db(), "bob", "BOB", None, "desktop", T0).unwrap();
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    assert_eq!(inbox_of(&t, &back.secret).await, vec![id]);
}

#[tokio::test]
async fn a_token_keeps_at_most_20_sessions_and_forgets_the_least_recently_seen() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let first = t.agent("bob", "term-first", "first");
    first.inbox().await;
    first.ok(json!({ "thread": id, "text": "mine" })).await;
    t.ok("ana", json!({ "thread": id, "text": "and?" })).await;
    for i in 0..18 {
        t.app.clock.advance(1);
        t.agent("bob", &format!("term-{i:04}"), &format!("ctx-{i}"))
            .inbox()
            .await;
    }
    // Twenty sessions: the first still holds its claim.
    t.app.clock.advance(1);
    let general = t.agent("bob", "term-general", "");
    assert!(thread_ids(&general.inbox().await).is_empty());
    let sessions = || db::list_sessions(&t.app.db(), Some("bob"), T0 + 100).unwrap();
    assert_eq!(sessions().len(), 20);
    // A twenty-first forgets the one seen least recently, with its claim.
    t.app.clock.advance(1);
    t.agent("bob", "term-late", "late").inbox().await;
    assert_eq!(sessions().len(), 20);
    assert!(
        sessions()
            .iter()
            .all(|s| s.context.as_deref() != Some("first"))
    );
    assert_eq!(
        new_texts(&general.inbox().await, &id),
        Some(vec!["and?".to_string()])
    );
    // So the team list holds at most 20 contexts per token.
    let (_, v) = t.get("ana", "/api/people").await;
    assert_eq!(v["team"][1]["contexts"].as_array().unwrap().len(), 19);
}

/// A thread as the model sees it, for the person `bob` and his sessions.
struct ModelThread {
    id: String,
    from: &'static str,
    team: bool,
    open: bool,
    /// Order of the latest note, and of the latest one not bob's.
    last: u64,
    last_other: u64,
    /// How far bob has handled it (written in it or marked it done).
    handled: u64,
    /// The latest note that reached any session of bob's, ended ones included.
    reached: u64,
    context: Option<&'static str>,
    claimed: Option<usize>,
    /// What reached each session.
    delivered: [u64; SESSIONS.len()],
}

impl ModelThread {
    fn pending(&self) -> bool {
        !self.team && self.open && self.handled < self.last_other
    }

    fn deliver(&mut self, s: usize, upto: u64) {
        self.delivered[s] = self.delivered[s].max(upto);
        self.reached = self.reached.max(upto);
    }
}

/// Bob's sessions: general, pagos-api, frontend, and pagos-api on a branch.
const SESSIONS: [(&str, &str); 4] = [
    ("model-general", ""),
    ("model-pagos", "pagos-api"),
    ("model-front", "frontend"),
    ("model-branch", "pagos-api@feat/x"),
];

/// Whether a thread addressed to `to` shows in a session in `context`: the repository alone
/// matches it on any branch, a branch only itself.
fn context_matches(to: &str, context: &str) -> bool {
    to == context || context.split_once('@').is_some_and(|(repo, _)| repo == to)
}

/// Checks one inbox call of session `s` against the model, then applies its deliveries.
async fn session_inbox_matches(t: &Team, s: usize, all: bool, model: &mut [ModelThread]) {
    let (id, context) = SESSIONS[s];
    // A closed team thread shows only with `all`.
    let sees = |th: &ModelThread| {
        all || (th.open || !th.team)
            && (th.claimed == Some(s)
                || (th.claimed.is_none()
                    && (context.is_empty()
                        || th.context.is_none_or(|to| context_matches(to, context)))))
    };
    // Shown: new (a note by someone else has not reached the session) or waiting; then any note
    // that has not reached it is in `new_notes`, bob's own included (a waiting thread may hold
    // one, written by a session that was not caught up).
    let mut expected: Vec<(bool, bool, u64, String, bool)> = model
        .iter()
        .filter(|th| sees(th))
        .map(|th| {
            let new = th.last_other > th.delivered[s];
            let waiting = th.pending();
            let unread = th.last > th.delivered[s];
            (new, waiting, th.last, th.id.clone(), unread)
        })
        .filter(|(new, waiting, _, _, _)| *new || *waiting)
        .collect();
    expected.sort_by_key(|e| std::cmp::Reverse((e.0, e.1, e.2)));
    let uri = if all {
        "/api/inbox?all=1"
    } else {
        "/api/inbox"
    };
    let inbox = t.agent("bob", id, context).inbox_at(uri).await;
    let want: Vec<String> = expected.iter().take(20).map(|e| e.3.clone()).collect();
    assert_eq!(thread_ids(&inbox), want, "session {id} all={all}");
    assert_eq!(inbox["more"], expected.len().saturating_sub(20), "{id}");
    for (shown, e) in inbox["threads"].as_array().unwrap().iter().zip(&expected) {
        assert_eq!(shown["waiting_for_you"], e.1, "{}", e.3);
        assert_eq!(shown["pending"], e.1, "{}", e.3);
        assert_eq!(
            shown["new_notes"].as_array().unwrap().is_empty(),
            !e.4,
            "{}",
            e.3
        );
    }
    for th in model.iter_mut().filter(|th| want.contains(&th.id)) {
        let last = th.last;
        th.deliver(s, last);
    }
}

#[tokio::test]
async fn session_inboxes_hold_order_and_more_over_hundreds_of_routed_threads() {
    let t = team(&["ana", "bob", "carl"]);
    for (id, context) in SESSIONS {
        t.agent("bob", id, context).inbox().await;
    }
    // A fixed pseudo-random sequence, so that a failure replays.
    let mut seed: u64 = 0x5eed;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut order = 0;
    let mut model: Vec<ModelThread> = Vec::new();
    let contexts = [
        None,
        Some("pagos-api"),
        Some("frontend"),
        Some("billing"),
        Some("pagos-api@feat/x"),
        Some("pagos-api@main"),
    ];
    for i in 0..150 {
        t.app.clock.advance(5);
        let from = if i % 2 == 0 { "ana" } else { "carl" };
        let context = contexts[next(contexts.len() as u64) as usize];
        let team = i % 25 == 7;
        let to = match (team, context) {
            (true, _) => json!(["@team"]),
            (false, None) => json!(["bob"]),
            (false, Some(c)) => json!([format!("bob/{c}")]),
        };
        let id = t
            .ok(from, json!({ "to": to, "text": format!("t{i}") }))
            .await;
        order += 1;
        model.push(ModelThread {
            id,
            from,
            team,
            open: true,
            last: order,
            last_other: order,
            handled: 0,
            reached: 0,
            context: if team { None } else { context },
            claimed: None,
            delivered: [0; SESSIONS.len()],
        });
    }
    // When each of bob's sessions was last seen, in seconds from the start: a claim lapses 15
    // minutes after its session was, which the next request (anyone's) notices.
    let mut now = 0;
    let mut seen = [0; SESSIONS.len()];
    for step in 0..400 {
        // Now and then a pause long enough for the claims of some sessions to lapse.
        let pause = if next(25) == 0 { LAPSE / 2 } else { 10 };
        t.app.clock.advance(pause);
        now += pause;
        for th in model.iter_mut() {
            if th.claimed.is_some_and(|c| now - seen[c] >= LAPSE) {
                th.claimed = None;
            }
        }
        let s = next(SESSIONS.len() as u64) as usize;
        match next(12) {
            // A session reads its inbox (sometimes all of it).
            0..=3 => {
                session_inbox_matches(&t, s, next(4) == 0, &mut model).await;
                seen[s] = now;
            }
            // A session of bob's answers in one of his threads it can see or not.
            4..=5 => {
                let i = next(model.len() as u64) as usize;
                let th = &mut model[i];
                if !th.open {
                    continue;
                }
                let (id, context) = SESSIONS[s];
                t.agent("bob", id, context)
                    .ok(json!({ "thread": th.id, "text": format!("bob {step}") }))
                    .await;
                order += 1;
                if th.delivered[s] >= th.last {
                    th.deliver(s, order);
                }
                th.last = order;
                // Writing handles what reached the session, its own note included when it was
                // caught up.
                th.handled = th.handled.max(th.delivered[s]);
                // A claim is never taken from another session.
                th.claimed.get_or_insert(s);
                seen[s] = now;
            }
            // Someone else writes again, or closes (only the asker writes in a team thread, to
            // close it).
            6..=8 => {
                let i = next(model.len() as u64) as usize;
                let th = &mut model[i];
                if !th.open {
                    continue;
                }
                let close = th.team || next(5) == 0;
                t.ok(
                    th.from,
                    json!({ "thread": th.id, "text": format!("again {step}"), "close": close }),
                )
                .await;
                order += 1;
                th.last = order;
                th.last_other = order;
                th.open = !close;
                // Closing releases every claim on the thread.
                if close {
                    th.claimed = None;
                }
            }
            // A session reads one thread whole.
            9 => {
                let i = next(model.len() as u64) as usize;
                let (id, context) = SESSIONS[s];
                t.agent("bob", id, context).thread(&model[i].id).await;
                let last = model[i].last;
                model[i].deliver(s, last);
                seen[s] = now;
            }
            // A session marks an open thread done: nothing written, nothing claimed.
            10 => {
                let i = next(model.len() as u64) as usize;
                let th = &mut model[i];
                if !th.open {
                    continue;
                }
                let (id, context) = SESSIONS[s];
                t.agent("bob", id, context)
                    .ok(json!({ "thread": th.id, "done": true }))
                    .await;
                // It handles what reached the session, not notes it never got.
                th.handled = th.handled.max(th.delivered[s]);
                seen[s] = now;
            }
            // A session ends, with its claims, and starts again: for the threads still pending
            // where a note bob has not handled reached a session, it starts with what bob handled;
            // elsewhere with what reached bob.
            _ => {
                let (id, context) = SESSIONS[s];
                t.agent("bob", id, context).end().await;
                for th in model.iter_mut() {
                    if th.claimed == Some(s) {
                        th.claimed = None;
                    }
                    th.delivered[s] = if th.pending() && th.handled < th.reached {
                        th.handled
                    } else {
                        th.reached
                    };
                }
                session_inbox_matches(&t, s, next(4) == 0, &mut model).await;
                seen[s] = now;
            }
        }
    }
    // Every session, routed and not, until nothing is new.
    for _ in 0..12 {
        for s in 0..SESSIONS.len() {
            session_inbox_matches(&t, s, false, &mut model).await;
            session_inbox_matches(&t, s, true, &mut model).await;
        }
    }
}

// ---------- waiting inbox ----------

/// Yields until `n` inbox requests are waiting.
async fn until_waiting(app: &App, n: usize) {
    let start = std::time::Instant::now();
    while app.waiting() != n {
        assert!(
            start.elapsed().as_secs() < 5,
            "{} waiting, not {n}",
            app.waiting()
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn wait_returns_as_soon_as_a_note_arrives() {
    let t = team(&["ana", "bob"]);
    let bob = t.agent("bob", "bridge-01", "");
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let bob = bob.clone();
        async move { bob.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    // The waiting request holds no database lock: everyone else goes on.
    assert!(thread_ids(&t.inbox("ana").await).is_empty());
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "now" })).await;
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_secs() < 10);
    assert_eq!(new_texts(&v, &id), Some(vec!["now".to_string()]));
    assert_eq!(t.app.waiting(), 0);
}

#[tokio::test]
async fn wait_answers_at_once_when_something_is_already_new() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let started = std::time::Instant::now();
    let v = t.agent("bob", "", "").inbox_at("/api/inbox?wait=60").await;
    assert!(started.elapsed().as_secs() < 10);
    assert_eq!(thread_ids(&v), vec![id]);
    for uri in ["/api/inbox?wait=0", "/api/inbox"] {
        let started = std::time::Instant::now();
        let v = t.agent("bob", "", "").inbox_at(uri).await;
        assert!(started.elapsed().as_millis() < 500, "{uri}");
        assert_eq!(v["threads"][0]["new_notes"], json!([]));
    }
}

#[tokio::test]
async fn wait_times_out_with_the_usual_answer_and_waiting_threads_do_not_count() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    t.inbox("bob").await;
    let started = std::time::Instant::now();
    let v = t.agent("bob", "", "").inbox_at("/api/inbox?wait=1").await;
    assert!(started.elapsed().as_millis() >= 1000);
    assert_eq!(thread_ids(&v), vec![id]);
    assert_eq!(v["threads"][0]["waiting_for_you"], true);
    assert_eq!(v["threads"][0]["new_notes"], json!([]));
}

#[tokio::test]
async fn a_note_this_session_does_not_see_wakes_it_but_it_keeps_waiting() {
    let t = team(&["ana", "bob"]);
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let front = front.clone();
        async move { front.inbox_at("/api/inbox?wait=1").await }
    });
    until_waiting(&t.app, 1).await;
    // For another context: front looks again, finds nothing of its own, and waits on.
    t.ok("ana", json!({ "to": ["bob/pagos-api"], "text": "x" }))
        .await;
    // Bob's own notes are never new for his sessions.
    t.agent("bob", "term-other", "")
        .ok(json!({ "to": ["ana"], "text": "mine" }))
        .await;
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_millis() >= 1000);
    assert!(thread_ids(&v).is_empty(), "{v}");
}

#[tokio::test]
async fn a_waiting_request_does_not_undo_a_newer_context_of_its_session() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-0001", "pagos-api");
    let front = t.agent("bob", "term-0001", "frontend");
    let waiter = tokio::spawn({
        let pagos = pagos.clone();
        async move { pagos.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    // The same session moves on to frontend while its old request waits.
    let (status, _) = front.get("/api/people").await;
    assert_eq!(status, StatusCode::OK);
    // A note it does not see wakes it to look again; one it sees answers it.
    t.ok("ana", json!({ "to": ["bob/billing"], "text": "x" }))
        .await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let v = waiter.await.unwrap();
    assert_eq!(thread_ids(&v), vec![id]);
    assert_eq!(v["session"]["context"], "frontend");
    let (_, people) = t.get("ana", "/api/people").await;
    assert_eq!(people["team"][1]["contexts"], json!(["frontend"]));
}

#[tokio::test]
async fn the_eleventh_wait_of_a_token_is_rate_limited() {
    let t = team(&["ana", "bob"]);
    let mut waiters = Vec::new();
    for i in 0..10 {
        let bob = t.agent("bob", &format!("bridge-{i:02}"), "");
        waiters.push(tokio::spawn(async move {
            bob.inbox_at("/api/inbox?wait=30").await
        }));
    }
    until_waiting(&t.app, 10).await;
    let (status, v) = t
        .agent("bob", "bridge-10", "")
        .get("/api/inbox?wait=30")
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&v), "RATE_LIMITED");
    // Without waiting, the same token is served; another token may wait.
    assert_eq!(
        t.agent("bob", "bridge-10", "").get("/api/inbox").await.0,
        StatusCode::OK
    );
    let ana = tokio::spawn({
        let ana = t.agent("ana", "", "");
        async move { ana.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 11).await;
    // One note wakes every waiting session of Bob's; Ana's own note does not wake her.
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "all of you" }))
        .await;
    for waiter in waiters {
        let v = waiter.await.unwrap();
        assert_eq!(new_texts(&v, &id), Some(vec!["all of you".to_string()]));
    }
    until_waiting(&t.app, 1).await;
    t.ok("bob", json!({ "thread": id, "text": "got it" })).await;
    assert_eq!(
        new_texts(&ana.await.unwrap(), &id),
        Some(vec!["got it".to_string()])
    );
    assert_eq!(t.app.waiting(), 0);
}

#[tokio::test]
async fn a_client_that_goes_away_frees_its_wait() {
    let t = team(&["ana", "bob"]);
    let waiters: Vec<_> = (0..10)
        .map(|i| {
            let bob = t.agent("bob", &format!("bridge-{i:02}"), "");
            tokio::spawn(async move { bob.get("/api/inbox?wait=60").await })
        })
        .collect();
    until_waiting(&t.app, 10).await;
    for waiter in &waiters {
        waiter.abort();
    }
    until_waiting(&t.app, 0).await;
    let waiter = tokio::spawn({
        let bob = t.agent("bob", "", "");
        async move { bob.get("/api/inbox?wait=60").await }
    });
    until_waiting(&t.app, 1).await;
    waiter.abort();
}

#[tokio::test]
async fn bad_wait_values_are_bad_requests() {
    let t = team(&["ana"]);
    for wait in ["61", "-1", "1.5", "", "abc"] {
        let (status, v) = t.get("ana", &format!("/api/inbox?wait={wait}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{wait}");
        assert_eq!(code(&v), "BAD_REQUEST");
    }
    let (status, _) = t.get("ana", "/api/inbox?wait=1&wait=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = t.get("ana", "/api/inbox?wait=0&all=1").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_claim_lapsing_wakes_a_waiting_inbox_when_it_lapses() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    t.app.clock.advance(LAPSE - 1);
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let front = front.clone();
        async move { front.inbox_at("/api/inbox?wait=30").await }
    });
    // Front looked once, a second before the lapse, and found nothing of its own.
    until_waiting(&t.app, 1).await;
    t.app.clock.advance(1);
    // With no other request, it looks again when the claim lapses, and gets the thread.
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_millis() >= 900);
    assert!(started.elapsed().as_secs() < 10);
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["q".to_string(), "mine".to_string()])
    );
    assert_eq!(v["threads"][0]["claimed"], Value::Null);
}

#[tokio::test]
async fn a_lapse_any_request_notices_wakes_waiting_inboxes() {
    let t = team(&["ana", "bob"]);
    let pagos = t.agent("bob", "term-pagos", "pagos-api");
    let front = t.agent("bob", "term-front", "frontend");
    front.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    pagos.inbox().await;
    pagos.ok(json!({ "thread": id, "text": "mine" })).await;
    t.app.clock.advance(10);
    let started = std::time::Instant::now();
    let waiter = tokio::spawn({
        let front = front.clone();
        async move { front.inbox_at("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    // The clock jumps past the lapse; the next request, anyone's, ends the claim and wakes Bob's
    // waiting requests.
    t.app.clock.advance(LAPSE);
    t.inbox("ana").await;
    let v = waiter.await.unwrap();
    assert!(started.elapsed().as_secs() < 10);
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["q".to_string(), "mine".to_string()])
    );
}

#[tokio::test]
async fn a_wait_does_not_outlive_its_token() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let bridge = t.agent("bob", "bridge-01", "");
    bridge.inbox().await;
    let waiter = tokio::spawn({
        let bob = bridge.clone();
        async move { bob.get("/api/inbox?wait=30").await }
    });
    until_waiting(&t.app, 1).await;
    db::deactivate(&t.app.db(), "bob", T0).unwrap();
    // Notes still land in Bob's old threads, but his waiting request does not get them.
    t.ok("ana", json!({ "thread": id, "text": "after" })).await;
    let (status, v) = waiter.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(code(&v), "UNAUTHORIZED");
}

// ---------- handled: a note stays pending until the person handles it ----------

/// The thread `id` in an inbox, if it is there.
fn listed<'a>(inbox: &'a Value, id: &str) -> Option<&'a Value> {
    inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
}

/// `who`'s entry of a thread's delivery.
fn delivery_of<'a>(th: &'a Value, who: &str) -> &'a Value {
    th["delivery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["handle"] == who)
        .unwrap_or_else(|| panic!("{who} not in {th}"))
}

#[tokio::test]
async fn a_note_fetched_by_a_session_that_ends_is_new_for_the_persons_next_session() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "Rotate the key?" }))
        .await;
    // Bob's pi fetches the note and dies before doing anything with it.
    let pi = t.agent("bob", "pi-000001", "infra");
    assert_eq!(
        new_texts(&pi.inbox().await, &id),
        Some(vec!["Rotate the key?".to_string()])
    );
    pi.end().await;
    // The next one gets it as new at once: a waiting inbox answers without waiting.
    let next = t.agent("bob", "pi-000002", "infra");
    let (status, v) = next.get("/api/inbox?wait=30").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["Rotate the key?".to_string()]),
        "{v}"
    );
    let th = listed(&v, &id).unwrap();
    assert_eq!(th["pending"], true);
    assert_eq!(th["waiting_for_you"], true);
    // Once: it reached this session, so it is not new for it again, though still pending.
    let v = next.inbox().await;
    assert_eq!(new_texts(&v, &id), Some(vec![]));
    assert_eq!(listed(&v, &id).unwrap()["pending"], true);
}

#[tokio::test]
async fn a_note_fetched_by_a_session_that_lapses_is_new_for_the_persons_next_session() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "Rotate the key?" }))
        .await;
    // Bob's pi fetches the note, claims the thread, and stops answering.
    let pi = t.agent("bob", "pi-000001", "infra");
    pi.inbox().await;
    let (_, v) = pi.send(json!({ "thread": id, "claim": true })).await;
    assert_eq!(v["claimed"], "this-session", "{v}");
    // A session started while the claim holds does not see the thread.
    t.app.clock.advance(60);
    let early = t.agent("bob", "pi-000002", "infra");
    assert!(listed(&early.inbox().await, &id).is_none());
    // Once the claim has lapsed, it shows there, as new: it never reached that session. And a
    // session started now gets it as new too.
    t.app.clock.advance(LAPSE);
    let v = early.inbox().await;
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["Rotate the key?".to_string()]),
        "{v}"
    );
    assert_eq!(listed(&v, &id).unwrap()["claimed"], Value::Null);
    let late = t.agent("bob", "pi-000003", "");
    assert_eq!(
        new_texts(&late.inbox().await, &id),
        Some(vec!["Rotate the key?".to_string()])
    );
}

#[tokio::test]
async fn a_claim_does_not_handle_and_when_it_ends_the_thread_shows_again() {
    let t = team(&["ana", "bob"]);
    let one = t.agent("bob", "term-0001", "");
    let two = t.agent("bob", "term-0002", "");
    one.inbox().await;
    two.inbox().await;
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let v = one.inbox().await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true);
    one.ok(json!({ "thread": id, "claim": true })).await;
    // Claimed: still pending for Bob, in the claiming session, and hidden from the other.
    let v = one.inbox().await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true, "{v}");
    assert!(listed(&two.inbox().await, &id).is_none());
    assert_eq!(
        t.thread("ana", &id).await["delivery"][1]["handled_at"],
        Value::Null
    );
    // The claiming session ends: the thread shows again in the other one, new and pending.
    one.end().await;
    let v = two.inbox().await;
    let th = listed(&v, &id).unwrap();
    assert_eq!(th["pending"], true);
    assert_eq!(th["claimed"], Value::Null);
    assert_eq!(new_texts(&v, &id), Some(vec!["q".to_string()]));
}

#[tokio::test]
async fn done_handles_the_thread_without_a_note() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok("ana", json!({ "to": ["bob"], "text": "FYI: deployed." }))
        .await;
    let pi = t.agent("bob", "pi-000001", "infra");
    pi.inbox().await;
    t.app.clock.advance(90);
    let (status, v) = pi.send(json!({ "thread": id, "done": true })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({ "thread": id, "note": null, "status": "open", "claimed": null })
    );
    // No note, no claim: the thread just stops being pending for Bob, in every session.
    let th = t.thread("ana", &id).await;
    assert_eq!(th["notes"].as_array().unwrap().len(), 1);
    assert_eq!(th["status"], "open");
    assert!(listed(&pi.inbox().await, &id).is_none());
    assert!(thread_ids(&t.agent("bob", "pi-000002", "").inbox().await).is_empty());
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["claimed_by"], Value::Null);
    assert_eq!(bob["handled_at"], "2026-11-08T10:42:30Z");
    assert_eq!(
        bob["handled_by"],
        json!({ "label": "laptop", "context": "infra" })
    );
    let th = pi.thread(&id).await;
    assert_eq!(th["pending"], false);
    assert_eq!(th["waiting_for_you"], false);
    // A new note by Ana makes it pending again.
    t.ok("ana", json!({ "thread": id, "text": "And restarted." }))
        .await;
    let v = pi.inbox().await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true);
    assert_eq!(
        delivery_of(&t.thread("ana", &id).await, "bob")["handled_at"],
        Value::Null
    );
}

#[tokio::test]
async fn done_without_a_fetch_handles_nothing_is_never_limited_idempotent_and_refused_once_closed()
{
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    for i in 0..19 {
        t.ok("ana", json!({ "thread": id, "text": format!("n{i}") }))
            .await;
    }
    let (_, v) = t
        .send("bob", json!({ "thread": id, "text": "busy?" }))
        .await;
    assert_eq!(code(&v), "THREAD_BUSY", "{v}");
    // Never limited, and a key replays it.
    let body = json!({ "thread": id, "done": true, "key": "done-1" });
    let (status, first) = t.send("bob", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (_, again) = t.send("bob", body).await;
    assert_eq!(first, again);
    // Nothing had reached the session, so it handled nothing: the notes are still pending.
    let v = t.inbox("bob").await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true);
    assert_eq!(new_texts(&v, &id).unwrap().len(), 20);
    assert_eq!(
        delivery_of(&t.thread("ana", &id).await, "bob")["handled_at"],
        Value::Null
    );
    // Once they reached it, `done` handles them all: it is gone.
    t.ok("bob", json!({ "thread": id, "done": true })).await;
    assert!(listed(&t.inbox("bob").await, &id).is_none());
    // Done on a closed thread is a write in a closed thread.
    t.ok("ana", json!({ "thread": id, "close": true })).await;
    let (status, v) = t.send("bob", json!({ "thread": id, "done": true })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&v), "THREAD_CLOSED");
}

#[tokio::test]
async fn replying_or_closing_handles_and_closed_or_team_threads_are_never_pending() {
    let t = team(&["ana", "bob", "carl"]);
    // A reply handles.
    let replied = t.ok("ana", json!({ "to": ["bob"], "text": "a" })).await;
    let pi = t.agent("bob", "pi-000001", "");
    pi.inbox().await;
    pi.ok(json!({ "thread": replied, "text": "done" })).await;
    let bob = delivery_of(&t.thread("ana", &replied).await, "bob").clone();
    assert_eq!(
        bob["handled_by"],
        json!({ "label": "laptop", "context": null })
    );
    // A close by Bob handles, without text too.
    let closed = t.ok("ana", json!({ "to": ["bob"], "text": "b" })).await;
    pi.inbox().await;
    pi.ok(json!({ "thread": closed, "close": true })).await;
    assert!(delivery_of(&t.thread("ana", &closed).await, "bob")["handled_at"].is_string());
    // A close by Ana does not handle for Bob, but a closed thread is never pending.
    let by_ana = t.ok("ana", json!({ "to": ["bob"], "text": "c" })).await;
    pi.inbox().await;
    t.ok("ana", json!({ "thread": by_ana, "close": true }))
        .await;
    let th = pi.thread(&by_ana).await;
    assert_eq!(th["pending"], false);
    assert_eq!(delivery_of(&th, "bob")["handled_at"], Value::Null);
    // A team question is never pending, and a new session starts with what reached the person.
    let team_q = t
        .ok("ana", json!({ "to": ["@team"], "text": "Who?" }))
        .await;
    let v = pi.inbox().await;
    assert_eq!(listed(&v, &team_q).unwrap()["pending"], false);
    // None of these threads is new or pending for a new session of Bob's.
    assert!(thread_ids(&t.agent("bob", "pi-000002", "").inbox().await).is_empty());
    assert!(thread_ids(&pi.inbox().await).is_empty());
}

#[tokio::test]
async fn the_senders_delivery_tells_seen_from_handled() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "q" })).await;
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(
        bob,
        json!({ "handle": "bob", "note": null, "delivered_at": null, "by": null,
                "claimed_by": null, "seen_by": 0, "handled_at": null, "handled_by": null })
    );
    // Seen by pi, not handled.
    let pi = t.agent("bob", "pi-000001", "embeddings");
    pi.inbox().await;
    let note = t.thread("ana", &id).await["notes"][0]["id"].clone();
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(bob["note"], note);
    assert_eq!(
        bob["by"],
        json!({ "label": "laptop", "context": "embeddings" })
    );
    assert_eq!(bob["seen_by"], 1);
    assert_eq!(bob["handled_at"], Value::Null);
    // Pi ends: no live session has it, but `by` still says who saw it first.
    pi.end().await;
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(bob["seen_by"], 0);
    assert_eq!(
        bob["by"],
        json!({ "label": "laptop", "context": "embeddings" })
    );
    assert_eq!(bob["delivered_at"], "2026-11-08T10:41:00Z");
    // Another session of Bob's answers: handled, when and from where.
    t.app.clock.advance(120);
    let cc = t.agent("bob", "cc-0000001", "producto");
    cc.inbox().await;
    cc.ok(json!({ "thread": id, "text": "Yes." })).await;
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(bob["note"], note);
    assert_eq!(
        bob["by"],
        json!({ "label": "laptop", "context": "embeddings" })
    );
    assert_eq!(bob["seen_by"], 1);
    assert_eq!(bob["handled_at"], "2026-11-08T10:43:00Z");
    assert_eq!(
        bob["handled_by"],
        json!({ "label": "laptop", "context": "producto" })
    );
    // Handling again later, with nothing new by someone else, keeps when it was handled.
    t.app.clock.advance(60);
    cc.ok(json!({ "thread": id, "text": "And more." })).await;
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(bob["handled_at"], "2026-11-08T10:43:00Z");
}

#[tokio::test]
async fn done_or_a_reply_handles_only_what_reached_the_session() {
    let t = team(&["ana", "bob"]);
    let id = t
        .ok(
            "ana",
            json!({ "to": ["bob"], "text": "Thanks for the deploy!" }),
        )
        .await;
    let pi = t.agent("bob", "pi-000001", "infra");
    pi.inbox().await;
    // A note lands between pi's fetch and its `done`.
    t.app.clock.advance(30);
    t.ok(
        "ana",
        json!({ "thread": id, "text": "Also: can you rotate the key today?" }),
    )
    .await;
    t.app.clock.advance(30);
    pi.ok(json!({ "thread": id, "done": true })).await;
    // The thank-you is handled; the follow-up never reached Bob, so it is still pending.
    let bob = delivery_of(&t.thread("ana", &id).await, "bob").clone();
    assert_eq!(bob["handled_at"], "2026-11-08T10:42:00Z");
    pi.end().await;
    let next = t.agent("bob", "pi-000002", "infra");
    let v = next.inbox().await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true, "{v}");
    assert_eq!(
        new_texts(&v, &id),
        Some(vec!["Also: can you rotate the key today?".to_string()])
    );
    // It reached Bob, but nobody handled it: Ana is not told it was.
    let th = t.thread("ana", &id).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["note"], th["notes"][1]["id"]);
    assert_eq!(bob["handled_at"], Value::Null);
    let v = next.inbox().await;
    assert_eq!(listed(&v, &id).unwrap()["pending"], true, "{v}");

    // A reply written across a new note handles up to what reached the writer, not past it.
    t.app.clock.advance(30);
    t.ok(
        "ana",
        json!({ "thread": id, "text": "And the staging one." }),
    )
    .await;
    t.app.clock.advance(30);
    next.ok(json!({ "thread": id, "text": "Rotated the key." }))
        .await;
    let th = t.thread("ana", &id).await;
    let bob = delivery_of(&th, "bob");
    assert_eq!(bob["note"], th["notes"][1]["id"]);
    assert_eq!(bob["handled_at"], "2026-11-08T10:43:00Z");
    let v = next.inbox().await;
    let listed_th = listed(&v, &id).unwrap();
    assert_eq!(listed_th["pending"], true, "{v}");
    assert_eq!(
        new_texts(&v, &id),
        Some(vec![
            "And the staging one.".to_string(),
            "Rotated the key.".to_string()
        ])
    );
    // A reply once caught up handles it all.
    next.ok(json!({ "thread": id, "text": "Staging too." }))
        .await;
    assert!(listed(&next.inbox().await, &id).is_none());
    assert!(listed(&t.agent("bob", "pi-000003", "").inbox().await, &id).is_none());
}
