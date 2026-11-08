//! The HTTP API: `send`, `inbox`, `thread`, `people` and `health`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::auth::Caller;
use crate::db::{RESERVED_HANDLES, random_id, rfc3339};
use crate::error::ApiError;

const MAX_BODY: usize = 80 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_RECIPIENTS: usize = 20;
const MAX_KEY_CHARS: usize = 100;
const TITLE_CHARS: usize = 80;
const INBOX_THREADS: usize = 20;
const INBOX_TEXT_CHARS: usize = 4000;
const THREAD_NOTES_PER_HOUR: i64 = 20;
const PERSON_NOTES_PER_MINUTE: i64 = 60;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/api/inbox", get(inbox))
        .route("/api/threads/{id}", get(thread))
        .route(
            "/api/send",
            post(send).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/api/people", get(people))
        .fallback(|| async { ApiError::not_found("no such route") })
        .with_state(app)
}

// ---------- shapes ----------

#[derive(Serialize)]
struct Member {
    handle: String,
    name: String,
    about: String,
}

#[derive(Serialize)]
struct NoteOut {
    id: String,
    author: String,
    via: String,
    created_at: String,
    text: String,
    truncated: bool,
}

#[derive(Serialize)]
struct Delivery {
    handle: String,
    note: Option<String>,
    delivered_at: Option<String>,
}

#[derive(Serialize)]
struct ThreadOut {
    id: String,
    kind: String,
    title: String,
    from: String,
    participants: Vec<String>,
    status: String,
    waiting_for_you: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_notes: Option<Vec<NoteOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<Vec<NoteOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<Vec<Delivery>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    to: Option<Vec<String>>,
    thread: Option<String>,
    text: Option<String>,
    #[serde(default)]
    close: bool,
    key: Option<String>,
}

#[derive(Serialize)]
struct SendResult {
    thread: String,
    /// `None` when the send only closed the thread.
    note: Option<String>,
    status: String,
}

/// A thread as seen by one participant.
struct ThreadRow {
    id: String,
    kind: String,
    title: String,
    from: String,
    opener_id: i64,
    status: String,
    delivered_seq: i64,
    last_seq: i64,
    waiting: bool,
}

// ---------- queries shared by the handlers ----------

/// Threads of person `?1`, with what `inbox` and `thread` need to know about each.
const THREADS_OF: &str = "
SELECT id, kind, title, opener, opener_id, status, delivered_seq, last_seq,
       (kind = 'direct' AND status = 'open' AND last_author != ?1) AS waiting
FROM (
    SELECT t.id, t.kind, t.title, o.handle AS opener, t.opener_id, t.status,
           coalesce(p.delivered_seq, 0) AS delivered_seq,
           (SELECT max(seq) FROM notes WHERE thread_id = t.id) AS last_seq,
           (SELECT author_id FROM notes WHERE thread_id = t.id ORDER BY seq DESC LIMIT 1)
               AS last_author
    FROM participants p
    JOIN threads t ON t.id = p.thread_id
    JOIN people o ON o.id = t.opener_id
    WHERE p.person_id = ?1
)";

fn thread_row(r: &rusqlite::Row) -> rusqlite::Result<ThreadRow> {
    Ok(ThreadRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        from: r.get(3)?,
        opener_id: r.get(4)?,
        status: r.get(5)?,
        delivered_seq: r.get(6)?,
        last_seq: r.get(7)?,
        waiting: r.get(8)?,
    })
}

fn find_thread(conn: &Connection, me: i64, id: &str) -> Result<ThreadRow, ApiError> {
    let sql = format!("{THREADS_OF} WHERE id = ?2");
    conn.query_row(&sql, params![me, id], thread_row)
        .optional()?
        .ok_or_else(|| ApiError::not_found("no such thread"))
}

