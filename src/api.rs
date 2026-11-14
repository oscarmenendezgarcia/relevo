//! The HTTP API: `send`, `inbox`, `thread`, `people`, ending a session and `health` (the
//! read-only timeline is in `crate::timeline`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Caller;
use crate::db::{
    CONTEXT_FORMAT, RESERVED_HANDLES, SYSTEM_PERSON_SQL, context_repo, random_id, rfc3339,
    valid_context,
};
use crate::error::ApiError;
use crate::mention::{Mention, mentions, mentions_only};
use crate::session::{Session, SessionOut, end, find, lapse, next_lapse, touch};
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
        .route("/api/timeline", get(crate::timeline::list))
        .route("/api/timeline/threads/{id}", get(crate::timeline::thread))
        .route("/api/session", delete(end_session))
        .route("/api/dist/{name}", get(crate::dist::file))
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
    /// The teams they are in, by name.
    teams: Vec<String>,
}

/// A team, with its active members.
#[derive(Serialize)]
struct TeamOut {
    name: String,
    title: String,
    members: Vec<String>,
}

#[derive(Serialize)]
struct NoteOut {
    id: String,
    author: String,
    via: String,
    created_at: String,
    text: String,
    truncated: bool,
    /// Written by the server's own person: the release notes (left out otherwise).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    system: bool,
}

#[derive(Serialize)]
struct Delivery {
    handle: String,
    /// The thread names them: only then can it be pending for them.
    named: bool,
    note: Option<String>,
    delivered_at: Option<String>,
    /// The session the note reached.
    by: Option<SessionOut>,
    /// The session of theirs that claimed the thread, with its context when it claimed it and
    /// since when it holds it.
    claimed_by: Option<ClaimOut>,
    /// How many of their live sessions have `note`.
    seen_by: i64,
    /// When the person handled `note` (wrote in the thread or marked it seen), and
    /// from which session; `None` while it is not handled.
    handled_at: Option<String>,
    handled_by: Option<SessionOut>,
}

/// Who holds a thread for a participant, and since when.
#[derive(Serialize)]
struct ClaimOut {
    label: String,
    context: Option<String>,
    since: String,
}

/// Another participant who holds the thread now: whose session, its context when it claimed it,
/// and since when.
#[derive(Serialize)]
struct HolderOut {
    handle: String,
    #[serde(flatten)]
    claim: ClaimOut,
}

#[derive(Serialize)]
struct ThreadOut {
    id: String,
    kind: String,
    title: String,
    from: String,
    participants: Vec<String>,
    /// The teams the opening note named, by their names then.
    teams: Vec<String>,
    /// The person is a named participant.
    named: bool,
    /// Always `"open"`: threads no longer close (kept through 0.x for clients that read it).
    status: &'static str,
    /// Pending for the person: naming them, with a note by someone else they have not handled.
    pending: bool,
    context: Option<String>,
    claimed: Option<&'static str>,
    /// The other participants who hold the thread now.
    held_by: Vec<HolderOut>,
    /// In `inbox`: the thread's latest note, whoever wrote it and whether or not it reached
    /// this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_note: Option<NoteOut>,
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
    /// Deprecated (through 0.x): opens a thread as 0.3 did, for clients not yet updated.
    to: Option<Vec<String>>,
    thread: Option<String>,
    text: Option<String>,
    /// With `thread`: claims it for this session, without a note when there is no `text`.
    #[serde(default)]
    claim: bool,
    /// With `thread` and no `text`: the person has seen the thread up to the latest note that
    /// reached this session, with nothing to write: it is handled. Releases this session's claim.
    #[serde(default)]
    seen: bool,
    /// Deprecated (through 0.x): `seen`'s name up to 0.4.x.
    #[serde(default)]
    done: bool,
    /// Deprecated (through 0.x): threads no longer close. Without `text`, read as `seen`; with
    /// `text`, ignored.
    #[serde(default)]
    close: bool,
    key: Option<String>,
}

#[derive(Serialize)]
struct SendResult {
    thread: String,
    /// `None` when the send wrote none (a claim or a `seen` alone).
    note: Option<String>,
    /// Always `"open"` (kept through 0.x for clients that read it).
    status: &'static str,
    /// Who holds the thread for the writer's person afterwards, as `claim` names it for the
    /// session that asks.
    claimed: Option<&'static str>,
    #[serde(flatten)]
    told: Told,
}

