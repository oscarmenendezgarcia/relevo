//! Sessions: one running agent of a person. Every request counts as one, named by its token and
//! the `Relevo-Session` header (none: the token's default session).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::App;
use crate::auth::Caller;
use crate::db::{SESSION_LIFETIME_SECS, forget_sessions};
use crate::error::ApiError;

/// Sessions one token keeps. A client that picks a new `Relevo-Session` for every request must
/// not grow the team list, or the delivery rows every read writes for each session, without bound.
pub const MAX_SESSIONS_PER_TOKEN: i64 = 20;

/// The session a request counts as.
pub struct Session {
    pub id: i64,
    pub person_id: i64,
    pub label: String,
    /// `None` for a general session.
    pub context: Option<String>,
}

/// A session as the protocol shows it: its token's label and its context.
#[derive(Serialize)]
pub struct SessionOut {
    pub label: String,
    pub context: Option<String>,
}

impl Session {
    pub fn out(&self) -> SessionOut {
        SessionOut {
            label: self.label.clone(),
            context: self.context.clone(),
        }
    }
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

/// Records that the caller's session was seen now and returns it. First forgets sessions not
/// seen for 7 days, then those of the token beyond `MAX_SESSIONS_PER_TOKEN`.
///
/// With `arrived`, the request has just arrived and records the context it sent (the latest one
/// wins). Otherwise it is a waiting request answering, whose context may be older than one the
/// session sent since: it keeps the session's context.
pub fn touch(
    conn: &Connection,
    app: &App,
    caller: &Caller,
    now: i64,
    arrived: bool,
) -> Result<Session, ApiError> {
    check_token(conn, caller)?;
    forget(conn, app, now)?;
    let (id, context): (i64, Option<String>) = conn.query_row(
        "INSERT INTO sessions (token_id, client_id, person_id, context, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (token_id, client_id)
         DO UPDATE SET context = CASE WHEN ?6 THEN excluded.context ELSE context END,
                       last_seen = excluded.last_seen
         RETURNING id, context",
        params![
            caller.token_id,
            caller.session,
            caller.person_id,
            caller.context,
            now,
            arrived
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
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
        label: caller.label.clone(),
        context,
    })
}

/// The caller's session as it is recorded, without recording anything: for a waiting request
/// looking again. `None` when it was forgotten meanwhile.
pub fn find(conn: &Connection, caller: &Caller) -> Result<Option<Session>, ApiError> {
    check_token(conn, caller)?;
    Ok(conn
        .query_row(
            "SELECT id, context FROM sessions WHERE token_id = ?1 AND client_id = ?2",
            params![caller.token_id, caller.session],
            |r| {
                Ok(Session {
                    id: r.get(0)?,
                    person_id: caller.person_id,
                    label: caller.label.clone(),
                    context: r.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Forgets every session not seen for 7 days, and its claims: the threads it claimed go back to
/// every session of the person that sees them, so those sessions' waiting requests look again.
fn forget(conn: &Connection, app: &App, now: i64) -> Result<(), ApiError> {
    let expired: Vec<(i64, i64)> = conn
        .prepare("SELECT id, person_id FROM sessions WHERE last_seen <= ?1")?
        .query_map([now - SESSION_LIFETIME_SECS], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    app.announce(expired.iter().map(|(_, person)| *person));
    let ids: Vec<i64> = expired.into_iter().map(|(id, _)| id).collect();
    forget_sessions(conn, &ids)?;
    Ok(())
}
