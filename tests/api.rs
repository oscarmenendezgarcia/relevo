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
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req = req.header("content-type", "application/json");
        let res = api::router(self.app.clone())
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
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
    db::deactivate(&t.app.db(), "bob").unwrap();
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
async fn invited_back_the_old_token_works_again() {
    let t = team(&["ana", "bob"]);
    db::deactivate(&t.app.db(), "bob").unwrap();
    assert_eq!(t.get("bob", "/api/inbox").await.0, StatusCode::UNAUTHORIZED);
    db::invite(&t.app.db(), "bob", "Bob", None, "desktop", T0).unwrap();
    assert_eq!(t.get("bob", "/api/inbox").await.0, StatusCode::OK);
    t.ok("ana", json!({ "to": ["bob"], "text": "welcome back" }))
        .await;
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
            { "handle": "ana", "name": "ANA", "about": "ana things" },
            { "handle": "bob", "name": "BOB", "about": "bob things" },
        ]})
    );
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
    let delivery = th["delivery"].as_array().unwrap();
    assert_eq!(delivery[0]["handle"], "ana");
    assert_eq!(delivery[0]["note"], th["notes"][0]["id"]);
    assert_eq!(delivery[0]["delivered_at"], "2026-11-08T10:41:00Z");
    assert_eq!(
        delivery[1],
        json!({ "handle": "bob", "note": null, "delivered_at": null })
    );
}

#[tokio::test]
async fn writing_does_not_skip_notes_that_never_reached_the_writer() {
    let t = team(&["ana", "bob"]);
    let id = t.ok("ana", json!({ "to": ["bob"], "text": "one" })).await;
    t.ok("bob", json!({ "thread": id, "text": "two" })).await;
    // Ana answers without having fetched "two": it must still reach her.
    t.ok("ana", json!({ "thread": id, "text": "three" })).await;
    let inbox = t.inbox("ana").await;
    let texts: Vec<&str> = inbox["threads"][0]["new_notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["two", "three"]);
    assert_eq!(inbox["threads"][0]["waiting_for_you"], false);
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
    assert_eq!(v, json!({ "thread": id, "note": null, "status": "closed" }));
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
            { "handle": "ana", "name": "ANA", "about": "ana things" },
            { "handle": "bob", "name": "BOB", "about": "bob things" },
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
    // A close without text is never limited.
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
    // Then: bob replies to some (they leave his inbox), ana writes again in some read and some
    // unread ones, carl closes one with a last note and asks the team.
    for i in [299, 250, 10] {
        t.app.clock.advance(1);
        t.ok("bob", json!({ "thread": model[i].id, "text": "done" }))
            .await;
        last += 1;
        model[i].last = last;
        model[i].waiting = false;
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
    assert_eq!(t.inbox("bob").await["more"], 296 - 20);
}