fn team(conn: &Connection) -> Result<Vec<Member>, ApiError> {
    let mut stmt =
        conn.prepare("SELECT handle, name, about FROM people WHERE active = 1 ORDER BY handle")?;
    let rows = stmt.query_map([], |r| {
        Ok(Member {
            handle: r.get(0)?,
            name: r.get(1)?,
            about: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn participants(conn: &Connection, thread: &str) -> Result<Vec<String>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT pe.handle FROM participants p JOIN people pe ON pe.id = p.person_id
         WHERE p.thread_id = ?1 ORDER BY p.rowid",
    )?;
    let rows = stmt.query_map([thread], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Notes of a thread after `seq`, oldest first, cut to `max_chars` when given.
fn notes_after(
    conn: &Connection,
    thread: &str,
    seq: i64,
    max_chars: Option<usize>,
) -> Result<Vec<NoteOut>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT n.id, a.handle, n.via, n.created_at, n.text
         FROM notes n JOIN people a ON a.id = n.author_id
         WHERE n.thread_id = ?1 AND n.seq > ?2 ORDER BY n.seq",
    )?;
    let rows = stmt.query_map(params![thread, seq], |r| {
        let mut text: String = r.get(4)?;
        let mut truncated = false;
        if let Some((cut, _)) = max_chars.and_then(|max| text.char_indices().nth(max)) {
            text.truncate(cut);
            truncated = true;
        }
        Ok(NoteOut {
            id: r.get(0)?,
            author: r.get(1)?,
            via: r.get(2)?,
            created_at: rfc3339(r.get(3)?),
            text,
            truncated,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn deliver(
    conn: &Connection,
    thread: &str,
    person: i64,
    seq: i64,
    now: i64,
) -> Result<(), ApiError> {
    conn.execute(
        "UPDATE participants SET delivered_seq = ?3, delivered_at = ?4
         WHERE thread_id = ?1 AND person_id = ?2 AND coalesce(delivered_seq, 0) < ?3",
        params![thread, person, seq, now],
    )?;
    Ok(())
}

fn thread_out(conn: &Connection, row: &ThreadRow) -> Result<ThreadOut, ApiError> {
    Ok(ThreadOut {
        id: row.id.clone(),
        kind: row.kind.clone(),
        title: row.title.clone(),
        from: row.from.clone(),
        participants: participants(conn, &row.id)?,
        status: row.status.clone(),
        waiting_for_you: row.waiting,
        new_notes: None,
        notes: None,
        delivery: None,
    })
}

// ---------- handlers ----------

async fn people(State(app): State<Arc<App>>, _caller: Caller) -> Result<Json<Value>, ApiError> {
    let team = team(&app.db())?;
    Ok(Json(json!({ "team": team })))
}

async fn inbox(State(app): State<Arc<App>>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let now = app.clock.now();
    let mut conn = app.db();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sql = format!(
        "{THREADS_OF} WHERE waiting OR last_seq > delivered_seq
         ORDER BY waiting DESC, last_seq DESC"
    );
    let rows: Vec<ThreadRow> = tx
        .prepare(&sql)?
        .query_map([caller.person_id], thread_row)?
        .collect::<rusqlite::Result<_>>()?;
    let more = rows.len().saturating_sub(INBOX_THREADS);
    let mut threads = Vec::new();
    for row in rows.iter().take(INBOX_THREADS) {
        let mut out = thread_out(&tx, row)?;
        out.new_notes = Some(notes_after(
            &tx,
            &row.id,
            row.delivered_seq,
            Some(INBOX_TEXT_CHARS),
        )?);
        deliver(&tx, &row.id, caller.person_id, row.last_seq, now)?;
        threads.push(out);
    }
    let team = team(&tx)?;
    tx.commit()?;
    tracing::info!(person = %caller.handle, threads = threads.len(), more, "inbox");
    Ok(Json(
        json!({ "team": team, "threads": threads, "more": more }),
    ))
}

async fn thread(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<ThreadOut>, ApiError> {
    let now = app.clock.now();
    let mut conn = app.db();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = find_thread(&tx, caller.person_id, &id)?;
    deliver(&tx, &row.id, caller.person_id, row.last_seq, now)?;
    let mut out = thread_out(&tx, &row)?;
    out.notes = Some(notes_after(&tx, &row.id, 0, None)?);
    let mut stmt = tx.prepare(
        "SELECT pe.handle, n.id, p.delivered_at
         FROM participants p JOIN people pe ON pe.id = p.person_id
         LEFT JOIN notes n ON n.seq = p.delivered_seq
         WHERE p.thread_id = ?1 ORDER BY p.rowid",
    )?;
    let delivery = stmt
        .query_map([&row.id], |r| {
            Ok(Delivery {
                handle: r.get(0)?,
                note: r.get(1)?,
                delivered_at: r.get::<_, Option<i64>>(2)?.map(rfc3339),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    out.delivery = Some(delivery);
    drop(stmt);
    tx.commit()?;
    tracing::info!(person = %caller.handle, thread = %row.id, "thread");
    Ok(Json(out))
}

async fn send(
    State(app): State<Arc<App>>,
    caller: Caller,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<SendResult>, ApiError> {
    let body = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::too_large(StatusCode::PAYLOAD_TOO_LARGE, "request body over 80 KiB")
        } else {
            ApiError::bad_request(e.body_text())
        }
    })?;
    let req: SendRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid send body: {e}")))?;
    let result = write(&app, &caller, req)?;
    tracing::info!(
        person = %caller.handle,
        thread = %result.thread,
        note = result.note.as_deref().unwrap_or("-"),
        status = %result.status,
        "send"
    );
    Ok(Json(result))
}

/// The whole of `send`, in one transaction so that idempotency and the limits cannot race.
fn write(app: &App, caller: &Caller, req: SendRequest) -> Result<SendResult, ApiError> {
    if let Some(key) = &req.key
        && (key.is_empty() || key.chars().count() > MAX_KEY_CHARS)
    {
        return Err(ApiError::bad_request("key must be 1 to 100 characters"));
    }
    if let Some(text) = &req.text {
        if text.trim().is_empty() {
            return Err(ApiError::bad_request("text is empty"));
        }
        if text.len() > MAX_TEXT {
            return Err(ApiError::too_large(
                StatusCode::BAD_REQUEST,
                "text over 64 KiB; split it or link to the content",
            ));
        }
    }
    if req.to.is_some() == req.thread.is_some() {
        return Err(ApiError::bad_request(
            "give exactly one of 'to' and 'thread'",
        ));
    }
    if req.text.is_none() && !(req.thread.is_some() && req.close) {
        return Err(ApiError::bad_request(
            "text is required (it may be left out only to close a thread)",
        ));
    }

    let now = app.clock.now();
    let mut conn = app.db();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    if let Some(key) = &req.key {
        let first = tx
            .query_row(
                "SELECT thread_id, note_id, status FROM send_keys WHERE person_id = ?1 AND key = ?2",
                params![caller.person_id, key],
                |r| {
                    Ok(SendResult {
                        thread: r.get(0)?,
                        note: r.get(1)?,
                        status: r.get(2)?,
                    })
                },
            )
            .optional()?;
        if let Some(first) = first {
            return Ok(first);
        }
    }

    let thread_id = match (&req.to, &req.thread) {
        (Some(to), _) => {
            let text = req.text.as_deref().unwrap_or_default();
            open_thread(&tx, app, caller, to, text, now)?
        }
        (None, Some(id)) => {
            let row = find_thread(&tx, caller.person_id, id)?;
            if row.status == "closed" {
                return Err(ApiError::thread_closed());
            }
            if req.close && row.kind == "team" && row.opener_id != caller.person_id {
                return Err(ApiError::forbidden());
            }
            if req.text.is_some() {
                let recent: i64 = tx.query_row(
                    "SELECT count(*) FROM notes WHERE thread_id = ?1 AND created_at > ?2",
                    params![row.id, now - 3600],
                    |r| r.get(0),
                )?;
                if recent >= THREAD_NOTES_PER_HOUR {
                    return Err(ApiError::thread_busy());
                }
            }
            row.id
        }
        (None, None) => unreachable!("checked above"),
    };

    let note_id = match &req.text {
        Some(text) => {
            let recent: i64 = tx.query_row(
                "SELECT count(*) FROM notes WHERE author_id = ?1 AND created_at > ?2",
                params![caller.person_id, now - 60],
                |r| r.get(0),
            )?;
            if recent >= PERSON_NOTES_PER_MINUTE {
                return Err(ApiError::rate_limited());
            }
            let id = random_id("n_");
            tx.execute(
                "INSERT INTO notes (id, thread_id, author_id, token_id, via, text, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    thread_id,
                    caller.person_id,
                    caller.token_id,
                    caller.label,
                    text,
                    now
                ],
            )?;
            // The writer has their own note delivered at once, unless notes before it never
            // reached them: then the pointer stays, so those notes are not skipped.
            tx.execute(
                "UPDATE participants SET delivered_seq = ?3, delivered_at = ?4
                 WHERE thread_id = ?1 AND person_id = ?2
                   AND coalesce(delivered_seq, 0) >= (SELECT coalesce(max(seq), 0) FROM notes
                                                      WHERE thread_id = ?1 AND seq < ?3)",
                params![thread_id, caller.person_id, tx.last_insert_rowid(), now],
            )?;
            Some(id)
        }
        None => None,
    };

    if req.close {
        tx.execute(
            "UPDATE threads SET status = 'closed' WHERE id = ?1",
            [&thread_id],
        )?;
    }
    let result = SendResult {
        thread: thread_id,
        note: note_id,
        status: if req.close { "closed" } else { "open" }.to_string(),
    };
    if let Some(key) = &req.key {
        tx.execute(
            "INSERT INTO send_keys (person_id, key, thread_id, note_id, status)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                caller.person_id,
                key,
                result.thread,
                result.note,
                result.status
            ],
        )?;
    }
    tx.commit()?;
    Ok(result)
}

/// Creates a `direct` or `team` thread with its participants (the writer first). Returns its id.
fn open_thread(
    conn: &Connection,
    app: &App,
    caller: &Caller,
    to: &[String],
    text: &str,
    now: i64,
) -> Result<String, ApiError> {
    let handles: Vec<String> = to
        .iter()
        .map(|h| h.trim().trim_start_matches('@').to_lowercase())
        .collect();
    let is_team = handles
        .iter()
        .any(|h| RESERVED_HANDLES.contains(&h.as_str()));

    let (kind, others): (&str, Vec<i64>) = if is_team {
        if handles.len() != 1 {
            return Err(ApiError::bad_request(
                "'@team' must be the only entry of 'to'",
            ));
        }
        let others: Vec<i64> = conn
            .prepare("SELECT id FROM people WHERE active = 1 AND id != ?1 ORDER BY handle")?
            .query_map([caller.person_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if others.is_empty() {
            return Err(ApiError::no_recipients());
        }
        let asked: i64 = conn.query_row(
            "SELECT count(*) FROM threads
             WHERE opener_id = ?1 AND kind = 'team' AND created_at > ?2",
            params![caller.person_id, now - 24 * 3600],
            |r| r.get(0),
        )?;
        if asked >= i64::from(app.team_questions_per_day) {
            return Err(ApiError::team_limit(app.team_questions_per_day));
        }
        ("team", others)
    } else {
        let mut seen = HashSet::new();
        let handles: Vec<&String> = handles.iter().filter(|h| seen.insert(*h)).collect();
        if handles.is_empty() || handles.len() > MAX_RECIPIENTS {
            return Err(ApiError::bad_request(
                "'to' needs 1 to 20 handles, or [\"@team\"]",
            ));
        }
        let mut others = Vec::new();
        for handle in handles {
            if *handle == caller.handle {
                return Err(ApiError::bad_request("you cannot send a note to yourself"));
            }
            let id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM people WHERE handle = ?1 AND active = 1",
                    [handle],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => others.push(id),
                None => {
                    let valid = serde_json::to_value(team(conn)?).unwrap_or_default();
                    return Err(ApiError::unknown_person(handle, valid));
                }
            }
        }
        ("direct", others)
    };

    let id = random_id("r_");
    conn.execute(
        "INSERT INTO threads (id, kind, title, opener_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'open', ?5)",
        params![id, kind, title_of(text), caller.person_id, now],
    )?;
    let mut add =
        conn.prepare("INSERT INTO participants (thread_id, person_id) VALUES (?1, ?2)")?;
    add.execute(params![id, caller.person_id])?;
    for person in others {
        add.execute(params![id, person])?;
    }
    Ok(id)
}

/// The first non-empty line of the note, without leading `#`s, cut to 80 characters.
fn title_of(text: &str) -> String {
    text.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .take(TITLE_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::title_of;

    #[test]
    fn titles() {
        assert_eq!(title_of("\n# Refunds endpoint\nmore"), "Refunds endpoint");
        assert_eq!(title_of(&"é".repeat(100)).chars().count(), 80);
    }
}
