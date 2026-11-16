//! Invitations (docs/protocol.md, "Invitations"): the routes that need no token, keyed by an
//! invitation's code. `POST /api/join` trades the code for a token of its person, once.
//! `GET /join/<code>` is the page the link opens, `GET /join/<code>/install` the script that
//! installs relevo and connects it, and `GET /join/<code>/relevo?target=` the binary the script
//! downloads, from the dist: none of them uses the code up. An address that tries too many codes
//! that do not work is refused for a while, so that codes cannot be guessed. Codes and tokens are
//! never logged.

use std::collections::HashMap;
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

/// Tries at codes that do not exist one address may make in `FAILURE_WINDOW_SECS`, and all
/// addresses together; beyond either, every invitation route refuses that address (or everyone)
/// until the oldest of them is that old. A used or expired code does not count: someone was given
/// it, and is told to ask for a new one.
pub const MAX_FAILURES: usize = 10;
pub const MAX_ALL_FAILURES: usize = 200;
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

/// The tries at codes that do not exist, in memory only.
#[derive(Default)]
pub(crate) struct Tries {
    /// When each address (see `address`) made one: those in the window, and older ones until the
    /// next prune.
    by_address: HashMap<IpAddr, Vec<i64>>,
    /// When the old ones were last dropped.
    pruned_at: i64,
    /// When the server last logged that it refuses everyone.
    warned_at: Option<i64>,
}

/// A try at a code under way. It counts as one at a code that does not exist from the moment it
/// starts, under the lock, so that tries made at once cannot pass the limits together; `forget`
/// takes it back once the code turns out to exist.
struct Try<'a> {
    app: &'a App,
    addr: IpAddr,
    at: i64,
}

/// Starts a try from `addr`, or refuses it when that address, or everyone, made too many.
fn start(app: &App, addr: IpAddr, now: i64) -> Result<Try<'_>, Refusal> {
    let fresh = |t: &i64| *t > now - FAILURE_WINDOW_SECS;
    let count = |times: &Vec<i64>| times.iter().filter(|t| fresh(t)).count();
    let mut tries = lock(&app.join_tries);
    if !fresh(&tries.pruned_at) {
        tries.by_address.retain(|_, times| {
            times.retain(fresh);
            !times.is_empty()
        });
        tries.pruned_at = now;
    }
    let all: usize = tries.by_address.values().map(count).sum();
    if all >= MAX_ALL_FAILURES {
        if !tries.warned_at.is_some_and(|at| fresh(&at)) {
            tracing::warn!(
                "{all} tries at invitation codes that do not exist in an hour: every invitation \
                 route is refused for a while"
            );
            tries.warned_at = Some(now);
        }
        return Err(Refusal::Limited);
    }
    let times = tries.by_address.entry(addr).or_default();
    if count(times) >= MAX_FAILURES {
        return Err(Refusal::Limited);
    }
    times.push(now);
    Ok(Try { app, addr, at: now })
}

impl Try<'_> {
    /// The code exists (or nothing was looked up): this try does not count.
    fn forget(self) {
        let mut tries = lock(&self.app.join_tries);
        if let Some(times) = tries.by_address.get_mut(&self.addr)
            && let Some(i) = times.iter().rposition(|t| *t == self.at)
        {
            times.remove(i);
        }
    }
}

/// Why an invitation route does not go on.
enum Refusal {
    Limited,
    Used,
    Expired,
    Unknown,
    Internal,
}

impl From<Invite> for Refusal {
    /// What a code that does not work answers (a valid one is no refusal: it stays `Unknown`).
    fn from(invite: Invite) -> Self {
        match invite {
            Invite::Used(_) => Refusal::Used,
            Invite::Expired(_) => Refusal::Expired,
            Invite::Valid { .. } | Invite::Unknown => Refusal::Unknown,
        }
    }
}

