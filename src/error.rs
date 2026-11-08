use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// An error as the protocol shows it: `{ "error": { "code", "message", ...extra } }`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub extra: Option<(&'static str, Value)>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            extra: None,
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

    pub fn unknown_person(handle: &str, valid: Value) -> Self {
        ApiError {
            extra: Some(("valid", valid)),
            ..Self::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_PERSON",
                format!("no active person with handle '{handle}'; see 'valid'"),
            )
        }
    }

    pub fn no_recipients() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "NO_RECIPIENTS",
            "there is nobody else on the team to ask",
        )
    }

    pub fn too_large(status: StatusCode, message: impl Into<String>) -> Self {
        Self::new(status, "TOO_LARGE", message)
    }

    pub fn thread_closed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "THREAD_CLOSED",
            "this thread is closed; open a new thread instead",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "only the person who asked can close a team thread",
        )
    }

    pub fn team_limit(limit: u32) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "TEAM_LIMIT",
            format!("at most {limit} team questions in 24 hours; ask someone directly instead"),
        )
    }

    pub fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "at most 60 notes per minute; slow down",
        )
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
        if let Some((key, value)) = self.extra {
            error[key] = value;
        }
        (self.status, Json(json!({ "error": error }))).into_response()
    }
}
