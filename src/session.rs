//! Sessions: one running agent of a person. Every request counts as one, named by its token and
//! the `Relevo-Session` header (none: the token's default session).

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::App;
use crate::auth::Caller;
use crate::db::SESSION_LIFETIME_SECS;
use crate::error::ApiError;

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

/// Records that the caller's session was seen now, with the context it sent (the latest one
/// wins), and returns it. First forgets sessions not seen for 7 days.
pub fn touch(conn: &Connection, app: &App, caller: &Caller, now: i64) -> Result<Session, ApiError> {
    forget(conn, app, now)?;
    let id = conn.query_row(
        "INSERT INTO sessions (token_id, client_id, person_id, context, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (token_id, client_id)
         DO UPDATE SET context = excluded.context, last_seen = excluded.last_seen
         RETURNING id",
        params![
            caller.token_id,
            caller.session,
            caller.person_id,
            caller.context,
            now
        ],
        |r| r.get(0),
    )?;
    Ok(Session {
        id,
        person_id: caller.person_id,
        label: caller.label.clone(),
        context: caller.context.clone(),
    })
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
    for (id, _) in expired {
        conn.execute(
            "UPDATE participants SET claimed_by = NULL WHERE claimed_by = ?1",
            [id],
        )?;
        conn.execute(
            "UPDATE session_threads SET claimed_by = NULL WHERE claimed_by = ?1",
            [id],
        )?;
        conn.execute("DELETE FROM session_threads WHERE session_id = ?1", [id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    }
    Ok(())
}
