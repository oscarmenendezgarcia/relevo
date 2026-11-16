//! Invitations (docs/protocol.md, "Invitations"): the routes that need no token, keyed by an
//! invitation's code. `POST /api/join` trades the code for a token of its person, once.
//! `GET /join/<code>` is the page the link opens, `GET /join/<code>/install` the script that
//! installs relevo and connects it, and `GET /join/<code>/relevo?target=` the binary the script
//! downloads, from the dist: none of them uses the code up. An address that tries too many codes
//! that do not work is refused for a while, so that codes cannot be guessed. Codes and tokens are
//! never logged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::db::{self, Invite};
use crate::error::ApiError;
use crate::{App, dist, lock};

/// Codes that do not work one address may try in `FAILURE_WINDOW_SECS`; beyond, every invitation
/// route refuses it until the oldest of them is that old.
pub const MAX_FAILURES: usize = 10;
pub const FAILURE_WINDOW_SECS: i64 = 3600;

const STYLE: &str = include_str!("join/page.css");
const SCRIPT: &str = include_str!("join/page.js");
const INSTALL: &str = include_str!("join/install.sh");
const MARK: &str = include_str!("../assets/brand/relevo-mark.svg");

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/join", post(join))
        .route("/join/{code}", get(page))
        .route("/join/{code}/install", get(install))
        .route("/join/{code}/relevo", get(binary))
}

/// The client's address, as `serve` gives it; none through the router alone (tests).
type Peer = Option<Extension<ConnectInfo<SocketAddr>>>;

/// What failures count against: the client's IP address, or its /64 for IPv6 (one host may have
/// a whole one). Requests that came with no address count as one.
fn address(peer: &Peer) -> IpAddr {
    match peer
        .as_ref()
        .map(|Extension(ConnectInfo(a))| a.ip().to_canonical())
    {
        Some(IpAddr::V6(ip)) => {
            let mut octets = ip.octets();
            octets[8..].fill(0);
            IpAddr::from(octets)
        }
        Some(ip) => ip,
        None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    }
}

/// Whether `addr` tried `MAX_FAILURES` codes that do not work in the last `FAILURE_WINDOW_SECS`.
fn limited(app: &App, addr: IpAddr, now: i64) -> bool {
    lock(&app.join_failures).get(&addr).is_some_and(|times| {
        times
            .iter()
            .filter(|t| **t > now - FAILURE_WINDOW_SECS)
            .count()
            >= MAX_FAILURES
    })
}

/// `addr` tried a code that does not work.
fn failed(app: &App, addr: IpAddr, now: i64) {
    let mut all = lock(&app.join_failures);
    // What is older than the window counts for nothing: dropped, so the map stays small.
    if all.len() > 1000 {
        all.retain(|_, times| times.last().is_some_and(|t| *t > now - FAILURE_WINDOW_SECS));
    }
    let times = all.entry(addr).or_default();
    times.retain(|t| *t > now - FAILURE_WINDOW_SECS);
    times.push(now);
    if times.len() == MAX_FAILURES {
        tracing::warn!(address = %addr, "too many invitation codes that do not work: refused for a while");
    }
}

/// Why an invitation route does not go on.
enum Refusal {
    Limited,
    Not(Invite),
    Internal,
}

impl Refusal {
    fn status(&self) -> StatusCode {
        match self {
            Refusal::Limited => StatusCode::TOO_MANY_REQUESTS,
            Refusal::Not(Invite::Used(_) | Invite::Expired(_)) => StatusCode::GONE,
            Refusal::Not(_) => StatusCode::NOT_FOUND,
            Refusal::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Refusal::Limited => "RATE_LIMITED",
            Refusal::Not(Invite::Used(_)) => "INVITE_USED",
            Refusal::Not(Invite::Expired(_)) => "INVITE_EXPIRED",
            Refusal::Not(_) => "INVITE_UNKNOWN",
            Refusal::Internal => "INTERNAL",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Refusal::Limited => "Too many tries",
            Refusal::Not(Invite::Used(_)) => "This invitation was already used",
            Refusal::Not(Invite::Expired(_)) => "This invitation has expired",
            Refusal::Not(_) => "This invitation link does not work",
            Refusal::Internal => "Something went wrong",
        }
    }