/// What a `send` says about whom it reached; kept with its key, for a replay.
#[derive(Serialize, Deserialize)]
struct Told {
    kind: String,
    /// Whom this note's mentions (or `to`) named.
    to: Named,
    /// The people this note added to the thread.
    added: Vec<String>,
    /// How many people other than the writer take part in the thread after it.
    reached: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct Named {
    people: Vec<String>,
    teams: Vec<String>,
}

/// A thread as seen by one session of a participant.
struct ThreadRow {
    id: String,
    kind: String,
    title: String,
    from: String,
    /// The latest note that reached this session.
    delivered_seq: i64,
    last_seq: i64,
    /// Pending for the person.
    waiting: bool,
    context: Option<String>,
    claimed_by: Option<i64>,
    /// The thread names the person.
    named: bool,
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
SELECT t.id, t.kind, t.title, o.handle,
       coalesce(s.delivered_seq, p.delivered_seq, 0), p.last_seq, p.waiting, p.context,
       p.claimed_by, p.named_seq IS NOT NULL
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
                delivered_seq: r.get(4)?,
                last_seq: r.get(5)?,
                waiting: r.get(6)?,
                context: r.get(7)?,
                claimed_by: r.get(8)?,
                named: r.get(9)?,
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

/// Recomputes what the rows of a thread know about it (latest note, new, waiting (pending),
/// inbox, fresh, and the copies on the sessions' rows). Runs after every change to the thread: a
/// note, a delivery, a claim, a person handling it, a person named.
pub(crate) fn refresh(conn: &Connection, thread: &str) -> rusqlite::Result<()> {
    // A thread is opened by its first note, so it always has a latest one.
    let last_seq: i64 = conn.query_row(
        "SELECT max(seq) FROM notes WHERE thread_id = ?1",
        [thread],
        |r| r.get(0),
    )?;
    // Pending (`waiting`): naming the person, with a note by someone else after what they
    // handled, and from the one that named them on (`FLOOR`). A thread never closes, so a note
    // written after they handled it is pending again. Fresh: pending, and one of their sessions
    // has a note they have not handled.
    let pending = format!("(named_seq IS NOT NULL AND {FLOOR} < last_other_seq)");
    conn.execute(
        &format!(
            "UPDATE participants SET
                 last_seq = ?2,
                 has_new = coalesce(delivered_seq, 0) < last_other_seq,
                 waiting = {pending},
                 inbox = coalesce(delivered_seq, 0) < last_other_seq OR ({pending}),
                 fresh = {pending} AND {FLOOR} < coalesce(delivered_seq, 0)
             WHERE thread_id = ?1"
        ),
        params![thread, last_seq],
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

/// How far a named participant's pending notes start: after what they handled, and from the note
/// that named them on (what came before was optional for them). `NULL` when not named. A new
/// session starts there in a fresh thread (`session::START_SQL`).
const FLOOR: &str = "max(coalesce(handled_seq, 0), named_seq - 1)";

/// The teams each person is in, by name.
fn teams_of(conn: &Connection) -> Result<HashMap<i64, Vec<String>>, ApiError> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT m.person_id, t.name FROM team_members m JOIN teams t ON t.id = m.team_id
         ORDER BY t.name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    for row in rows {
        let (person, team) = row?;
        out.entry(person).or_default().push(team);
    }
    Ok(out)
}

/// Every active person, with the contexts of their sessions seen in the last 15 minutes and the
/// teams they are in.
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
    let mut teams = teams_of(conn)?;
    let mut stmt = conn
        .prepare("SELECT id, handle, name, about FROM people WHERE active = 1 ORDER BY handle")?;
    let rows = stmt.query_map([], |r| {
        let id: i64 = r.get(0)?;
        Ok(Member {
            contexts: contexts.remove(&id).unwrap_or_default(),
            teams: teams.remove(&id).unwrap_or_default(),
            handle: r.get(1)?,
            name: r.get(2)?,
            about: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every team, by name, with its active members, by handle.
fn teams(conn: &Connection) -> Result<Vec<TeamOut>, ApiError> {
    let mut out: Vec<(i64, TeamOut)> = conn
        .prepare("SELECT id, name, title FROM teams ORDER BY name")?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                TeamOut {
                    name: r.get(1)?,
                    title: r.get(2)?,
                    members: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "SELECT m.team_id, p.handle FROM team_members m JOIN people p ON p.id = m.person_id
         WHERE p.active = 1 ORDER BY p.handle",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?)))?;
    for row in rows {
        let (team, handle) = row?;
        if let Some((_, t)) = out.iter_mut().find(|(id, _)| *id == team) {
            t.members.push(handle);
        }
    }
    Ok(out.into_iter().map(|(_, t)| t).collect())
}

/// The teams the note that opened a thread named, sorted.
fn thread_teams(conn: &Connection, thread: &str) -> Result<Vec<String>, ApiError> {
    let rows = conn
        .prepare_cached("SELECT name FROM thread_teams WHERE thread_id = ?1 ORDER BY name")?
        .query_map([thread], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The participants of a thread, in order; never the server's own person, who only posts.
fn participants(conn: &Connection, thread: &str) -> Result<Vec<String>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT pe.handle FROM participants p JOIN people pe ON pe.id = p.person_id
         WHERE p.thread_id = ?1 AND p.person_id IS NOT {SYSTEM_PERSON_SQL} ORDER BY p.rowid"
    ))?;
    let rows = stmt.query_map([thread], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `n.author_id IS (…)`: the server's own person (`db::SYSTEM_PERSON_SQL`), one key read.
const NOTE_COLUMNS: &str = "SELECT n.id, a.handle, n.via, n.created_at, n.text,
       n.author_id IS (SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'system_person')
     FROM notes n JOIN people a ON a.id = n.author_id";

/// The note `?1` of thread `?2`, by its primary key: one row read per thread shown.
const NOTE_AT_SQL: &str = "WHERE n.seq = ?1 AND n.thread_id = ?2";

/// A row of `NOTE_COLUMNS` as a note, its text cut to `max_chars` when given.
fn note_out(r: &rusqlite::Row, max_chars: Option<usize>) -> rusqlite::Result<NoteOut> {
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
        system: r.get(5)?,
    })
}

/// Notes of a thread after `seq`, oldest first, cut to `max_chars` when given.
fn notes_after(
    conn: &Connection,
    thread: &str,
    seq: i64,
    max_chars: Option<usize>,
) -> Result<Vec<NoteOut>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "{NOTE_COLUMNS} WHERE n.thread_id = ?1 AND n.seq > ?2 ORDER BY n.seq"
    ))?;
    let rows = stmt.query_map(params![thread, seq], |r| note_out(r, max_chars))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The note `seq` of a thread, cut to `max_chars` when given; `None` if it has no such note.
fn note_at(
    conn: &Connection,
    thread: &str,
    seq: i64,
    max_chars: Option<usize>,
) -> Result<Option<NoteOut>, ApiError> {
    Ok(conn
        .query_row(
            &format!("{NOTE_COLUMNS} {NOTE_AT_SQL}"),
            params![seq, thread],
            |r| note_out(r, max_chars),
        )
        .optional()?)
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
        // When and where a note reached the person is told for notes by others only: their own
        // say nothing about whether they read the others'.
        let by_others: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM notes
                            WHERE thread_id = ?1 AND seq > ?2 AND seq <= ?3 AND author_id != ?4)",
            params![thread, person_has, seq, session.person_id],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE participants SET delivered_seq = ?3,
                 delivered_at = CASE WHEN ?7 THEN ?4 ELSE delivered_at END,
                 delivered_label = CASE WHEN ?7 THEN ?5 ELSE delivered_label END,
                 delivered_context = CASE WHEN ?7 THEN ?6 ELSE delivered_context END
             WHERE thread_id = ?1 AND person_id = ?2",
            params![
                thread,
                session.person_id,
                seq,
                now,
                session.label,
                session.context,
                by_others
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
        teams: thread_teams(conn, &row.id)?,
        named: row.named,
        status: "open",
        pending: row.waiting,
        context: row.context.clone(),
        claimed: claim(row.claimed_by, session),
        held_by: held_by(conn, &row.id, session.person_id)?,
        last_note: None,
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

/// The live claims of the participants of thread `?1` other than person `?2`, in participant
/// order: the thread's participant rows by their key, and each holder's session, token and handle
/// by theirs. (Lapsed claims were ended when the request was seen.)
const HELD_BY_SQL: &str = "
SELECT pe.handle, tk.label, p.claimed_context, p.claimed_at
FROM participants p
JOIN sessions cs ON cs.id = p.claimed_by
JOIN tokens tk ON tk.id = cs.token_id
JOIN people pe ON pe.id = p.person_id
WHERE p.thread_id = ?1 AND p.person_id != ?2
ORDER BY p.rowid";

fn held_by(conn: &Connection, thread: &str, me: i64) -> Result<Vec<HolderOut>, ApiError> {
    let rows = conn
        .prepare_cached(HELD_BY_SQL)?
        .query_map(params![thread, me], |r| {
            Ok(HolderOut {
                handle: r.get(0)?,
                claim: ClaimOut {
                    label: r.get(1)?,
                    context: r.get(2)?,
                    since: rfc3339(r.get(3)?),
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// For each participant, in order: the latest note by someone else that reached one of their
/// sessions, which session it reached first, which session claimed the thread (each with its
/// context then) and when, how many of their live sessions have that note (those without a row
/// of their own have what their person has), and whether the person has handled it, when and
/// from which session.
fn delivery(conn: &Connection, thread: &str) -> Result<Vec<Delivery>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT pe.handle, n.id, p.delivered_at, p.delivered_label, p.delivered_context,
                ct.label, p.claimed_context, p.claimed_at,
                (SELECT count(*) FROM sessions s
                 LEFT JOIN session_threads st ON st.session_id = s.id AND st.thread_id = p.thread_id
                 WHERE s.person_id = p.person_id
                   AND coalesce(st.delivered_seq, p.delivered_seq) >= n.seq),
                p.handled_seq >= n.seq, p.handled_at, p.handled_label, p.handled_context,
                p.named_seq IS NOT NULL
         FROM participants p JOIN people pe ON pe.id = p.person_id
         LEFT JOIN notes n ON n.seq = (
             SELECT max(o.seq) FROM notes o
             WHERE o.thread_id = p.thread_id AND o.author_id != p.person_id
               AND o.seq <= p.delivered_seq)
         LEFT JOIN sessions cs ON cs.id = p.claimed_by
         LEFT JOIN tokens ct ON ct.id = cs.token_id
         WHERE p.thread_id = ?1 AND p.person_id IS NOT {SYSTEM_PERSON_SQL} ORDER BY p.rowid"
    ))?;
    let rows = stmt.query_map([thread], |r| {
        let session = |label: Option<String>, context: Option<String>| {
            label.map(|label| SessionOut { label, context })
        };
        Ok(Delivery {
            handle: r.get(0)?,
            named: r.get(13)?,
            note: r.get(1)?,
            delivered_at: r.get::<_, Option<i64>>(2)?.map(rfc3339),
            by: session(r.get(3)?, r.get(4)?),
            claimed_by: match (r.get(5)?, r.get::<_, Option<i64>>(7)?) {
                (Some(label), Some(at)) => Some(ClaimOut {
                    label,
                    context: r.get(6)?,
                    since: rfc3339(at),
                }),
                _ => None,
            },
            seen_by: r.get(8)?,
            handled_at: match r.get::<_, Option<bool>>(9)? {
                Some(true) => r.get::<_, Option<i64>>(10)?.map(rfc3339),
                _ => None,
            },
            handled_by: match r.get::<_, Option<bool>>(9)? {
                Some(true) => session(r.get(11)?, r.get(12)?),
                _ => None,
            },
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
    let (team, teams) = as_session(&app, &caller, true, |tx, _, now| {
        Ok((team(tx, now)?, teams(tx)?))
    })?;
    Ok(Json(
        json!({ "team": team, "teams": teams, "you": caller.handle }),
    ))
}

/// `DELETE /api/session`: the client ends its session. It is forgotten at once, with its claims.
async fn end_session(State(app): State<Arc<App>>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let ended = {
        let mut conn = app.db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = app.clock.now();
        let ended = end(&tx, &app, &caller, now)?;
        tx.commit()?;
        ended
    };
    tracing::info!(person = %caller.handle, ended, "session ended");
    Ok(Json(json!({ "ok": true })))
}

async fn inbox(
    State(app): State<Arc<App>>,
    caller: Caller,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let (all, wait) = inbox_params(query)?;
    if wait > 0 {
        let Some(_slot) = app.wait_slot(&caller.token_id) else {
            // Past its parameter checks, a refused request still counts as seen.
            as_session(&app, &caller, true, |_, _, _| Ok(()))?;
            return Err(ApiError::rate_limited(&format!(
                "at most {MAX_WAITS_PER_TOKEN} waiting inbox requests per token; wait for one to answer"
            )));
        };
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
            // Shown, not delivered: what a waiting thread waits on, even when another session of
            // the person already has it.
            out.last_note = note_at(tx, &row.id, row.last_seq, Some(INBOX_TEXT_CHARS))?;
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
        let teams = teams(tx)?;
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
            "teams": teams,
            // `you`: which member of `team` this session speaks for.
            "session": {
                "label": session.label,
                "context": session.context,
                "you": caller.handle,
            },
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
        kind = %result.told.kind,
        reached = result.told.reached,
        "send"
    );
    Ok(Json(result))
}

/// The whole of `send`, in one transaction so that idempotency and the limits cannot race.
/// Returns the result and the people to wake: the other participants, when a note was written,
/// and the writer's person, when a `seen` released this session's claim.
fn write(
    app: &App,
    caller: &Caller,
    mut req: SendRequest,
) -> Result<(SendResult, Vec<i64>), ApiError> {
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
    if req.to.is_some() {
        if req.thread.is_some() {
            return Err(ApiError::bad_request(
                "give exactly one of 'to' and 'thread' ('to' is deprecated: write who the note \
                 is for in its text, with @handle)",
            ));
        }
        if req.close || req.claim || req.seen || req.done {
            return Err(ApiError::bad_request(
                "'to' opens a thread: 'claim' and 'seen' go with 'thread' only",
            ));
        }
    }
    if req.seen && req.done {
        return Err(ApiError::bad_request(
            "give 'seen' or 'done', not both ('done' is the deprecated name of 'seen')",
        ));
    }
    if req.done && (req.text.is_some() || req.claim) {
        return Err(ApiError::bad_request(
            "done goes with 'thread' alone: no 'text' and no 'claim' ('done' is the deprecated \
             name of 'seen')",
        ));
    }
    // The deprecated `close`: threads no longer close. With `text` the note is written, and this
    // session lets go of the thread as after `seen` (see `write_as`); without, it is read as `seen`. A 0.4 client's close never fails:
    // with `claim` too, the claim is dropped, as a close without text never claimed.
    if req.close && req.text.is_none() && !req.seen && !req.done {
        req.claim = false;
        req.seen = true;
    }
    req.seen |= req.done;
    if req.seen && (req.text.is_some() || req.claim) {
        return Err(ApiError::bad_request(
            "seen goes with 'thread' alone: no 'text' and no 'claim'",
        ));
    }
    let marks = req.claim || req.seen;
    if req.thread.is_none() && marks {
        return Err(ApiError::bad_request(
            "'claim' and 'seen' go with 'thread' only: opening a thread claims nothing",
        ));
    }
    if req.text.is_none() && !(req.thread.is_some() && marks) {
        return Err(ApiError::bad_request(
            "text is required (it may be left out only to claim a thread or mark it seen)",
        ));
    }
    // Who it is for, as far as that can be checked without the database: an unknown name is
    // past these checks.
    let whom = match (&req.to, &req.text) {
        (Some(to), _) => Whom {
            mentions: recipients(to, &caller.handle)?,
            people_only: true,
        },
        // A reply from a client before 0.4 (no `Relevo-Mentions: 1`), which cannot know that an
        // `@` now names people, is written as 0.3 wrote it: its mentions are not read. Through
        // 0.x (docs/protocol.md, `send`).
        (None, Some(_)) if req.thread.is_some() && !caller.reads_mentions => Whom {
            mentions: Vec::new(),
            people_only: false,
        },
        (None, Some(text)) => {
            let mentions = mentions(text, &caller.handle).map_err(ApiError::bad_request)?;
            // A new thread that mentions only its writer goes nowhere, not to everyone: only a
            // note with no mention at all is for the global board.
            if req.thread.is_none() && mentions.is_empty() && mentions_only(text, &caller.handle) {
                return Err(ApiError::only_you());
            }
            Whom {
                mentions,
                people_only: false,
            }
        }
        (None, None) => Whom {
            mentions: Vec::new(),
            people_only: false,
        },
    };

    // Only a request past its own parameter checks counts as seen.
    as_session(app, caller, true, |tx, session, now| {
        write_as(tx, app, caller, session, &req, &whom, now)
    })
}

/// Whom a `send` names: the mentions of its text, or, from the deprecated `to`, its addresses
/// (people only; none for `@team`, which opens a `global` thread).
struct Whom {
    mentions: Vec<Mention>,
    people_only: bool,
}

/// `send`'s writes, as `session`.
fn write_as(
    tx: &Connection,
    app: &App,
    caller: &Caller,
    session: &Session,
    req: &SendRequest,
    whom: &Whom,
    now: i64,
) -> Result<(SendResult, Vec<i64>), ApiError> {
    // Keys expire 24 hours after the send that used them. Forgetting them here keeps the table
    // small, and lets an expired key be used again as a fresh send.
    tx.execute(
        "DELETE FROM send_keys WHERE created_at <= ?1",
        [now - KEY_LIFETIME_SECS],
    )?;
    if let Some(key) = &req.key
        && let Some(first) = replay(tx, caller, session, key)?
    {
        return Ok((first, Vec::new()));
    }

    // `caught_up`: every note of the thread had reached this session before it wrote, the
    // person's own from other sessions included. `to`: whom this note names.
    let (thread_id, caught_up, mut to, reply_names) = match &req.thread {
        None => {
            let text = req.text.as_deref().unwrap_or_default();
            let (id, to) = open_thread(tx, app, caller, whom, text, now)?;
            (id, true, to, Vec::new())
        }
        Some(id) => {
            let row = find_thread(tx, session, id)?;
            let names = reply_names(tx, &whom.mentions, now)?;
            if req.text.is_some() {
                // The server's own notes (release notes) count against no limit.
                let recent: i64 = tx.query_row(
                    &format!(
                        "SELECT count(*) FROM notes WHERE thread_id = ?1 AND created_at > ?2
                         AND author_id IS NOT {SYSTEM_PERSON_SQL}"
                    ),
                    params![row.id, now - 3600],
                    |r| r.get(0),
                )?;
                if recent >= THREAD_NOTES_PER_HOUR {
                    return Err(ApiError::thread_busy());
                }
            }
            let caught_up = row.delivered_seq >= row.last_seq;
            let to = Named {
                people: names.iter().map(|(_, h, _)| h.clone()).collect(),
                teams: Vec::new(),
            };
            (row.id, caught_up, to, names)
        }
    };

    let mut added = Vec::new();
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
            // The context it was written in, for the timeline (none for a general session).
            if let Some(context) = &session.context {
                tx.execute(
                    "INSERT INTO note_contexts (seq, context) VALUES (?1, ?2)",
                    params![seq, context],
                )?;
            }
            // The people this note names, from this note on; those not in the thread join it.
            added = name(tx, &thread_id, seq, &reply_names)?;
            tx.execute(
                "UPDATE participants SET last_other_seq = ?2 WHERE thread_id = ?1 AND person_id != ?3",
                params![thread_id, seq, caller.person_id],
            )?;
            // The writing session has the thread up to its note, unless earlier notes never
            // reached it: then nothing moves, so those notes are not skipped.
            if caught_up {
                deliver(tx, session, &thread_id, seq, now)?;
            }
            Some(id)
        }
        None => None,
    };
    // Writing in the thread or `seen` handles it for the writer's person, up to the latest note
    // that reached this session. Claiming does not: if the claiming session dies, the note is
    // still pending.
    if note_id.is_some() || req.seen {
        handle(tx, caller, session, &thread_id, now)?;
    }
    // Writing in an existing thread claims it for this session, and so does `claim` without a
    // note. Opening a thread claims nothing: the asker has nothing to take on, and the answers
    // must reach the person's next session too.
    if req.thread.is_some() && (note_id.is_some() || req.claim) {
        take_claim(tx, caller, session, &thread_id, now)?;
    }
    // `seen`: this session has nothing more to do there, so it lets go of the thread if it holds
    // it (`refresh` copies that to the sessions' rows). So does a 0.4 client's "reply and close"
    // (`close` with `text`), once the note is written. The person's other sessions may see it
    // now, so they are woken. Another session's claim stays.
    let released = (req.seen || (req.close && note_id.is_some() && req.thread.is_some()))
        && tx.execute(
            "UPDATE participants SET claimed_by = NULL, claimed_context = NULL, claimed_at = NULL
             WHERE thread_id = ?1 AND person_id = ?2 AND claimed_by = ?3",
            params![thread_id, caller.person_id, session.id],
        )? > 0;
    refresh(tx, &thread_id)?;
    let claimed_by: Option<i64> = tx.query_row(
        "SELECT claimed_by FROM participants WHERE thread_id = ?1 AND person_id = ?2",
        params![thread_id, caller.person_id],
        |r| r.get(0),
    )?;
    let (kind, reached) = reach(tx, &thread_id)?;
    to.people.dedup();
    let result = SendResult {
        thread: thread_id,
        note: note_id,
        status: "open",
        claimed: claim(claimed_by, session),
        told: Told {
            kind,
            to,
            added,
            reached,
        },
    };
    if let Some(key) = &req.key {
        let told = serde_json::to_string(&result.told).map_err(|_| ApiError::internal())?;
        tx.execute(
            "INSERT INTO send_keys (person_id, key, thread_id, note_id, status, claimed_by,
                                    created_at, result)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                caller.person_id,
                key,
                result.thread,
                result.note,
                result.status,
                claimed_by,
                now,
                told
            ],
        )?;
    }
    let mut woken: Vec<i64> = match result.note {
        Some(_) => tx
            .prepare("SELECT person_id FROM participants WHERE thread_id = ?1 AND person_id != ?2")?
            .query_map(params![result.thread, caller.person_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        None => Vec::new(),
    };
    if released {
        woken.push(caller.person_id);
    }
    Ok((result, woken))
}

/// The person has handled the thread up to the latest note that reached this session (after
/// `deliver` ran for a note it wrote caught up, so that note counts): a note that landed after
/// the session last looked was never seen there, so writing or `seen` cannot handle it. When
/// that covers a note by someone else they had not handled, it records when and from which
/// session (its context now).
fn handle(
    tx: &Connection,
    caller: &Caller,
    session: &Session,
    thread_id: &str,
    now: i64,
) -> Result<(), ApiError> {
    let reached: i64 = tx.query_row(
        "SELECT coalesce(s.delivered_seq, p.delivered_seq, 0)
         FROM participants p
         LEFT JOIN session_threads s ON s.session_id = ?3 AND s.thread_id = p.thread_id
         WHERE p.thread_id = ?1 AND p.person_id = ?2",
        params![thread_id, caller.person_id, session.id],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE participants SET
             handled_at = CASE WHEN covers THEN ?3 ELSE handled_at END,
             handled_label = CASE WHEN covers THEN ?4 ELSE handled_label END,
             handled_context = CASE WHEN covers THEN ?5 ELSE handled_context END,
             handled_seq = nullif(max(coalesce(handled_seq, 0), ?6), 0)
         FROM (SELECT EXISTS (
                   SELECT 1 FROM notes n, participants q
                   WHERE q.thread_id = ?1 AND q.person_id = ?2
                     AND n.thread_id = ?1 AND n.author_id != ?2
                     AND n.seq > coalesce(q.handled_seq, 0) AND n.seq <= ?6) AS covers)
         WHERE thread_id = ?1 AND person_id = ?2",
        params![
            thread_id,
            caller.person_id,
            now,
            session.label,
            session.context,
            reached
        ],
    )?;
    Ok(())
}

/// Claims the thread for this session, unless another session of the person holds it: a claim is
/// never taken from a session that still has it. (A lapsed claim was already ended when this
/// request was seen.) Claiming again is not a new claim: `claimed_context` and `claimed_at` stay
/// the context it claimed in and when.
fn take_claim(
    tx: &Connection,
    caller: &Caller,
    session: &Session,
    thread_id: &str,
    now: i64,
) -> Result<(), ApiError> {
    let claimed = tx.execute(
        "UPDATE participants SET claimed_by = ?3,
                claimed_context = CASE WHEN claimed_by IS NULL THEN ?4 ELSE claimed_context END,
                claimed_at = CASE WHEN claimed_by IS NULL THEN ?5 ELSE claimed_at END
         WHERE thread_id = ?1 AND person_id = ?2
           AND (claimed_by IS NULL OR claimed_by = ?3)",
        params![
            thread_id,
            caller.person_id,
            session.id,
            session.context,
            now
        ],
    )?;
    if claimed > 0 {
        tx.execute("UPDATE sessions SET claims = 1 WHERE id = ?1", [session.id])?;
    }
    Ok(())
}

/// A thread's kind, and how many people other than the writer take part in it.
fn reach(tx: &Connection, thread: &str) -> Result<(String, i64), ApiError> {
    Ok(tx.query_row(
        &format!(
            "SELECT kind, (SELECT count(*) - 1 FROM participants
                           WHERE thread_id = ?1 AND person_id IS NOT {SYSTEM_PERSON_SQL})
             FROM threads WHERE id = ?1"
        ),
        [thread],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// The first result of a `send` with this key, if it is still kept. It keeps who held the thread
/// after that send, named for the session that asks: a retry from another session is not told it
/// holds a thread it does not. A key used before layout 7 kept no more than that: the rest is
/// told as the thread is now.
fn replay(
    tx: &Connection,
    caller: &Caller,
    session: &Session,
    key: &str,
) -> Result<Option<SendResult>, ApiError> {
    let first = tx
        .query_row(
            "SELECT thread_id, note_id, claimed_by, result FROM send_keys
             WHERE person_id = ?1 AND key = ?2",
            params![caller.person_id, key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    // Its `status` is not read: a send that closed a thread under 0.4.x kept "closed", and every
    // thread is open now.
    let Some((thread, note, claimed_by, told)) = first else {
        return Ok(None);
    };
    let told = match told.and_then(|t| serde_json::from_str(&t).ok()) {
        Some(told) => told,
        None => {
            let (kind, reached) = reach(tx, &thread)?;
            Told {
                kind,
                to: Named::default(),
                added: Vec::new(),
                reached,
            }
        }
    };
    Ok(Some(SendResult {
        thread,
        note,
        status: "open",
        claimed: claim(claimed_by, session),
        told,
    }))
}

/// Who mentions name: people (id, handle, context), in the order mentioned, and teams (id, name),
/// once each.
struct Resolved {
    people: Vec<(i64, String, Option<String>)>,
    teams: Vec<(i64, String)>,
}

/// Finds whom the mentions name: an active person's handle or a team's name (`team` and
/// `equipo` are neither). Names that are none of these fail with `UNKNOWN_PERSON`, all of them at
/// once; a context on a team, with `BAD_REQUEST`. With `people_only` (the deprecated `to`), a
/// team's name is unknown.
fn resolve(
    conn: &Connection,
    mentions: &[Mention],
    people_only: bool,
    now: i64,
) -> Result<Resolved, ApiError> {
    let mut out = Resolved {
        people: Vec::new(),
        teams: Vec::new(),
    };
    let mut unknown = Vec::new();
    let mut refused = None;
    for m in mentions {
        let person: Option<i64> = conn
            .prepare_cached("SELECT id FROM people WHERE handle = ?1 AND active = 1")?
            .query_row([&m.name], |r| r.get(0))
            .optional()?;
        if let Some(id) = person {
            out.people.push((id, m.name.clone(), m.context.clone()));
            continue;
        }
        let team: Option<i64> = if people_only {
            None
        } else {
            conn.prepare_cached("SELECT id FROM teams WHERE name = ?1")?
                .query_row([&m.name], |r| r.get(0))
                .optional()?
        };
        let Some(id) = team else {
            unknown.push(m.name.clone());
            continue;
        };
        if let Some(context) = &m.context {
            refused.get_or_insert(ApiError::bad_request(format!(
                "'@{}/{context}': @{} is a team, and a team takes no context",
                m.name, m.name
            )));
        }
        if !out.teams.iter().any(|(seen, _)| *seen == id) {
            out.teams.push((id, m.name.clone()));
        }
    }
    if !unknown.is_empty() {
        let valid = serde_json::to_value(team(conn, now)?).unwrap_or_default();
        let teams = serde_json::to_value(teams(conn)?).unwrap_or_default();
        return Err(ApiError::unknown_person(&unknown, valid, teams));
    }
    match refused {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// The people a reply's mentions name. A team cannot be named there: a thread's teams are those
/// of the note that opened it.
fn reply_names(
    conn: &Connection,
    mentions: &[Mention],
    now: i64,
) -> Result<Vec<(i64, String, Option<String>)>, ApiError> {
    let resolved = resolve(conn, mentions, false, now)?;
    if let Some((_, name)) = resolved.teams.first() {
        return Err(ApiError::bad_request(format!(
            "@{name} is a team, and a team cannot be mentioned in a reply: open a new thread for \
             it, or name its people (@handle)"
        )));
    }
    Ok(resolved.people)
}

/// Names these people in the thread from note `seq` on: a participant not named yet becomes
/// named; someone not in it joins it, named, addressed to the context of their mention. (The
/// context of a mention of someone already in the thread is ignored.) Returns those who joined.
fn name(
    conn: &Connection,
    thread: &str,
    seq: i64,
    people: &[(i64, String, Option<String>)],
) -> Result<Vec<String>, ApiError> {
    let mut added = Vec::new();
    for (person, handle, context) in people {
        let named: Option<Option<i64>> = conn
            .query_row(
                "SELECT named_seq FROM participants WHERE thread_id = ?1 AND person_id = ?2",
                params![thread, person],
                |r| r.get(0),
            )
            .optional()?;
        match named {
            Some(Some(_)) => {}
            Some(None) => {
                conn.execute(
                    "UPDATE participants SET named_seq = ?3 WHERE thread_id = ?1 AND person_id = ?2",
                    params![thread, person, seq],
                )?;
            }
            None => {
                conn.execute(
                    "INSERT INTO participants (thread_id, person_id, context, named_seq)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![thread, person, context, seq],
                )?;
                added.push(handle.clone());
            }
        }
    }
    Ok(added)
}

/// Opens a thread for whom the mentions (or `to`) name: `direct` for people only, `team` with a
/// team, `global` with nobody. Its participants: the writer first, then the people named, then
/// the other active members of the teams named (or, in a `global` thread, every other active
/// person), by handle. Returns its id and whom it named.
fn open_thread(
    conn: &Connection,
    app: &App,
    caller: &Caller,
    whom: &Whom,
    text: &str,
    now: i64,
) -> Result<(String, Named), ApiError> {
    let r = resolve(conn, &whom.mentions, whom.people_only, now)?;
    let kind = match (r.teams.is_empty(), r.people.is_empty()) {
        (false, _) => "team",
        (true, false) => "direct",
        (true, true) => "global",
    };
    // The others: (person, the context the thread is addressed to for them, named).
    let mut others: Vec<(i64, Option<String>, bool)> = r
        .people
        .iter()
        .map(|(id, _, context)| (*id, context.clone(), true))
        .collect();
    let reached: Vec<i64> = match kind {
        "team" => {
            let mut stmt = conn.prepare_cached(
                "SELECT p.id FROM team_members m JOIN people p ON p.id = m.person_id
                 WHERE m.team_id = ?1 AND p.active = 1 AND p.id != ?2 ORDER BY p.handle",
            )?;
            let mut ids = Vec::new();
            for (team, _) in &r.teams {
                for id in stmt.query_map(params![team, caller.person_id], |r| r.get(0))? {
                    ids.push(id?);
                }
            }
            ids
        }
        "global" => conn
            .prepare("SELECT id FROM people WHERE active = 1 AND id != ?1 ORDER BY handle")?
            .query_map([caller.person_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        _ => Vec::new(),
    };
    for id in reached {
        if !others.iter().any(|(o, _, _)| *o == id) {
            others.push((id, None, false));
        }
    }
    if others.is_empty() {
        return Err(ApiError::no_recipients());
    }
    if kind != "direct" {
        let opened: i64 = conn.query_row(
            "SELECT count(*) FROM threads
             WHERE opener_id = ?1 AND kind IN ('team', 'global') AND created_at > ?2",
            params![caller.person_id, now - 24 * 3600],
            |r| r.get(0),
        )?;
        if opened >= i64::from(app.team_questions_per_day) {
            return Err(ApiError::team_limit(app.team_questions_per_day));
        }
    }

    let id = random_id("r_");
    conn.execute(
        "INSERT INTO threads (id, kind, title, opener_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'open', ?5)",
        params![id, kind, title_of(text), caller.person_id, now],
    )?;
    let mut add = conn.prepare(
        "INSERT INTO participants (thread_id, person_id, context, named_seq)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    // Every participant of a `direct` thread is named, its opener too: the others answer to them.
    let opener_named = (kind == "direct").then_some(0);
    add.execute(params![id, caller.person_id, None::<String>, opener_named])?;
    for (person, context, named) in others {
        add.execute(params![id, person, context, named.then_some(0)])?;
    }
    for (_, name) in &r.teams {
        conn.execute(
            "INSERT INTO thread_teams (thread_id, name) VALUES (?1, ?2)",
            params![id, name],
        )?;
    }
    let to = Named {
        people: r.people.into_iter().map(|(_, h, _)| h).collect(),
        teams: r.teams.into_iter().map(|(_, n)| n).collect(),
    };
    Ok((id, to))
}

/// The deprecated `to`, checked as far as it can be without the database, as 0.3 did: `@team`
/// alone and without a context (none named: a `global` thread), or 1 to 20 people other than
/// the writer (`me`).
fn recipients(to: &[String], me: &str) -> Result<Vec<Mention>, ApiError> {
    let addresses = addresses(to)?;
    if addresses.iter().any(|(h, _)| h == "team") {
        if addresses.len() != 1 {
            return Err(ApiError::bad_request(
                "'@team' must be the only entry of 'to'",
            ));
        }
        if addresses[0].1.is_some() {
            return Err(ApiError::bad_request("'@team' takes no context"));
        }
        return Ok(Vec::new());
    }
    if addresses.is_empty() || addresses.len() > MAX_RECIPIENTS {
        return Err(ApiError::bad_request(
            "'to' needs 1 to 20 handles, or [\"@team\"]",
        ));
    }
    if addresses.iter().any(|(h, _)| h == me) {
        return Err(ApiError::bad_request("you cannot send a note to yourself"));
    }
    Ok(addresses
        .into_iter()
        .map(|(name, context)| Mention { name, context })
        .collect())
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
    use super::{HELD_BY_SQL, NOTE_AT_SQL, NOTE_COLUMNS, count_sql, page_sqls, title_of};
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
                        let bounded = if step.contains("session_count") {
                            step.contains("session_id=? AND claimed_by")
                        } else {
                            step.contains("person_id=? AND claimed_by")
                        };
                        assert!(bounded, "{plan:#?}");
                    }
                }
                assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:#?}");
            }
        }
    }

    /// `last_note` costs one primary-key lookup (and its author's) per thread returned, and one
    /// of the server's own person in `meta`.
    #[test]
    fn last_note_is_read_by_its_key() {
        let plan = plan(&format!("{NOTE_COLUMNS} {NOTE_AT_SQL}"));
        assert_eq!(plan.len(), 4, "{plan:#?}");
        assert!(
            plan[2..]
                .iter()
                .any(|s| s.starts_with("SEARCH meta USING INDEX")),
            "{plan:#?}"
        );
        assert!(
            plan[0].starts_with("SEARCH n USING INTEGER PRIMARY KEY"),
            "{plan:#?}"
        );
        assert!(
            plan[1].starts_with("SEARCH a USING INTEGER PRIMARY KEY"),
            "{plan:#?}"
        );
    }

    /// `held_by` reads one thread's participant rows by their key, and each holder's session,
    /// token and person by theirs: never the claims of other threads.
    #[test]
    fn held_by_reads_the_threads_rows_by_key() {
        let plan = plan(HELD_BY_SQL);
        assert!(
            plan[0]
                .starts_with("SEARCH p USING INDEX sqlite_autoindex_participants_1 (thread_id=?"),
            "{plan:#?}"
        );
        for table in ["cs", "tk", "pe"] {
            assert!(
                plan.iter()
                    .any(|s| s.starts_with(&format!("SEARCH {table} USING"))
                        && (s.contains("PRIMARY KEY") || s.contains("autoindex"))),
                "{table}: {plan:#?}"
            );
        }
        assert!(!plan.iter().any(|s| s.starts_with("SCAN")), "{plan:#?}");
    }

    #[test]
    fn titles() {
        assert_eq!(title_of("\n# Refunds endpoint\nmore"), "Refunds endpoint");
        assert_eq!(title_of(&"é".repeat(100)).chars().count(), 80);
    }
}
