//! Bearer-token authentication: a request acts as the person who owns the token, in the
//! session named by the `Relevo-Session` and `Relevo-Context` headers.

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
}

const SESSION: HeaderName = HeaderName::from_static("relevo-session");
const CONTEXT: HeaderName = HeaderName::from_static("relevo-context");

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

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let secret = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().split_once(' '))
            // Auth schemes are case-insensitive (RFC 9110 section 11.1).
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, secret)| secret.trim())
            .ok_or_else(ApiError::unauthorized)?;
        let (person_id, handle, token_id, label) = app
            .db()
            .query_row(
                "SELECT p.id, p.handle, t.id, t.label
                 FROM tokens t JOIN people p ON p.id = t.person_id
                 WHERE t.hash = ?1 AND t.revoked_at IS NULL AND p.active = 1",
                [hash_token(secret)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(ApiError::unauthorized)?;
        // Checked after the token, so that a stranger learns nothing from a 400.
        let (session, context) = session_headers(&parts.headers)?;
        Ok(Caller {
            person_id,
            handle,
            token_id,
            label,
            session,
            context,
        })
    }
}