    fn message(&self) -> String {
        match self {
            Refusal::Limited => "Too many invitation links that do not work came from your \
                                 address. Wait an hour, then try again."
                .to_string(),
            Refusal::Not(Invite::Used(handle) | Invite::Expired(handle)) => format!(
                "An invitation works once, for 24 hours. Ask for a new one: on the server, \
                 relevo invite {handle}."
            ),
            Refusal::Not(_) => "No invitation has this code: check that you have the whole \
                                link, or ask for a new one."
                .to_string(),
            Refusal::Internal => "The server could not read the invitation: try again in a \
                                  moment."
                .to_string(),
        }
    }

    fn api(&self) -> ApiError {
        ApiError::new(
            self.status(),
            self.code(),
            format!("{}. {}", self.title(), self.message()),
        )
    }
}

/// The invitation `code` names, when it works and the client may still try codes.
fn check(app: &App, peer: &Peer, code: &str) -> Result<Invite, Refusal> {
    let (addr, now) = (address(peer), app.clock.now());
    if limited(app, addr, now) {
        return Err(Refusal::Limited);
    }
    match db::find_invite(&app.db(), code, now) {
        Ok(found @ Invite::Valid { .. }) => Ok(found),
        Ok(other) => {
            failed(app, addr, now);
            Err(Refusal::Not(other))
        }
        Err(e) => {
            tracing::error!(error = %e, "reading an invitation");
            Err(Refusal::Internal)
        }
    }
}

#[derive(Deserialize)]
struct JoinRequest {
    code: String,
}

/// `POST /api/join` `{ "code" }`: the invitation used, and a new token of its person.
async fn join(
    State(app): State<Arc<App>>,
    peer: Peer,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let (addr, now) = (address(&peer), app.clock.now());
    if limited(&app, addr, now) {
        return Err(Refusal::Limited.api());
    }
    let body = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let req: JoinRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("the body must be {{ \"code\": … }}: {e}")))?;
    let redeemed = db::redeem_invite(&app.db(), &req.code, now).map_err(|e| {
        tracing::error!(error = %e, "using an invitation");
        ApiError::internal()
    })?;
    match redeemed {
        Ok(joined) => {
            tracing::info!(person = %joined.handle, token = %joined.token.id, "joined with an invitation");
            Ok(Json(json!({
                "token": joined.token.secret,
                "handle": joined.handle,
                "url": joined.url,
            })))
        }
        Err(state) => {
            failed(&app, addr, now);
            Err(Refusal::Not(state).api())
        }
    }
}

/// `s` as one word of sh: as it is when nothing in it means anything to sh, else quoted.
fn sh_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_./:@%+=,[]".contains(&c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The command that runs the install script of `code`, trusting the server by its key's pin when
/// it has one.
fn curl_install(url: &str, code: &str, pin: Option<&str>) -> String {
    let trust = pin
        .map(|pin| format!("--pinnedpubkey {} -k ", sh_word(&format!("sha256//{pin}"))))
        .unwrap_or_default();
    format!(
        "curl -sSf {trust}{}",
        sh_word(&format!("{url}/join/{code}/install"))
    )
}

/// The binaries the dist lists: each target and its SHA-256.
async fn binaries(app: &App) -> Vec<(String, String)> {
    let Some(dir) = &app.dist else {
        return vec![];
    };
    let Some(manifest) = dist::read_manifest(dir).await else {
        return vec![];
    };
    manifest
        .files
        .into_iter()
        .filter(|f| f.kind == "binary")
        .filter_map(|f| Some((f.target?, f.sha256)))
        .collect()
}

/// `GET /join/<code>/install`: the script, or one that says why there is none and fails.
async fn install(State(app): State<Arc<App>>, peer: Peer, Path(code): Path<String>) -> Response {
    let found = check(&app, &peer, &code);
    let public = db::public(&app.db()).unwrap_or_default();
    let (status, script) = match found {
        Ok(Invite::Valid { url, .. }) => {
            let sums: Vec<String> = binaries(&app)
                .await
                .into_iter()
                .map(|(target, sum)| format!("{target} {sum}"))
                .collect();
            let code = db::invite_code(&code).expect("a valid invitation's code");
            let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
            let script = INSTALL
                .replace("@URL@", &quote(&url))
                .replace("@CODE@", &quote(&code))
                .replace("@PIN@", &quote(public.pin.as_deref().unwrap_or("")))
                .replace(
                    "@FINGERPRINT@",
                    &quote(public.fingerprint.as_deref().unwrap_or("")),
                )
                .replace("@SUMS@", &quote(&sums.join("\n")));
            (StatusCode::OK, script)
        }
        Ok(_) => unreachable!("check answers only a valid invitation"),
        Err(refusal) => {
            let said = format!("{}. {}", refusal.title(), refusal.message());
            let script = format!(
                "#!/bin/sh\nprintf 'relevo: %s\\n' {} >&2\nexit 1\n",
                sh_word(&said)
            );
            (refusal.status(), script)
        }
    };
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        script,
    )
        .into_response()
}

