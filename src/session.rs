//! Sessions: one running agent of a person. Every request counts as one, named by its token and
//! the `Relevo-Session` header (none: the token's default session).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::App;
use crate::auth::Caller;
use crate::db::{CLAIM_LAPSE_SECS, SESSION_LIFETIME_SECS, forget_sessions, release_claims};
use crate::error::ApiError;

/// Sessions one token keeps. A client that picks a new `Relevo-Session` for every request must
/// not grow the team list, or the delivery rows every read writes for each session, without bound.
pub const MAX_SESSIONS_PER_TOKEN: i64 = 20;

/// The session a request counts as.
pub struct Session {
    pub id: i64,
    pub person_id: i64,
    /// What teammates see it as (a note's `via`): the agent it said it is (`Relevo-Agent`), else
    /// its token's label.
    pub label: String,
    /// `None` for a general session.
    pub context: Option<String>,
}

/// A session as the protocol shows it: its label (see `Session`) and its context.
#[derive(Serialize)]
pub struct SessionOut {
    pub label: String,
    pub context: Option<String>,
}

/// The token is checked again whenever a request looks: a waiting `inbox` request looks and
/// answers long after it was authenticated, and must not answer for a token revoked (or a person
/// deactivated) since.
fn check_token(conn: &Connection, caller: &Caller) -> Result<(), ApiError> {
    let valid: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tokens t JOIN people p ON p.id = t.person_id
                        WHERE t.id = ?1 AND t.revoked_at IS NULL AND p.active = 1)",
        [&caller.token_id],
        |r| r.get(0),
    )?;
    if !valid {
        return Err(ApiError::unauthorized());
    }
    Ok(())
}

