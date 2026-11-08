//! The HTTP API: `send`, `inbox`, `thread`, `people` and `health`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Caller;
use crate::db::{
    CONTEXT_FORMAT, RESERVED_HANDLES, context_repo, random_id, rfc3339, valid_context,
};
use crate::error::ApiError;
use crate::session::{Session, SessionOut, find, lapse, next_lapse, touch};
use crate::{App, MAX_WAITS_PER_TOKEN};

const MAX_TEXT: usize = 64 * 1024;
/// Room for 64 KiB of text even when every byte is JSON-escaped as `\u00XX` (6 bytes), plus the
/// other fields. The text limit is checked on the decoded text.
const MAX_BODY: usize = 400 * 1024;
const MAX_RECIPIENTS: usize = 20;
const MAX_KEY_CHARS: usize = 100;
const TITLE_CHARS: usize = 80;
const INBOX_THREADS: usize = 20;
const INBOX_TEXT_CHARS: usize = 4000;
const THREAD_NOTES_PER_HOUR: i64 = 20;
const PERSON_NOTES_PER_MINUTE: i64 = 60;
const KEY_LIFETIME_SECS: i64 = 24 * 3600;
const MAX_WAIT_SECS: u64 = 60;
/// A person's `contexts` in the team list are those of their sessions seen this recently.
const CONTEXT_WINDOW_SECS: i64 = 15 * 60;

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
        .method_not_allowed_fallback(|| async { ApiError::not_found("no such route") })
        .with_state(app)
}

// ---------- shapes ----------

#[derive(Serialize)]
struct Member {
    handle: String,
    name: String,
    about: String,
    contexts: Vec<String>,
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
    /// The session the note reached.
    by: Option<SessionOut>,
    /// The session of theirs that claimed the thread, with its context when it claimed it.
    claimed_by: Option<SessionOut>,
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
    context: Option<String>,
    claimed: Option<&'static str>,
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
    /// Who holds the thread for the writer's person afterwards, as `claim` names it.
    claimed: Option<String>,
}

/// A thread as seen by one session of a participant.
struct ThreadRow {
    id: String,
    kind: String,
    title: String,
    from: String,
    opener_id: i64,
    status: String,
    /// The latest note that reached this session.
    delivered_seq: i64,
    last_seq: i64,
    waiting: bool,
    context: Option<String>,
    claimed_by: Option<i64>,
}

/// One thread of an inbox page, with what orders it.
struct Candidate {
    thread: String,
    new: bool,
    waiting: bool,
    last_seq: i64,
}

// ---------- queries shared by the handlers ----------

/// The thread `?3` of person `?1` as session `?2` sees it. A session without its own delivery row
/// has what its person has.
const THREAD_SQL: &str = "
SELECT t.id, t.kind, t.title, o.handle, t.opener_id, t.status,
       coalesce(s.delivered_seq, p.delivered_seq, 0), p.last_seq, p.waiting, p.context,
       p.claimed_by
FROM participants p
JOIN threads t ON t.id = p.thread_id
JOIN people o ON o.id = t.opener_id
LEFT JOIN session_threads s ON s.session_id = ?2 AND s.thread_id = p.thread_id
WHERE p.person_id = ?1 AND p.thread_id = ?3";