impl Refusal {
    fn status(&self) -> StatusCode {
        match self {
            Refusal::Limited => StatusCode::TOO_MANY_REQUESTS,
            Refusal::Used | Refusal::Expired => StatusCode::GONE,
            Refusal::Unknown => StatusCode::NOT_FOUND,
            Refusal::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Refusal::Limited => "RATE_LIMITED",
            Refusal::Used => "INVITE_USED",
            Refusal::Expired => "INVITE_EXPIRED",
            Refusal::Unknown => "INVITE_UNKNOWN",
            Refusal::Internal => "INTERNAL",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Refusal::Limited => "Too many attempts",
            Refusal::Used => "This invitation was already used",
            Refusal::Expired => "This invitation has expired",
            Refusal::Unknown => "Invitation not found",
            Refusal::Internal => "Something went wrong",
        }
    }

    fn message(&self) -> String {
        match self {
            Refusal::Limited => {
                "Too many wrong invitation links were tried. Wait an hour and try again.".into()
            }
            Refusal::Used | Refusal::Expired => "Invitations work once and last 24 hours. Ask \
                                                 whoever invited you for a new link."
                .into(),
            Refusal::Unknown => "Check you have the whole link, or ask for a new one.".into(),
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
    let now = app.clock.now();
    let attempt = start(app, address(peer), now)?;
    let found = db::find_invite(&app.db(), code, now);
    if !matches!(found, Ok(Invite::Unknown)) {
        attempt.forget();
    }
    match found {
        Ok(found @ Invite::Valid { .. }) => Ok(found),
        Ok(other) => Err(other.into()),
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
    let body = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let req: JoinRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("the body must be {{ \"code\": … }}: {e}")))?;
    let now = app.clock.now();
    let attempt = start(&app, address(&peer), now).map_err(|refusal| refusal.api())?;
    let redeemed = db::redeem_invite(&app.db(), &req.code, now);
    if !matches!(redeemed, Ok(Err(Invite::Unknown))) {
        attempt.forget();
    }
    match redeemed {
        Ok(Ok(joined)) => {
            tracing::info!(person = %joined.handle, token = %joined.token.id, "joined with an invitation");
            Ok(Json(json!({
                "token": joined.token.secret,
                "handle": joined.handle,
                "url": joined.url,
            })))
        }
        Ok(Err(state)) => Err(Refusal::from(state).api()),
        Err(e) => {
            tracing::error!(error = %e, "using an invitation");
            Err(ApiError::internal())
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
/// it has one. Without `-f`: when the code does not work, the server's answer is a script that
/// says why and fails, and sh runs that.
fn curl_install(url: &str, code: &str, pin: Option<&str>) -> String {
    let trust = pin
        .map(|pin| format!("--pinnedpubkey {} -k ", sh_word(&format!("sha256//{pin}"))))
        .unwrap_or_default();
    format!(
        "curl -sS {trust}{}",
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

/// `template` with each `@NAME@` of `values` in place, in one pass: a value is never read again,
/// so one that holds `@NAME@` stays as it is. Any other `@` stays too (`"$@"`).
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let named = values.iter().find(|(name, _)| {
            after
                .strip_prefix(name)
                .is_some_and(|tail| tail.starts_with('@'))
        });
        match named {
            Some((name, value)) => {
                out.push_str(value);
                rest = &after[name.len() + 1..];
            }
            None => {
                out.push('@');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `GET /join/<code>/install`: the script, or one that says why there is none and fails.
async fn install(State(app): State<Arc<App>>, peer: Peer, Path(code): Path<String>) -> Response {
    let (status, script) = match check(&app, &peer, &code) {
        Ok(Invite::Valid { address, .. }) => {
            let sums: Vec<String> = binaries(&app)
                .await
                .into_iter()
                .map(|(target, sum)| format!("{target} {sum}"))
                .collect();
            let code = db::invite_code(&code).expect("a valid invitation's code");
            let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
            let values = [
                ("URL", quote(&address.url)),
                ("CODE", quote(&code)),
                ("PIN", quote(address.pin.as_deref().unwrap_or(""))),
                (
                    "FINGERPRINT",
                    quote(address.fingerprint.as_deref().unwrap_or("")),
                ),
                ("SUMS", quote(&sums.join("\n"))),
            ];
            let script = fill(INSTALL, &values);
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
    let (handle, address, expires_at) = match found {
        Ok(Invite::Valid {
            handle,
            address,
            expires_at,
            ..
        }) => (handle, address, expires_at),
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
    let targets = binaries(&app).await;
    let code = db::invite_code(&code).expect("a valid invitation's code");
    let url = &address.url;
    let link = format!("{url}/join/{code}");
    let command = curl_install(url, &code, address.pin.as_deref());
    // Plain http: nothing can show that the server is the one that invited you.
    let plain = if url.starts_with("http://") {
        "<p class=\"warn\">This link is not encrypted (plain HTTP). Use it only on a network you \
         trust, like your office or a VPN.</p>\n"
    } else {
        ""
    };
    let mac = headers
        .get(header::USER_AGENT)
        .and_then(|ua| ua.to_str().ok())
        .is_some_and(|ua| ua.contains("Macintosh") || ua.contains("Mac OS X"));
    let has = |suffix: &str| targets.iter().any(|(t, _)| t.ends_with(suffix));
    let panel = |id: &str, on: bool, has: bool, system: &str, under: &str| {
        let body = if has {
            format!(
                "<div class=\"row\"><code class=\"cmd\" id=\"cmd-{id}\">{} | sh</code>\
                 <button type=\"button\" class=\"ghost\" data-copy=\"cmd-{id}\" \
                 aria-label=\"Copy command\">Copy</button></div>\n\
                 <p class=\"under\">{under}</p>",
                escape(&command)
            )
        } else {
            format!(
                "<p class=\"under\">No {system} build on this server yet. Ask whoever invited you.</p>"
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
<p class=\"lead\">You'll join as <b>@{handle}</b>. Your agents will pass notes to your \
teammates' agents, and only what needs you reaches you. Open this on the computer where your \
agents run.</p>
{plain}<section class=\"card\">
<h2>Give this link to your agent</h2>
<p>Paste it in Claude Code, pi or Hermes and say “join”. Your agent asks you before it runs \
anything.</p>
<div class=\"row\"><code id=\"link\">{link}</code><button type=\"button\" data-copy=\"link\" \
aria-label=\"Copy link\">Copy link</button></div>
</section>
<section class=\"card\">
<h2>Or run this in a terminal</h2>
<div class=\"tabs\" role=\"tablist\">{mac_tab}{linux_tab}</div>
{mac_panel}{linux_panel}<details><summary>Rather not pipe to sh?</summary>
<p>Save the script instead: the same command with <code>-o relevo-join.sh</code> in place of \
<code>| sh</code>. Read it, then run <code>sh relevo-join.sh</code>. It installs relevo in \
<code>~/.local/bin</code> and connects it to this server, without sudo. <code>-k</code> with \
<code>--pinnedpubkey</code> trusts only this server's own certificate.</p>
</details>
</section>
<p class=\"foot\">Works once · expires in {expires}</p>
<p class=\"sr\" id=\"copied\" aria-live=\"polite\"></p>
<details class=\"agents\" id=\"for-agents\">
<summary>For AI agents</summary>
<p>This page invites the user you work for to join a Relevo server, as @{handle}. Relevo passes \
notes between teammates' agents. To accept, first ask the user whether to join. Only if they \
say yes, run this once, in a shell on their machine. It installs relevo in ~/.local/bin, \
connects it to this server and adds Relevo to you; it never shows the token. Then tell the user \
what it said, and that you need a restart to load Relevo.</p>
<pre><code>{command} | sh -s -- --setup AGENT</code></pre>
<p>Replace AGENT with what you are: <code>claude-code</code>, <code>pi</code> or \
<code>hermes</code>. Copy the command exactly. The invitation works once: if the command fails, \
tell the user what it said rather than running it again. Do not share this link.</p>
</details>
",
        handle = escape(&handle),
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
    fn a_template_is_filled_in_one_pass() {
        let values = [
            ("URL", "'@SUMS@'".to_string()),
            ("SUMS", "'x @URL@'".to_string()),
        ];
        assert_eq!(
            fill("u=@URL@ s=@SUMS@ \"$@\" a@b @NONE@", &values),
            "u='@SUMS@' s='x @URL@' \"$@\" a@b @NONE@"
        );
    }

    #[test]
    fn expiry_in_words() {
        assert_eq!(expires_in(24 * 3600), "24 h");
        assert_eq!(expires_in(23 * 3600 + 100), "23 h");
        assert_eq!(expires_in(3599), "59 min");
        assert_eq!(expires_in(10), "1 min");
    }
}