/// Records that the caller's session was seen now and returns it. First ends the claims of
/// sessions not seen for 15 minutes (this one's included: they lapsed before it came back),
/// forgets sessions not seen for 7 days, then those of the token beyond `MAX_SESSIONS_PER_TOKEN`.
///
/// With `arrived`, the request has just arrived and records the context, the client and the agent
/// it sent (the latest request wins, saying nothing included). Otherwise it is a waiting request
/// answering, whose context may be older than one the session sent since: it keeps the
/// session's.
pub fn touch(
    conn: &Connection,
    app: &App,
    caller: &Caller,
    now: i64,
    arrived: bool,
) -> Result<Session, ApiError> {
    check_token(conn, caller)?;
    lapse(conn, app, now)?;
    forget(conn, now)?;
    let known: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE token_id = ?1 AND client_id = ?2)",
        )?
        .query_row(params![caller.token_id, caller.session], |r| r.get(0))?;
    let (id, context, agent): (i64, Option<String>, Option<String>) = conn.query_row(
        "INSERT INTO sessions (token_id, client_id, person_id, context, last_seen, client,
                               client_inferred, agent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?7, ?8, ?9)
         ON CONFLICT (token_id, client_id)
         DO UPDATE SET context = CASE WHEN ?6 THEN excluded.context ELSE context END,
                       client = CASE WHEN ?6 THEN excluded.client ELSE client END,
                       client_inferred = CASE WHEN ?6 THEN excluded.client_inferred
                                         ELSE client_inferred END,
                       agent = CASE WHEN ?6 THEN excluded.agent ELSE agent END,
                       last_seen = excluded.last_seen
         RETURNING id, context, agent",
        params![
            caller.token_id,
            caller.session,
            caller.person_id,
            caller.context,
            now,
            arrived,
            caller.client,
            caller.client_inferred,
            caller.agent
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if !known {
        start(conn, id, caller.person_id)?;
    }
    // Beyond the cap, the token's sessions seen least recently are forgotten. This one is now
    // the most recent, so it stays.
    let evicted: Vec<i64> = conn
        .prepare_cached(
            "SELECT id FROM sessions WHERE token_id = ?1 AND id != ?2
             ORDER BY last_seen DESC, id DESC LIMIT -1 OFFSET ?3",
        )?
        .query_map(
            params![caller.token_id, id, MAX_SESSIONS_PER_TOKEN - 1],
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    if !evicted.is_empty() {
        app.announce([caller.person_id]);
        forget_sessions(conn, &evicted)?;
    }
    Ok(Session {
        id,
        person_id: caller.person_id,
        label: agent.unwrap_or_else(|| caller.label.clone()),
        context,
    })
}

/// A new session starts with what its person has (it has no row of its own: see
/// `session_threads`), except in the threads still pending for the person where one of their
/// sessions has a note they have not handled (`fresh`): there it starts with what the person
/// handled (or, named later, from the note that named them), so a note fetched by a session that
/// died before handling it is new for this one.
fn start(conn: &Connection, id: i64, person: i64) -> Result<(), ApiError> {
    conn.prepare_cached(START_SQL)?
        .execute(params![id, person])?;
    Ok(())
}

/// The rows a new session `?1` of person `?2` starts with, read from an index that holds only
/// fresh threads: it costs nothing for a person without any. It starts where the person's pending
/// notes do (`api::FLOOR`): after what they handled, and from the note that named them on. Only a
/// named participant is ever fresh, but the starting point is `max` of what may be `NULL`, and in
/// SQLite `max(a, NULL)` is `NULL`: the name is said here too, so that one row that should not
/// exist cannot leave a new session of this person started nowhere.
const START_SQL: &str = "
INSERT INTO session_threads (session_id, thread_id, person_id, delivered_seq, has_new, waiting,
                             last_seq, claimed_by, context, person_inbox)
SELECT ?1, thread_id, person_id, max(coalesce(handled_seq, 0), named_seq - 1), 1,
       waiting, last_seq, claimed_by, context, inbox
FROM participants INDEXED BY participants_fresh
WHERE person_id = ?2 AND fresh = 1 AND named_seq IS NOT NULL";

/// The caller's session as it is recorded, without recording anything: for a waiting request
/// looking again. `None` when it was forgotten meanwhile.
pub fn find(conn: &Connection, caller: &Caller) -> Result<Option<Session>, ApiError> {
    check_token(conn, caller)?;
    Ok(conn
        .query_row(
            "SELECT id, context, agent FROM sessions WHERE token_id = ?1 AND client_id = ?2",
            params![caller.token_id, caller.session],
            |r| {
                Ok(Session {
                    id: r.get(0)?,
                    person_id: caller.person_id,
                    label: r
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| caller.label.clone()),
                    context: r.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Forgets the caller's session, with its claims, when its client ends it (`DELETE
/// /api/session`). Its threads go back to every session of the person that sees them, so those
/// sessions' waiting requests look again. A session already forgotten (or never seen) is fine.
pub fn end(conn: &Connection, app: &App, caller: &Caller, now: i64) -> Result<bool, ApiError> {
    check_token(conn, caller)?;
    lapse(conn, app, now)?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM sessions WHERE token_id = ?1 AND client_id = ?2",
            params![caller.token_id, caller.session],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        app.announce([caller.person_id]);
        forget_sessions(conn, &[id])?;
    }
    Ok(id.is_some())
}

/// The sessions whose claims have lapsed (`?1`: 15 minutes ago), read from an index that holds
/// only sessions that may hold claims, so a request that finds none reads one index entry at most.
const LAPSED_SQL: &str = "SELECT id, person_id FROM sessions INDEXED BY sessions_claiming
                          WHERE claims = 1 AND last_seen <= ?1";

/// Ends the claims of every session not seen for 15 minutes: the threads it claimed go back to
/// every session of the person that sees them, so those sessions' waiting requests look again.
/// Every request runs this before it reads or writes a claim, so a lapsed claim is never seen as
/// held.
/// Each session's claims lapse once: the index holds only sessions that may still hold some.
pub fn lapse(conn: &Connection, app: &App, now: i64) -> Result<(), ApiError> {
    let lapsed: Vec<(i64, i64)> = conn
        .prepare_cached(LAPSED_SQL)?
        .query_map([now - CLAIM_LAPSE_SECS], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    app.announce(lapsed.iter().map(|(_, person)| *person));
    for (id, _) in lapsed {
        release_claims(conn, id)?;
    }
    Ok(())
}

/// When the next claim of the person's sessions lapses, if any of them holds one.
pub fn next_lapse(conn: &Connection, person: i64) -> Result<Option<i64>, ApiError> {
    Ok(conn
        .prepare_cached(
            "SELECT min(last_seen) + ?2 FROM sessions WHERE person_id = ?1 AND claims = 1",
        )?
        .query_row(params![person, CLAIM_LAPSE_SECS], |r| r.get(0))?)
}

/// Forgets every session not seen for 7 days. Their claims lapsed long before.
fn forget(conn: &Connection, now: i64) -> Result<(), ApiError> {
    let expired: Vec<i64> = conn
        .prepare("SELECT id FROM sessions WHERE last_seen <= ?1")?
        .query_map([now - SESSION_LIFETIME_SECS], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    forget_sessions(conn, &expired)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LAPSED_SQL, START_SQL};

    /// A new session reads only its person's fresh threads, from their own index.
    #[test]
    fn a_new_session_reads_only_its_persons_fresh_threads() {
        let conn = crate::db::open(":memory:").unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {START_SQL}"))
            .unwrap()
            .query_map([1, 2], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            plan,
            vec!["SEARCH participants USING INDEX participants_fresh (person_id=?)"]
        );
    }

    /// Every request looks for lapsed claims, so that must cost nothing however many sessions,
    /// lapsed or not, the server keeps: a range of the index of sessions that may hold claims.
    #[test]
    fn looking_for_lapsed_claims_reads_one_index_range() {
        let conn = crate::db::open(":memory:").unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {LAPSED_SQL}"))
            .unwrap()
            .query_map([0], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            plan,
            vec!["SEARCH sessions USING INDEX sessions_claiming (last_seen<?)"]
        );
    }
}