fn find_thread(conn: &Connection, session: &Session, id: &str) -> Result<ThreadRow, ApiError> {
    conn.query_row(
        THREAD_SQL,
        params![session.person_id, session.id, id],
        |r| {
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
                context: r.get(9)?,
                claimed_by: r.get(10)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("no such thread"))
}

/// The index ranges that hold the threads a session sees, each as (index on `participants`,
/// index on `session_threads`, condition on `x`, the row read through either). `?2` is the
/// session, `?3` its context and `?4` that context's repository when it names a branch: a thread
/// addressed to the repository alone reaches the session on any branch. With `all`, every thread
/// of the person. The routes do not overlap.
fn routes(context: Option<&str>, all: bool) -> Vec<(&'static str, &'static str, &'static str)> {
    const CLAIM: (&str, &str) = ("participants_inbox_claim", "session_new_claim");
    const CONTEXT: (&str, &str) = ("participants_inbox_context", "session_new_context");
    if all {
        return vec![("participants_inbox", "session_new", "1")];
    }
    let mut routes = vec![(CLAIM.0, CLAIM.1, "x.claimed_by = ?2")];
    let Some(context) = context else {
        routes.push((CLAIM.0, CLAIM.1, "x.claimed_by IS NULL"));
        return routes;
    };
    routes.push((
        CONTEXT.0,
        CONTEXT.1,
        "x.claimed_by IS NULL AND x.context IS NULL",
    ));
    routes.push((
        CONTEXT.0,
        CONTEXT.1,
        "x.claimed_by IS NULL AND x.context = ?3",
    ));
    if context_repo(context).is_some() {
        routes.push((
            CONTEXT.0,
            CONTEXT.1,
            "x.claimed_by IS NULL AND x.context = ?4",
        ));
    }
    routes
}

/// The queries whose rows hold a session's inbox page: for each route, the person's flagged
/// threads in inbox order, and the threads new for this session alone (another session of the
/// person has their latest note) in the order of their group. Each reads its index in order and
/// stops after a page, so the cost does not grow with the number of flagged threads.
///
/// A thread is new for the session when it is new for the person (`x.has_new`: no session of
/// theirs has its latest note) or for the session's own row. The first query orders by the
/// person's flag, so a thread new only for the session reads as only waiting there; `page`
/// takes its flag from the second query. One page of each is enough: if the second query is
/// full, its threads alone fill the page ahead of any it left out; if not, it holds them all.
/// And a range's rows before the last one the page needs are new ones (which the page counts)
/// or needed ones, so they fit in a page.
fn page_sqls(context: Option<&str>, all: bool) -> Vec<String> {
    routes(context, all)
        .into_iter()
        .flat_map(|(person_index, session_index, route)| {
            [
                format!(
                    "SELECT x.thread_id, x.has_new, x.waiting, x.last_seq
                     FROM participants x INDEXED BY {person_index}
                     WHERE x.person_id = ?1 AND x.inbox = 1 AND {route}
                     ORDER BY x.has_new DESC, x.waiting DESC, x.last_seq DESC LIMIT {INBOX_THREADS}"
                ),
                format!(
                    "SELECT x.thread_id, 1, x.waiting, x.last_seq
                     FROM session_threads x INDEXED BY {session_index}
                     WHERE x.session_id = ?2 AND x.has_new = 1 AND {route}
                     ORDER BY x.waiting DESC, x.last_seq DESC LIMIT {INBOX_THREADS}"
                ),
            ]
        })
        .collect()
}

/// How many threads the session's inbox holds: the person's flagged threads it sees, plus those
/// new only for the session. The routes do not overlap, so their counts add up; each counts the
/// entries of one index range, reading nothing else.
fn count_sql(context: Option<&str>, all: bool) -> String {
    let counts: Vec<String> = routes(context, all)
        .into_iter()
        .flat_map(|(person_index, _, route)| {
            [
                format!(
                    "(SELECT count(*) FROM participants x INDEXED BY {person_index}
                      WHERE x.person_id = ?1 AND x.inbox = 1 AND {route})"
                ),
                format!(
                    "(SELECT count(*) FROM session_threads x INDEXED BY session_count
                      WHERE x.session_id = ?2 AND x.has_new = 1 AND x.person_inbox = 0 AND {route})"
                ),
            ]
        })
        .collect();
    format!("SELECT {}", counts.join(" + "))
}