#[derive(Deserialize)]
struct Target {
    target: String,
}

/// `GET /join/<code>/relevo?target=<triple>`: the dist's binary for that target, as is.
async fn binary(
    State(app): State<Arc<App>>,
    peer: Peer,
    Path(code): Path<String>,
    query: Result<Query<Target>, QueryRejection>,
) -> Result<Response, ApiError> {
    check(&app, &peer, &code).map_err(|refusal| refusal.api())?;
    let Query(Target { target }) =
        query.map_err(|_| ApiError::bad_request("say which binary: ?target=<target triple>"))?;
    let not_found = || ApiError::not_found("this server hands out no relevo for that target");
    let dir = app.dist.as_ref().ok_or_else(not_found)?;
    let manifest = dist::read_manifest(dir).await.ok_or_else(not_found)?;
    let file = manifest
        .files
        .iter()
        .find(|f| f.kind == "binary" && f.target.as_deref() == Some(target.as_str()))
        .ok_or_else(not_found)?;
    dist::send(dir, &file.name).await
}

/// `text` for HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The CSP source of an inline style or script.
fn csp_hash(text: &str) -> String {
    let sum = Sha256::digest(text.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(sum)
    )
}

/// A whole page, with `main` inside: nothing in it loads or runs but its own style and script.
fn html(status: StatusCode, title: &str, main: &str) -> Response {
    let csp = format!(
        "default-src 'none'; style-src {}; script-src {}; img-src data:; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'",
        csp_hash(STYLE),
        csp_hash(SCRIPT)
    );
    let page = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"referrer\" content=\"no-referrer\">\n<meta name=\"robots\" content=\"noindex\">\n\
         <title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<main>\n\
         <header>{MARK}<span>relevo</span></header>\n{main}</main>\n<script>{SCRIPT}</script>\n\
         </body>\n</html>\n",
        title = escape(title),
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
            (header::X_FRAME_OPTIONS, "DENY".to_string()),
            (
                header::HeaderName::from_static("x-robots-tag"),
                "noindex".to_string(),
            ),
        ],
        page,
    )
        .into_response()
}

/// "23 h", or "40 min" in the last hour.
fn expires_in(secs: i64) -> String {
    if secs >= 3600 {
        format!("{} h", (secs + 1800) / 3600)
    } else {
        format!("{} min", (secs / 60).max(1))
    }
}

