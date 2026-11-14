//! Bearer-token authentication: a request acts as the person who owns the token, in the
//! session named by the `Relevo-Session` and `Relevo-Context` headers (and, through 0.x, with
//! `Relevo-Mentions` saying whether a reply's mentions are read).

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};
use rusqlite::OptionalExtension;

use crate::App;
use crate::db::{CONTEXT_FORMAT, hash_token, valid_context, valid_session_id};
use crate::error::ApiError;

/// The person (and agent token) behind a request.
pub struct Caller {
    pub person_id: i64,
    pub handle: String,
    pub token_id: String,
    pub label: String,
    /// The `Relevo-Session` id, or '' for the token's default session.
    pub session: String,
    /// The `Relevo-Context`, lowercased; `None` for a general session.
    pub context: Option<String>,
    /// `Relevo-Mentions: 1`: a 0.4 client, which knows that a reply's `@` names people. Through
    /// 0.x a reply without it is written as 0.3 wrote it, its mentions not read.
    pub reads_mentions: bool,
}

const SESSION: HeaderName = HeaderName::from_static("relevo-session");
const CONTEXT: HeaderName = HeaderName::from_static("relevo-context");
const MENTIONS: HeaderName = HeaderName::from_static("relevo-mentions");

/// A header as trimmed text; `None` when absent.
fn header(headers: &HeaderMap, name: &HeaderName) -> Result<Option<String>, ApiError> {
    headers
        .get(name)
        .map(|v| {
            v.to_str()
                .map(|s| s.trim().to_string())
                .map_err(|_| ApiError::bad_request(format!("{name} must be plain ASCII")))
        })
        .transpose()
}

/// The session id and context the request names, checked.
fn session_headers(headers: &HeaderMap) -> Result<(String, Option<String>), ApiError> {
    let session = header(headers, &SESSION)?.unwrap_or_default();
    if headers.contains_key(SESSION) && !valid_session_id(&session) {
        return Err(ApiError::bad_request(
            "Relevo-Session must match ^[A-Za-z0-9_-]{8,64}$",
        ));
    }
    let context = header(headers, &CONTEXT)?
        .map(|c| c.to_lowercase())
        .filter(|c| !c.is_empty());
    if let Some(context) = &context
        && !valid_context(context)
    {
        return Err(ApiError::bad_request(format!(
            "Relevo-Context must be {CONTEXT_FORMAT}"
        )));
    }
    Ok((session, context))
}

/// `Relevo-Mentions`: absent, or `1`.
fn mentions_header(headers: &HeaderMap) -> Result<bool, ApiError> {
    match header(headers, &MENTIONS)?.as_deref() {
        None => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(ApiError::bad_request("Relevo-Mentions must be 1")),
    }
}

/// The person and token behind the request's bearer token: (person id, handle, token id, label).
fn owner(parts: &Parts, app: &App) -> Result<(i64, String, String, String), ApiError> {
    let secret = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().split_once(' '))
        // Auth schemes are case-insensitive (RFC 9110 section 11.1).
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, secret)| secret.trim())
        .ok_or_else(ApiError::unauthorized)?;
    app.db()
        .query_row(
            "SELECT p.id, p.handle, t.id, t.label
             FROM tokens t JOIN people p ON p.id = t.person_id
             WHERE t.hash = ?1 AND t.revoked_at IS NULL AND p.active = 1",
            [hash_token(secret)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(ApiError::unauthorized)
}

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let (person_id, handle, token_id, label) = owner(parts, app)?;
        // Checked after the token, so that a stranger learns nothing from a 400.
        let (session, context) = session_headers(&parts.headers)?;
        let reads_mentions = mentions_header(&parts.headers)?;
        Ok(Caller {
            person_id,
            handle,
            token_id,
            label,
            session,
            context,
            reads_mentions,
        })
    }
}

/// The person behind a read-only timeline request (`crate::timeline`): the token alone. It is no
/// session's request, so the session headers are not read at all.
pub struct Reader {
    pub person_id: i64,
    pub handle: String,
}

impl FromRequestParts<Arc<App>> for Reader {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let (person_id, handle, _, _) = owner(parts, app)?;
        Ok(Reader { person_id, handle })
    }
}
