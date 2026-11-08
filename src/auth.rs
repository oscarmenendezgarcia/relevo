//! Bearer-token authentication: a request acts as the person who owns the token.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use rusqlite::OptionalExtension;

use crate::App;
use crate::db::hash_token;
use crate::error::ApiError;

/// The person (and agent token) behind a request.
pub struct Caller {
    pub person_id: i64,
    pub handle: String,
    pub token_id: String,
    pub label: String,
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
        app.db()
            .query_row(
                "SELECT p.id, p.handle, t.id, t.label
                 FROM tokens t JOIN people p ON p.id = t.person_id
                 WHERE t.hash = ?1 AND t.revoked_at IS NULL AND p.active = 1",
                [hash_token(secret)],
                |r| {
                    Ok(Caller {
                        person_id: r.get(0)?,
                        handle: r.get(1)?,
                        token_id: r.get(2)?,
                        label: r.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(ApiError::unauthorized)
    }
}