/// Runs one of the inbox queries with the parameters it uses: `?1` person, `?2` session,
/// `?3` context, `?4` its repository when it names a branch.
fn run<T>(
    conn: &Connection,
    sql: &str,
    session: &Session,
    f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, ApiError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let context = session.context.as_deref();
    let text = |t: Option<&str>| t.map_or(SqlValue::Null, |t| SqlValue::Text(t.to_string()));
    let values = [
        SqlValue::Integer(session.person_id),
        SqlValue::Integer(session.id),
        text(context),
        text(context.and_then(context_repo)),
    ];
    let n = stmt.parameter_count();
    let rows = stmt.query_map(params_from_iter(&values[..n]), f)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The threads of a session's inbox page, in inbox order: new ones first (waiting ones first
/// among them), then waiting ones with nothing new; newest first within each.
fn page(conn: &Connection, session: &Session, all: bool) -> Result<Vec<Candidate>, ApiError> {
    let mut found: HashMap<String, Candidate> = HashMap::new();
    for sql in page_sqls(session.context.as_deref(), all) {
        let rows = run(conn, &sql, session, |r| {
            Ok(Candidate {
                thread: r.get(0)?,
                new: r.get(1)?,
                waiting: r.get(2)?,
                last_seq: r.get(3)?,
            })
        })?;
        for c in rows {
            found
                .entry(c.thread.clone())
                .and_modify(|seen| seen.new |= c.new)
                .or_insert(c);
        }
    }
    let mut page: Vec<Candidate> = found.into_values().collect();
    page.sort_by_key(|c| std::cmp::Reverse((c.new, c.waiting, c.last_seq)));
    page.truncate(INBOX_THREADS);
    Ok(page)
}

fn inbox_count(conn: &Connection, session: &Session, all: bool) -> Result<usize, ApiError> {
    let sql = count_sql(session.context.as_deref(), all);
    let counts = run(conn, &sql, session, |r| r.get::<_, i64>(0))?;
    Ok(counts.first().copied().unwrap_or(0) as usize)
}

/// Recomputes what the rows of a thread know about it (latest note, new, waiting, inbox, and the
/// copies on the sessions' rows). Runs after every change to the thread: a note, a close, a
/// delivery, a claim.
fn refresh(conn: &Connection, thread: &str) -> Result<(), ApiError> {
    // A thread is opened by its first note, so it always has a latest one.
    let (last_seq, last_author, open_direct): (i64, i64, bool) = conn.query_row(
        "SELECT n.seq, n.author_id, t.kind = 'direct' AND t.status = 'open'
         FROM threads t JOIN notes n ON n.thread_id = t.id
         WHERE t.id = ?1 ORDER BY n.seq DESC LIMIT 1",
        [thread],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    conn.execute(
        "UPDATE participants SET
             last_seq = ?2,
             has_new = coalesce(delivered_seq, 0) < last_other_seq,
             waiting = ?3 AND person_id != ?4,
             inbox = coalesce(delivered_seq, 0) < last_other_seq OR (?3 AND person_id != ?4)
         WHERE thread_id = ?1",
        params![thread, last_seq, open_direct, last_author],
    )?;
    conn.execute(
        "UPDATE session_threads AS s SET
             has_new = s.delivered_seq < p.last_other_seq,
             waiting = p.waiting,
             last_seq = p.last_seq,
             claimed_by = p.claimed_by,
             context = p.context,
             person_inbox = p.inbox
         FROM participants p
         WHERE s.thread_id = ?1 AND p.thread_id = s.thread_id AND p.person_id = s.person_id",
        [thread],
    )?;
    Ok(())
}

/// Every active person, with the contexts of their sessions seen in the last 15 minutes.
fn team(conn: &Connection, now: i64) -> Result<Vec<Member>, ApiError> {
    let mut contexts: HashMap<i64, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT person_id, context FROM sessions
         WHERE context IS NOT NULL AND last_seen > ?1 ORDER BY context",
    )?;
    let rows = stmt.query_map([now - CONTEXT_WINDOW_SECS], |r| Ok((r.get(0)?, r.get(1)?)))?;
    for row in rows {
        let (person, context) = row?;
        contexts.entry(person).or_default().push(context);
    }
    let mut stmt = conn
        .prepare("SELECT id, handle, name, about FROM people WHERE active = 1 ORDER BY handle")?;
    let rows = stmt.query_map([], |r| {
        Ok(Member {
            contexts: contexts.remove(&r.get(0)?).unwrap_or_default(),
            handle: r.get(1)?,
            name: r.get(2)?,
            about: r.get(3)?,
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

/// Delivers the notes of `thread` up to `seq` to `session`. When that moves what its person has,
/// every other session of theirs first gets a row keeping what it had, so that it still gets
/// those notes. Callers `refresh` the thread afterwards.
fn deliver(
    conn: &Connection,
    session: &Session,
    thread: &str,
    seq: i64,
    now: i64,
) -> Result<(), ApiError> {
    let person_has: i64 = conn.query_row(
        "SELECT coalesce(delivered_seq, 0) FROM participants
         WHERE thread_id = ?1 AND person_id = ?2",
        params![thread, session.person_id],
        |r| r.get(0),
    )?;
    if seq > person_has {
        conn.execute(
            "INSERT OR IGNORE INTO session_threads (session_id, thread_id, person_id, delivered_seq)
             SELECT id, ?2, ?3, ?4 FROM sessions WHERE person_id = ?3 AND id != ?1",
            params![session.id, thread, session.person_id, person_has],
        )?;
        conn.execute(
            "UPDATE participants SET delivered_seq = ?3, delivered_at = ?4,
                 delivered_label = ?5, delivered_context = ?6
             WHERE thread_id = ?1 AND person_id = ?2",
            params![
                thread,
                session.person_id,
                seq,
                now,
                session.label,
                session.context
            ],
        )?;
    }
    conn.execute(
        "UPDATE session_threads SET delivered_seq = ?3
         WHERE session_id = ?1 AND thread_id = ?2 AND delivered_seq < ?3",
        params![session.id, thread, seq],
    )?;
    Ok(())
}

fn thread_out(
    conn: &Connection,
    session: &Session,
    row: &ThreadRow,
) -> Result<ThreadOut, ApiError> {
    Ok(ThreadOut {
        id: row.id.clone(),
        kind: row.kind.clone(),
        title: row.title.clone(),
        from: row.from.clone(),
        participants: participants(conn, &row.id)?,
        status: row.status.clone(),
        waiting_for_you: row.waiting,
        context: row.context.clone(),
        claimed: claim(row.claimed_by, session),
        new_notes: None,
        notes: None,
        delivery: None,
    })
}

/// Whose a claim is, as the session that asks sees it: `None` (unclaimed), `"this-session"` or
/// `"other-session"`.
fn claim(claimed_by: Option<i64>, session: &Session) -> Option<&'static str> {
    claimed_by.map(|by| {
        if by == session.id {
            "this-session"
        } else {
            "other-session"
        }
    })
}

/// For each participant, in order: the latest note that reached one of their sessions, which
/// session that was, and which session claimed the thread (each with its context then).
fn delivery(conn: &Connection, thread: &str) -> Result<Vec<Delivery>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT pe.handle, n.id, p.delivered_at, p.delivered_label, p.delivered_context,
                ct.label, p.claimed_context
         FROM participants p JOIN people pe ON pe.id = p.person_id
         LEFT JOIN notes n ON n.seq = p.delivered_seq
         LEFT JOIN sessions cs ON cs.id = p.claimed_by
         LEFT JOIN tokens ct ON ct.id = cs.token_id
         WHERE p.thread_id = ?1 ORDER BY p.rowid",
    )?;
    let rows = stmt.query_map([thread], |r| {
        let session = |label: Option<String>, context: Option<String>| {
            label.map(|label| SessionOut { label, context })
        };
        Ok(Delivery {
            handle: r.get(0)?,
            note: r.get(1)?,
            delivered_at: r.get::<_, Option<i64>>(2)?.map(rfc3339),
            by: session(r.get(3)?, r.get(4)?),
            claimed_by: session(r.get(5)?, r.get(6)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `inbox`'s query string: `all` (`1`/`true` or `0`/`false`) and `wait` (0 to 60 seconds).
fn inbox_params(
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<(bool, u64), ApiError> {
    let Query(pairs) = query.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (mut all, mut wait) = (None, None);
    for (name, value) in pairs {
        match name.as_str() {
            "all" if all.is_none() => {
                all = Some(match value.as_str() {
                    "1" | "true" => true,
                    "0" | "false" => false,
                    _ => return Err(ApiError::bad_request("'all' must be 1, 0, true or false")),
                })
            }
            "wait" if wait.is_none() => {
                wait = Some(
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|w| *w <= MAX_WAIT_SECS)
                        .ok_or_else(|| ApiError::bad_request("'wait' must be 0 to 60 seconds"))?,
                )
            }
            "all" | "wait" => return Err(ApiError::bad_request(format!("'{name}' given twice"))),
            _ => return Err(ApiError::bad_request(format!("unknown parameter '{name}'"))),
        }
    }
    Ok((all.unwrap_or(false), wait.unwrap_or(0)))
}

/// Runs `op` as the caller's session, in one transaction, after recording that the session was
/// seen. `op` runs in a savepoint, so when it fails only its own writes are undone: a failed
/// request still counts as seen.
/// `arrived` as in `touch`.
fn as_session<T>(
    app: &App,
    caller: &Caller,
    arrived: bool,
    op: impl FnOnce(&Connection, &Session, i64) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let mut conn = app.db();
    let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read under the lock, so that times follow the order of `seq`.
    let now = app.clock.now();
    let session = touch(&tx, app, caller, now, arrived)?;
    let out = {
        let op_tx = tx.savepoint()?;
        let out = op(&op_tx, &session, now);
        if out.is_ok() {
            op_tx.commit()?;
        }
        out
    };
    tx.commit()?;
    out
}

// ---------- handlers ----------

async fn people(State(app): State<Arc<App>>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let team = as_session(&app, &caller, true, |tx, _, now| team(tx, now))?;
    Ok(Json(json!({ "team": team })))
}

async fn inbox(
    State(app): State<Arc<App>>,
    caller: Caller,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let (all, wait) = inbox_params(query)?;
    if wait > 0 {
        let _slot = app.wait_slot(&caller.token_id).ok_or_else(|| {
            ApiError::rate_limited(&format!(
                "at most {MAX_WAITS_PER_TOKEN} waiting inbox requests per token; wait for one to answer"
            ))
        })?;
        // Subscribed before the first look, so a note landing in between still wakes us.
        let mut news = app.news(caller.person_id);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
        let mut first = true;
        loop {
            news.borrow_and_update();
            let look = look(&app, &caller, all, first)?;
            if look.new {
                break;
            }
            first = false;
            // A claim lapsing may give this session a thread: look again then too.
            let wake = look.lapse_in.map_or(deadline, |secs| {
                deadline.min(tokio::time::Instant::now() + Duration::from_secs(secs))
            });
            // The database lock is not held here: `look` let it go.
            match tokio::time::timeout_at(wake, news.changed()).await {
                Ok(Ok(())) => continue,
                Err(_) if wake < deadline => continue,
                _ => break,
            }
        }
    }
    // A request that waited answers with its session's context as it is now, which another
    // request of the session may have changed meanwhile.
    Ok(Json(answer_inbox(&app, &caller, all, wait == 0)?))
}

/// What a waiting `inbox` request found when it looked.
struct Look {
    /// The session has a thread with new notes.
    new: bool,
    /// Seconds until the next claim of the person's sessions lapses, if one does.
    lapse_in: Option<u64>,
}

/// Whether the caller's session has a thread with new notes, without delivering anything. The
/// `first` look records the session, with the context the request sent; later ones do not
/// record it, and use the session's context as it is now. Every look first ends lapsed claims.
fn look(app: &App, caller: &Caller, all: bool, first: bool) -> Result<Look, ApiError> {
    let look = |tx: &Connection, session: &Session, now: i64| {
        Ok(Look {
            new: page(tx, session, all)?.first().is_some_and(|c| c.new),
            lapse_in: next_lapse(tx, session.person_id)?.map(|at| (at - now).max(1) as u64),
        })
    };
    if first {
        return as_session(app, caller, true, look);
    }
    let mut conn = app.db();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = app.clock.now();
    lapse(&tx, app, now)?;
    let found = match find(&tx, caller)? {
        Some(session) => look(&tx, &session, now)?,
        // Forgotten while waiting (its token started too many others): answer, which records it.
        None => Look {
            new: true,
            lapse_in: None,
        },
    };
    tx.commit()?;
    Ok(found)
}

/// The inbox as it is now, delivering the threads it returns. `arrived` as in `touch`.
fn answer_inbox(app: &App, caller: &Caller, all: bool, arrived: bool) -> Result<Value, ApiError> {
    as_session(app, caller, arrived, |tx, session, now| {
        let page = page(tx, session, all)?;
        let more = inbox_count(tx, session, all)?.saturating_sub(page.len());
        let mut threads = Vec::new();
        for candidate in &page {
            let row = find_thread(tx, session, &candidate.thread)?;
            let mut out = thread_out(tx, session, &row)?;
            out.new_notes = Some(notes_after(
                tx,
                &row.id,
                row.delivered_seq,
                Some(INBOX_TEXT_CHARS),
            )?);
            deliver(tx, session, &row.id, row.last_seq, now)?;
            refresh(tx, &row.id)?;
            threads.push(out);
        }
        let team = team(tx, now)?;
        tracing::info!(
            person = %caller.handle,
            session = session.id,
            all,
            threads = threads.len(),
            more,
            "inbox"
        );
        Ok(json!({
            "team": team,
            "session": session.out(),
            "threads": threads,
            "more": more,
        }))
    })
}

async fn thread(
    State(app): State<Arc<App>>,
    caller: Caller,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<ThreadOut>, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::not_found("no such thread"))?;
    let out = as_session(&app, &caller, true, |tx, session, now| {
        let row = find_thread(tx, session, &id)?;
        deliver(tx, session, &row.id, row.last_seq, now)?;
        refresh(tx, &row.id)?;
        let mut out = thread_out(tx, session, &row)?;
        out.notes = Some(notes_after(tx, &row.id, 0, None)?);
        out.delivery = Some(delivery(tx, &row.id)?);
        tracing::info!(person = %caller.handle, session = session.id, thread = %row.id, "thread");
        Ok(out)
    })?;
    Ok(Json(out))
}

async fn send(
    State(app): State<Arc<App>>,
    caller: Caller,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<SendResult>, ApiError> {
    let body = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::too_large(StatusCode::PAYLOAD_TOO_LARGE, "request body over 400 KiB")
        } else {
            ApiError::bad_request(e.body_text())
        }
    })?;
    let invalid = |e: serde_json::Error| ApiError::bad_request(format!("invalid send body: {e}"));
    // Serde would also read the struct from a JSON array, by position; only an object is a send.
    let req: SendRequest = match serde_json::from_slice(&body).map_err(invalid)? {
        Value::Object(fields) => serde_json::from_value(Value::Object(fields)).map_err(invalid)?,
        _ => return Err(ApiError::bad_request("the send body must be a JSON object")),
    };
    let (result, woken) = write(&app, &caller, req)?;
    app.announce(woken);
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
/// Returns the result and the people to wake: the other participants, when a note was written.
fn write(app: &App, caller: &Caller, req: SendRequest) -> Result<(SendResult, Vec<i64>), ApiError> {
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

    as_session(app, caller, true, |tx, session, now| {
        write_as(tx, app, caller, session, &req, now)
    })
}

/// `send`'s writes, as `session`.
fn write_as(
    tx: &Connection,
    app: &App,
    caller: &Caller,
    session: &Session,
    req: &SendRequest,
    now: i64,
) -> Result<(SendResult, Vec<i64>), ApiError> {
    // Keys expire 24 hours after the send that used them. Forgetting them here keeps the table
    // small, and lets an expired key be used again as a fresh send.
    tx.execute(
        "DELETE FROM send_keys WHERE created_at <= ?1",
        [now - KEY_LIFETIME_SECS],
    )?;
    if let Some(key) = &req.key {
        let first = tx
            .query_row(
                "SELECT thread_id, note_id, status, claimed FROM send_keys
                 WHERE person_id = ?1 AND key = ?2",
                params![caller.person_id, key],
                |r| {
                    Ok(SendResult {
                        thread: r.get(0)?,
                        note: r.get(1)?,
                        status: r.get(2)?,
                        claimed: r.get(3)?,
                    })
                },
            )
            .optional()?;
        if let Some(first) = first {
            return Ok((first, Vec::new()));
        }
    }

    // `caught_up`: every note of the thread had reached this session before it wrote, the
    // person's own from other sessions included.
    let (thread_id, caught_up) = match (&req.to, &req.thread) {
        (Some(to), _) => {
            let text = req.text.as_deref().unwrap_or_default();
            (open_thread(tx, app, caller, to, text, now)?, true)
        }
        (None, Some(id)) => {
            let row = find_thread(tx, session, id)?;
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
            let caught_up = row.delivered_seq >= row.last_seq;
            (row.id, caught_up)
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
                return Err(ApiError::rate_limited(
                    "at most 60 notes per minute; slow down",
                ));
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
            let seq = tx.last_insert_rowid();
            tx.execute(
                "UPDATE participants SET last_other_seq = ?2 WHERE thread_id = ?1 AND person_id != ?3",
                params![thread_id, seq, caller.person_id],
            )?;
            // The writing session has the thread up to its note, unless earlier notes never
            // reached it: then nothing moves, so those notes are not skipped.
            if caught_up {
                deliver(tx, session, &thread_id, seq, now)?;
            }
            // Writing claims the thread for this session, unless another session of the person
            // holds it: a claim is never taken from a session that still has it.
            // (A lapsed claim was already ended when this request was seen.)
            let claimed = tx.execute(
                "UPDATE participants SET claimed_by = ?3, claimed_context = ?4
                 WHERE thread_id = ?1 AND person_id = ?2
                   AND (claimed_by IS NULL OR claimed_by = ?3)",
                params![thread_id, caller.person_id, session.id, session.context],
            )?;
            if claimed > 0 {
                tx.execute("UPDATE sessions SET claims = 1 WHERE id = ?1", [session.id])?;
            }
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
    refresh(tx, &thread_id)?;
    let claimed_by: Option<i64> = tx.query_row(
        "SELECT claimed_by FROM participants WHERE thread_id = ?1 AND person_id = ?2",
        params![thread_id, caller.person_id],
        |r| r.get(0),
    )?;
    let result = SendResult {
        thread: thread_id,
        note: note_id,
        status: if req.close { "closed" } else { "open" }.to_string(),
        claimed: claim(claimed_by, session).map(str::to_string),
    };
    if let Some(key) = &req.key {
        tx.execute(
            "INSERT INTO send_keys (person_id, key, thread_id, note_id, status, claimed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                caller.person_id,
                key,
                result.thread,
                result.note,
                result.status,
                result.claimed,
                now
            ],
        )?;
    }
    let woken: Vec<i64> = match result.note {
        Some(_) => tx
            .prepare("SELECT person_id FROM participants WHERE thread_id = ?1 AND person_id != ?2")?
            .query_map(params![result.thread, caller.person_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        None => Vec::new(),
    };
    Ok((result, woken))
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
    let addresses = addresses(to)?;
    let is_team = addresses.iter().any(|(h, _)| h == "team");

    // The others, each with the context the thread is addressed to for them.
    let (kind, others): (&str, Vec<(i64, Option<String>)>) = if is_team {
        if addresses.len() != 1 {
            return Err(ApiError::bad_request(
                "'@team' must be the only entry of 'to'",
            ));
        }
        if addresses[0].1.is_some() {
            return Err(ApiError::bad_request("'@team' takes no context"));
        }
        let others: Vec<(i64, Option<String>)> = conn
            .prepare("SELECT id FROM people WHERE active = 1 AND id != ?1 ORDER BY handle")?
            .query_map([caller.person_id], |r| Ok((r.get(0)?, None)))?
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
        if addresses.is_empty() || addresses.len() > MAX_RECIPIENTS {
            return Err(ApiError::bad_request(
                "'to' needs 1 to 20 handles, or [\"@team\"]",
            ));
        }
        let mut others = Vec::new();
        for (handle, context) in addresses {
            if handle == caller.handle {
                return Err(ApiError::bad_request("you cannot send a note to yourself"));
            }
            let id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM people WHERE handle = ?1 AND active = 1",
                    [&handle],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => others.push((id, context)),
                None => {
                    let valid = serde_json::to_value(team(conn, now)?).unwrap_or_default();
                    return Err(ApiError::unknown_person(&handle, valid));
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
    let mut add = conn
        .prepare("INSERT INTO participants (thread_id, person_id, context) VALUES (?1, ?2, ?3)")?;
    add.execute(params![id, caller.person_id, None::<String>])?;
    for (person, context) in others {
        add.execute(params![id, person, context])?;
    }
    Ok(id)
}

/// `to`, read as (handle, context) pairs: without leading `@`, lowercased, `equipo` read as
/// `team`, and repeats merged (the first one kept). The same person under two contexts (or
/// with and without one) is a `BAD_REQUEST`.
fn addresses(to: &[String]) -> Result<Vec<(String, Option<String>)>, ApiError> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for raw in to {
        let address = raw.trim().trim_start_matches('@').to_lowercase();
        let (handle, context) = match address.split_once('/') {
            Some((handle, context)) => (handle.to_string(), Some(context.to_string())),
            None => (address, None),
        };
        if let Some(context) = &context
            && !valid_context(context)
        {
            return Err(ApiError::bad_request(format!(
                "'{raw}': a context must be {CONTEXT_FORMAT}"
            )));
        }
        let handle = if RESERVED_HANDLES.contains(&handle.as_str()) {
            "team".to_string()
        } else {
            handle
        };
        match out.iter().find(|(seen, _)| *seen == handle) {
            Some((_, seen)) if *seen == context => {}
            Some(_) => {
                return Err(ApiError::bad_request(format!(
                    "'{handle}' is named twice with different contexts"
                )));
            }
            None => out.push((handle, context)),
        }
    }
    Ok(out)
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
    use super::{count_sql, page_sqls, title_of};
    use rusqlite::types::Value as SqlValue;

    fn plan(sql: &str) -> Vec<String> {
        let conn = crate::db::open(":memory:").unwrap();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let n = stmt.parameter_count();
        let values = [
            SqlValue::Integer(1),
            SqlValue::Integer(2),
            SqlValue::Text("a@b".into()),
            SqlValue::Text("a".into()),
        ];
        stmt.query_map(rusqlite::params_from_iter(&values[..n]), |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// The inbox must cost the same for 20 flagged threads as for 100,000: agents poll it, a
    /// runaway agent can open thousands of threads a day for one person, and it runs under the
    /// lock every request needs. So each query of a session's page reads its rows already in
    /// inbox order from an index whose leading columns pick exactly the threads that session
    /// sees, and stops after a page: no sort, no rows read only to be skipped, and nothing
    /// done per thread it does not return.
    #[test]
    fn inbox_page_reads_index_ranges_in_order_without_sorting() {
        for (context, all, queries) in [
            (None, false, 4),
            (Some("pagos-api"), false, 6),
            (Some("pagos-api@main"), false, 8),
            (None, true, 2),
        ] {
            let sqls = page_sqls(context, all);
            assert_eq!(sqls.len(), queries);
            for sql in sqls {
                let plan = plan(&sql);
                let first = &plan[0];
                assert!(
                    first.starts_with("SEARCH x USING INDEX participants_inbox")
                        || first.starts_with("SEARCH x USING INDEX session_new"),
                    "{plan:#?}"
                );
                // Every condition is an equality on the index's leading columns, so the range
                // holds only threads the session sees.
                let leading = if all {
                    "_id=?)"
                } else {
                    "_id=? AND claimed_by=?"
                };
                assert!(first.contains(leading), "{plan:#?}");
                assert!(!sql.contains(" OR x."), "{sql}");
                assert!(
                    !plan.iter().any(|step| step.contains("TEMP B-TREE")
                        || step.contains("SUBQUERY")
                        || step.starts_with("SCAN")),
                    "{plan:#?}"
                );
                assert_eq!(plan.len(), 1, "{plan:#?}");
            }
        }
    }

    /// `more` is a count, so it reads every entry it counts; but only those, and only index
    /// entries: never a table row, nor a flagged thread the session does not see (one claimed
    /// by another session, or addressed to another context).
    #[test]
    fn inbox_count_reads_only_the_index_entries_it_counts() {
        for context in [None, Some("pagos-api"), Some("pagos-api@main")] {
            for all in [false, true] {
                let plan = plan(&count_sql(context, all));
                let steps: Vec<&String> = plan.iter().filter(|s| s.contains(" x ")).collect();
                assert!(!steps.is_empty(), "{plan:#?}");
                for step in steps {
                    assert!(
                        step.starts_with("SEARCH x USING COVERING INDEX"),
                        "{plan:#?}"
                    );
                    if !all {
                        assert!(step.contains("_id=? AND claimed_by"), "{plan:#?}");
                    }
                }
                assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:#?}");
            }
        }
    }

    #[test]
    fn titles() {
        assert_eq!(title_of("\n# Refunds endpoint\nmore"), "Refunds endpoint");
        assert_eq!(title_of(&"é".repeat(100)).chars().count(), 80);
    }
}