/// `GET /join/<code>`: the invitation, or why it does not work.
async fn page(
    State(app): State<Arc<App>>,
    peer: Peer,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Response {
    let found = check(&app, &peer, &code);
    let (handle, name, url, expires_at) = match found {
        Ok(Invite::Valid {
            handle,
            name,
            url,
            expires_at,
        }) => (handle, name, url, expires_at),
        Ok(_) => unreachable!("check answers only a valid invitation"),
        Err(refusal) => {
            let main = format!(
                "<h1>{}</h1>\n<p class=\"lead\">{}</p>\n",
                escape(refusal.title()),
                escape(&refusal.message())
            );
            return html(refusal.status(), refusal.title(), &main);
        }
    };
    let pin = db::public(&app.db()).unwrap_or_default().pin;
    let targets = binaries(&app).await;
    let code = db::invite_code(&code).expect("a valid invitation's code");
    let link = format!("{url}/join/{code}");
    let command = curl_install(&url, &code, pin.as_deref());
    let mac = headers
        .get(header::USER_AGENT)
        .and_then(|ua| ua.to_str().ok())
        .is_some_and(|ua| ua.contains("Macintosh") || ua.contains("Mac OS X"));
    let has = |prefix: &str| targets.iter().any(|(t, _)| t.ends_with(prefix));
    let panel = |id: &str, on: bool, has: bool, system: &str, under: &str| {
        let body = if has {
            format!(
                "<div class=\"row\"><code id=\"cmd-{id}\">{} | sh</code>\
                 <button type=\"button\" data-copy=\"cmd-{id}\">Copy</button></div>\n\
                 <p class=\"under\">{under}</p>",
                escape(&command)
            )
        } else {
            format!(
                "<p class=\"under\">This server hands out no relevo for {system} yet: ask \
                 whoever invited you.</p>"
            )
        };
        format!(
            "<div id=\"panel-{id}\" role=\"tabpanel\"{}>\n{body}\n</div>\n",
            if on { "" } else { " hidden" }
        )
    };
    let tab = |id: &str, on: bool, label: &str| {
        format!(
            "<button type=\"button\" role=\"tab\" aria-controls=\"panel-{id}\" \
             aria-selected=\"{on}\">{label}</button>"
        )
    };
    let main = format!(
        "<h1>You're invited to Relevo</h1>
<p class=\"lead\">You'll join as <b>@{handle}</b> ({name}). Your agents will pass notes to your \
teammates' agents, and only what needs you reaches you.</p>
<section class=\"card\">
<h2>Easiest: give this link to your agent</h2>
<p>Paste it in Claude Code, pi or Hermes and say “join”. Your agent asks you before it runs \
anything.</p>
<div class=\"row\"><code id=\"link\">{link}</code><button type=\"button\" class=\"ghost\" \
data-copy=\"link\">Copy link</button></div>
</section>
<section class=\"card\">
<h2>Or paste this in a terminal</h2>
<div class=\"tabs\" role=\"tablist\">{mac_tab}{linux_tab}</div>
{mac_panel}{linux_panel}<details><summary>Rather not pipe to sh?</summary>
<p>Save the script instead: the same command with <code>-o relevo-join.sh</code> in place of \
<code>| sh</code>. Read it, then run <code>sh relevo-join.sh</code>. It installs relevo in \
<code>~/.local/bin</code> and connects it to this server, without sudo.</p>
</details>
</section>
<p class=\"foot\">One use · expires in {expires} · it never shows your token</p>
<section class=\"agents\" id=\"for-agents\">
<h2>For AI agents</h2>
<p>This page invites the person you work for to join a Relevo server, as @{handle}. Relevo \
passes notes between teammates' agents. To accept, first ask your person whether to join. Only \
if they say yes, run this once, in a shell on their machine. It installs relevo in \
~/.local/bin and connects it to this server; it never shows the token.</p>
<pre><code>{command} | sh -s -- --setup claude-code</code></pre>
<p>If you are pi, end it with <code>--setup pi</code>; if you are Hermes, with \
<code>--setup hermes</code>. The invitation works once: if the command fails, tell your person \
what it said rather than running it again. Do not share this link.</p>
</section>
",
        handle = escape(&handle),
        name = escape(&name),
        link = escape(&link),
        command = escape(&command),
        mac_tab = tab("mac", mac, "macOS"),
        linux_tab = tab("linux", !mac, "Linux"),
        mac_panel = panel(
            "mac",
            mac,
            has("-apple-darwin"),
            "macOS",
            "For macOS on Apple Silicon."
        ),
        linux_panel = panel(
            "linux",
            !mac,
            has("-linux-gnu"),
            "Linux",
            "For Linux on x86_64 or ARM64, and WSL on Windows."
        ),
        expires = expires_in(expires_at - app.clock.now()),
    );
    html(StatusCode::OK, &format!("Join Relevo as @{handle}"), &main)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_for_sh() {
        assert_eq!(
            sh_word("https://10.0.0.5:7878/join/K7Q2-9FPA"),
            "https://10.0.0.5:7878/join/K7Q2-9FPA"
        );
        assert_eq!(sh_word("sha256//ab+c/d="), "sha256//ab+c/d=");
        assert_eq!(sh_word("https://x/a b"), "'https://x/a b'");
        assert_eq!(sh_word("it's"), r"'it'\''s'");
        assert_eq!(sh_word(""), "''");
    }

    #[test]
    fn addresses_count_by_host() {
        let peer = |s: &str| Some(Extension(ConnectInfo(s.parse::<SocketAddr>().unwrap())));
        assert_eq!(
            address(&peer("10.0.0.1:5")),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            address(&peer("[::ffff:10.0.0.1]:5")),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            address(&peer("[2001:db8:1:2:3:4:5:6]:5")),
            "2001:db8:1:2::".parse::<IpAddr>().unwrap()
        );
        assert_eq!(address(&None), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn expiry_in_words() {
        assert_eq!(expires_in(24 * 3600), "24 h");
        assert_eq!(expires_in(23 * 3600 + 100), "23 h");
        assert_eq!(expires_in(3599), "59 min");
        assert_eq!(expires_in(10), "1 min");
    }
}
