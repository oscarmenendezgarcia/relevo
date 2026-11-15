//! Bearer-token authentication: a request acts as the person who owns the token, in the
//! session named by the `Relevo-Session` and `Relevo-Context` headers (and, through 0.x, with
//! `Relevo-Mentions` saying whether a reply's mentions are read). `Relevo-Client` says which
//! client the session runs, for the admin (`relevo clients`); `Relevo-Agent`, which agent it is,
//! for everyone (a note's `via`).

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, USER_AGENT};
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
    /// What the request says of its client, `<name>/<version>` (see `client`); `None` when it
    /// says nothing that can be read.
    pub client: Option<String>,
    /// Whether `client` was read from the User-Agent rather than said by `Relevo-Client`.
    pub client_inferred: bool,
    /// `Relevo-Agent`: the agent's name, as teammates see it ("Claude Code", "pi"); `None` when
    /// absent or not one (see `agent`).
    pub agent: Option<String>,
}

const SESSION: HeaderName = HeaderName::from_static("relevo-session");
const CONTEXT: HeaderName = HeaderName::from_static("relevo-context");
const MENTIONS: HeaderName = HeaderName::from_static("relevo-mentions");
const CLIENT: HeaderName = HeaderName::from_static("relevo-client");
const AGENT: HeaderName = HeaderName::from_static("relevo-agent");

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

/// What the request says of its client, and whether that was inferred: `Relevo-Client` when it is
/// well-formed, else `mcp/<version>` from a `relevo/<version>` User-Agent (`relevo mcp` before
/// 0.5.0, and `relevo connect` and `update`, which say it so). Anything else says nothing: never
/// an error, so that other and older clients keep working.
fn client(headers: &HeaderMap) -> (Option<String>, bool) {
    let text = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(said) = text(CLIENT).filter(|c| valid_client(c)) {
        return (Some(said.to_string()), false);
    }
    let inferred = text(USER_AGENT)
        .and_then(|ua| ua.strip_prefix("relevo/"))
        .map(|version| format!("mcp/{version}"))
        .filter(|c| valid_client(c));
    let found = inferred.is_some();
    (inferred, found)
}

/// `^[a-z][a-z0-9-]{0,15}/[0-9A-Za-z.+-]{1,32}$`: a client's name and its version.
pub fn valid_client(client: &str) -> bool {
    let Some((name, version)) = client.split_once('/') else {
        return false;
    };
    name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.len() <= 16
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && (1..=32).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// `Relevo-Agent`, trimmed, when it is a name (`valid_agent`). Anything else says nothing, never
/// an error.
fn agent(headers: &HeaderMap) -> Option<String> {
    let agent = std::str::from_utf8(headers.get(AGENT)?.as_bytes())
        .ok()?
        .trim();
    valid_agent(agent).then(|| agent.to_string())
}

/// An agent's name: 1 to 40 characters, each a letter or digit (of any script), a space or one of
/// `.-_+()/`. Nothing else, so that it can be shown inside the text an agent trusts (quoted after
/// "via") without a quote, `@`, `<` or line break to end it early.
pub fn valid_agent(agent: &str) -> bool {
    (1..=40).contains(&agent.chars().count())
        && agent
            .chars()
            .all(|c| c.is_alphanumeric() || " .-_+()/".contains(c))
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
        let (client, client_inferred) = client(&parts.headers);
        let agent = agent(&parts.headers);
        Ok(Caller {
            person_id,
            handle,
            token_id,
            label,
            session,
            context,
            reads_mentions,
            client,
            client_inferred,
            agent,
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
