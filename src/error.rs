use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::db::RESERVED_HANDLES;

/// An error as the protocol shows it: `{ "error": { "code", "message", ...extra } }`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Fields the error carries besides `code` and `message`.
    pub extra: Vec<(&'static str, Value)>,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            extra: Vec::new(),
        }
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "missing, unknown or revoked token, or inactive person",
        )
    }

    pub fn not_found(message: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }

    /// Names that are no active person nor team: `unknown`, with the people (`valid`, each with
    /// their `about`) and the teams (`teams`, with their members) that can be named.
    /// `@team` and `@equipo` are among them like any other name, with a word on naming the team.
    pub fn unknown_person(unknown: &[String], valid: Value, teams: Value) -> Self {
        let names: Vec<String> = unknown.iter().map(|n| format!("@{n}")).collect();
        let hint = match unknown
            .iter()
            .find(|n| RESERVED_HANDLES.contains(&n.as_str()))
        {
            None => String::new(),
            Some(reserved) => {
                let named: Vec<String> = teams
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t["name"].as_str())
                    .map(|n| format!("@{n}"))
                    .collect();
                if named.is_empty() {
                    format!(
                        ". @{reserved} is no team: a team is always named, and there are none; \
                         name the people (@handle)"
                    )
                } else {
                    format!(
                        ". @{reserved} is no team: a team is always named; name the team ({}) \
                         or the people (@handle)",
                        named.join(", ")
                    )
                }
            }
        };
        ApiError {
            extra: vec![
                ("valid", valid),
                ("teams", teams),
                ("unknown", Value::from(unknown.to_vec())),
            ],
            ..Self::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_PERSON",
                format!(
                    "no active person or team is named {}; see 'valid' for the people and \
                     'teams' for the teams (nothing was sent: a note never falls back to the \
                     global board){hint}",
                    names.join(", ")
                ),
            )
        }
    }

    pub fn no_recipients() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "NO_RECIPIENTS",
            "nobody but you would get this note: the teams it names have no other active \
             member, or nobody else is on the server; name someone with @handle",
        )
    }

    /// A new thread whose only mentions are its writer: not the global board, which takes only a
    /// note with no mention at all.
    pub fn only_you() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "NO_RECIPIENTS",
            "the only person this note mentions is you: name who it is for with @handle, or write \
             no @ at all to post it on the global board",
        )
    }

    pub fn too_large(status: StatusCode, message: impl Into<String>) -> Self {
        Self::new(status, "TOO_LARGE", message)
    }

    pub fn team_limit(limit: u32) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "TEAM_LIMIT",
            format!(
                "at most {limit} team and global-board threads in 24 hours: name the people the \
                 note is for with @handle, or wait"
            ),
        )
    }

    pub fn rate_limited(message: &str) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED", message)
    }

    pub fn thread_busy() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "THREAD_BUSY",
            "this thread got 20 notes in the last hour; check with your person before going on",
        )
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "internal error",
        )
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!(error = %e, "database error");
        ApiError::internal()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = json!({ "code": self.code, "message": self.message });
        for (key, value) in self.extra {
            error[key] = value;
        }
        (self.status, Json(json!({ "error": error }))).into_response()
    }
}
