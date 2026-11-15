//! A client of the HTTP API (`docs/protocol.md`), as one session of one token.

use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;

/// What every request says of its client (`Relevo-Client`, docs/protocol.md): `relevo mcp`, and
/// the binary's other commands that talk to the server (`connect`, `update`, `web`), all one
/// binary of this version.
pub const CLIENT: &str = concat!("mcp/", env!("CARGO_PKG_VERSION"));

/// How long an ordinary request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
    /// This process's `Relevo-Session`.
    pub session: String,
    /// `RELEVO_CONTEXT`, which overrides git when set.
    fixed_context: Option<String>,
    /// Where git looks for the context.
    dir: PathBuf,
    /// The handle of the person the token acts for (`you`), once an answer has said it.
    you: Mutex<Option<String>>,
}

#[derive(Debug)]
pub enum Error {
    /// No answer from the server (could not connect, timed out, cut off).
    Network(String),
    /// A 2xx answer that is not what a Relevo server sends (not JSON, or the wrong shape): the
    /// URL most likely points at something else.
    BadResponse(String),
    /// The server answered with an error.
    Api {
        status: u16,
        code: String,
        message: String,
        body: Value,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Network(e) => write!(f, "could not reach the Relevo server: {e}"),
            Error::BadResponse(e) => write!(
                f,
                "the server did not answer as a Relevo server does ({e}); is the URL right?"
            ),
            Error::Api { code, message, .. } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl Client {
    /// Fails only when TLS cannot be set up: a pinned certificate (`config.cert`) that is not
    /// PEM, or, for an `https://` server without one, no CA certificates on the system. A plain
    /// `http://` server needs neither.
    pub fn new(
        config: &Config,
        fixed_context: Option<String>,
        dir: PathBuf,
    ) -> anyhow::Result<Self> {
        // reqwest is built without a default TLS provider; ring is the one this binary carries.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let builder = || {
            reqwest::Client::builder()
                .user_agent(concat!("relevo/", env!("CARGO_PKG_VERSION")))
                .default_headers(reqwest::header::HeaderMap::from_iter([(
                    reqwest::header::HeaderName::from_static("relevo-client"),
                    reqwest::header::HeaderValue::from_static(CLIENT),
                )]))
        };
        let http = match &config.cert {
            // That certificate alone, never the system's authorities.
            Some(pem) => builder()
                .tls_backend_preconfigured(
                    crate::tls::pinned(pem)
                        .map_err(|e| anyhow::anyhow!("the saved certificate: {e:#}"))?,
                )
                .build()
                .map_err(|e| anyhow::anyhow!("setting up TLS: {}", causes(&e)))?,
            None => match builder().build() {
                Ok(http) => http,
                // The system's roots are loaded up front even for plain http; without any, an
                // empty root store still serves http (and refuses every https certificate).
                Err(_) if config.url.starts_with("http://") => builder()
                    .tls_certs_only([])
                    .build()
                    .map_err(|e| anyhow::anyhow!("setting up HTTP: {}", causes(&e)))?,
                Err(e) => anyhow::bail!("TLS roots could not be loaded: {}", causes(&e)),
            },
        };
        Ok(Client {
            http,
            url: config.url.clone(),
            token: config.token.clone(),
            session: crate::db::random_id("mcp_"),
            fixed_context,
            dir,
            you: Mutex::new(None),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The context this session has now.
    pub async fn context(&self) -> Option<String> {
        crate::context::current(&self.dir, self.fixed_context.as_deref()).await
    }

    async fn call(&self, req: reqwest::RequestBuilder, timeout: Duration) -> Result<Value, Error> {
        let mut req = req
            .bearer_auth(&self.token)
            .header("relevo-session", &self.session)
            // A reply's mentions name people (docs/protocol.md, `send`).
            .header("relevo-mentions", "1")
            .timeout(timeout);
        if let Some(context) = self.context().await {
            req = req.header("relevo-context", context);
        }
        let res = req.send().await.map_err(network)?;
        let status = res.status();
        let bytes = res.bytes().await.map_err(network)?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            if !body.is_object() {
                return Err(Error::BadResponse(format!("{status}, not a JSON object")));
            }
            return Ok(body);
        }
        let err = &body["error"];
        Err(Error::Api {
            status: status.as_u16(),
            code: err["code"].as_str().unwrap_or("HTTP_ERROR").to_string(),
            message: err["message"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("the server answered {status}")),
            body,
        })
    }

    /// `GET path` with the token alone, no session or context (it counts as no session's
    /// request), and its body as bytes: at most `max` of them, or `BadResponse`.
    pub async fn fetch(&self, path: &str, max: u64, timeout: Duration) -> Result<Vec<u8>, Error> {
        let mut res = self
            .http
            .get(format!("{}{path}", self.url))
            .bearer_auth(&self.token)
            .timeout(timeout)
            .send()
            .await
            .map_err(network)?;
        let status = res.status();
        // An error's body is small; a body bigger than asked for is not read at all.
        let max = if status.is_success() { max } else { 64 * 1024 };
        if res.content_length().is_some_and(|n| n > max) {
            return Err(Error::BadResponse(format!(
                "{status}, more than {max} bytes"
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(network)? {
            if (bytes.len() + chunk.len()) as u64 > max {
                return Err(Error::BadResponse(format!(
                    "{status}, more than {max} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status.is_success() {
            return Ok(bytes);
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let err = &body["error"];
        Err(Error::Api {
            status: status.as_u16(),
            code: err["code"].as_str().unwrap_or("HTTP_ERROR").to_string(),
            message: err["message"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("the server answered {status}")),
            body,
        })
    }

    pub async fn people(&self) -> Result<Value, Error> {
        let req = self.http.get(format!("{}/api/people", self.url));
        let v = shaped(
            self.call(req, TIMEOUT).await,
            &[("team", Value::is_array), ("you", Value::is_string)],
        )?;
        self.learn_you(&v["you"]);
        Ok(v)
    }

    fn learn_you(&self, you: &Value) {
        if let Some(you) = you.as_str() {
            *self.you.lock().unwrap_or_else(|e| e.into_inner()) = Some(you.to_string());
        }
    }

    /// The handle of the person the token acts for, if an answer has said it yet.
    pub fn known_you(&self) -> Option<String> {
        self.you.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The handle of the person the token acts for: from an earlier answer, or else asked of
    /// `GET /api/people`. `None` when the server does not say.
    pub async fn you(&self) -> Option<String> {
        if self.known_you().is_none() {
            // `people` keeps it.
            self.people().await.ok()?;
        }
        self.known_you()
    }

    /// `GET /api/inbox`; `wait` in seconds, 0 for none.
    pub async fn inbox(&self, all: bool, wait: u64) -> Result<Value, Error> {
        let mut query = vec![];
        if all {
            query.push("all=1".to_string());
        }
        if wait > 0 {
            query.push(format!("wait={wait}"));
        }
        let mut url = format!("{}/api/inbox", self.url);
        if !query.is_empty() {
            url = format!("{url}?{}", query.join("&"));
        }
        let req = self.http.get(url);
        let v = self.call(req, TIMEOUT + Duration::from_secs(wait)).await;
        let v = shaped(
            v,
            &[("threads", Value::is_array), ("team", Value::is_array)],
        )?;
        self.learn_you(&v["session"]["you"]);
        Ok(v)
    }

    /// `GET /api/timeline?<query>`: read only, it delivers nothing, and the server ignores the
    /// session headers (docs/protocol.md, "timeline").
    pub async fn timeline(&self, query: &str) -> Result<Value, Error> {
        let req = self.http.get(format!("{}/api/timeline?{query}", self.url));
        shaped(
            self.call(req, TIMEOUT).await,
            &[
                ("threads", Value::is_array),
                ("counts", Value::is_object),
                ("you", Value::is_string),
            ],
        )
    }

    pub async fn thread(&self, id: &str) -> Result<Value, Error> {
        // A thread id is `r_` + base32; anything else is sent encoded and the server says it is
        // not found.
        let req = self
            .http
            .get(format!("{}/api/threads/{}", self.url, encode_segment(id)));
        shaped(
            self.call(req, TIMEOUT).await,
            &[("id", Value::is_string), ("notes", Value::is_array)],
        )
    }

    /// `DELETE /api/session`: ends this session, so the threads it holds go back to the
    /// person's other sessions at once. Waits at most `timeout`, context lookup included.
    pub async fn end(&self, timeout: Duration) -> Result<(), Error> {
        let req = self.http.delete(format!("{}/api/session", self.url));
        match tokio::time::timeout(timeout, self.call(req, timeout)).await {
            Ok(v) => v.map(drop),
            Err(_) => Err(Error::Network("timed out".into())),
        }
    }

    /// `POST /api/send` with a fresh idempotency key, sent again once with the same key when the
    /// first try got no answer from Relevo itself (no answer at all, or a gateway error from a
    /// proxy in front of it): the server may have written the note, and a replay is safe.
    pub async fn send(&self, mut body: serde_json::Map<String, Value>) -> Result<Value, Error> {
        body.insert("key".into(), crate::db::random_id("k_").into());
        let body = Value::Object(body);
        let post = || self.http.post(format!("{}/api/send", self.url)).json(&body);
        let mut v = self.call(post(), TIMEOUT).await;
        if unanswered(&v) {
            v = self.call(post(), TIMEOUT).await;
        }
        let maybe = "; the note may have been written: check with thread or inbox before sending \
                     it again";
        match v {
            Err(Error::Network(e)) => Err(Error::Network(format!("{e}{maybe}"))),
            Err(Error::Api {
                status,
                code,
                message,
                body,
            }) if is_gateway(status, &code) => Err(Error::Api {
                status,
                code,
                message: format!("{message}{maybe}"),
                body,
            }),
            v => shaped(v, &[("thread", Value::is_string)]),
        }
    }
}

impl Error {
    /// The server was not reached, or only a proxy in front of it answered.
    pub fn unreachable(&self) -> bool {
        match self {
            Error::Network(_) => true,
            Error::Api { status, code, .. } => is_gateway(*status, code),
            Error::BadResponse(_) => false,
        }
    }
}

/// A 502, 503 or 504 without Relevo's error body: a proxy's answer, not the server's.
fn is_gateway(status: u16, code: &str) -> bool {
    matches!(status, 502..=504) && code == "HTTP_ERROR"
}

/// Whether `v` failed before Relevo answered.
fn unanswered(v: &Result<Value, Error>) -> bool {
    match v {
        Err(Error::Network(_)) => true,
        Err(Error::Api { status, code, .. }) => is_gateway(*status, code),
        _ => false,
    }
}

/// A field an answer must have, and the test of its type.
type Field<'a> = (&'a str, fn(&Value) -> bool);

/// `v`, unless it is an answer that lacks one of `fields` (with the right type).
fn shaped(v: Result<Value, Error>, fields: &[Field]) -> Result<Value, Error> {
    let v = v?;
    match fields.iter().find(|(name, ok)| !ok(&v[*name])) {
        Some((name, _)) => Err(Error::BadResponse(format!("no `{name}` in the answer"))),
        None => Ok(v),
    }
}

fn network(e: reqwest::Error) -> Error {
    // Without the URL (it is known), with the causes ("connection refused").
    Error::Network(causes(&e.without_url()))
}

/// `e` and its causes, one after the other.
fn causes(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        msg.push_str(": ");
        msg.push_str(&cause.to_string());
        source = cause.source();
    }
    msg
}

/// `s` as one path segment.
fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Reads one HTTP request (headers and a `content-length` body) and returns its body.
    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed mid-request");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let len: usize = text[..end]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    return text[end + 4..end + 4 + len].to_string();
                }
            }
        }
    }

    #[tokio::test]
    async fn send_retries_once_with_the_same_key() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // The first try is read and then cut off without an answer.
            let (mut sock, _) = listener.accept().await.unwrap();
            let first = read_request(&mut sock).await;
            drop(sock);
            let (mut sock, _) = listener.accept().await.unwrap();
            let second = read_request(&mut sock).await;
            let reply = r#"{"thread":"r_x","note":"n_x","status":"open","claimed":"this-session"}"#;
            let res = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            sock.write_all(res.as_bytes()).await.unwrap();
            (first, second)
        });
        let config = Config {
            url: format!("http://{addr}"),
            token: "rlv_test".into(),
            cert: None,
            adapters: vec![],
        };
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        let mut body = serde_json::Map::new();
        body.insert("thread".into(), "r_x".into());
        body.insert("text".into(), "hi".into());
        let out = client.send(body).await.unwrap();
        assert_eq!(out["note"], "n_x");
        let (first, second) = server.await.unwrap();
        let first: Value = serde_json::from_str(&first).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert!(first["key"].as_str().unwrap().starts_with("k_"));
        assert_eq!(first, second);
    }

    /// A server that answers the requests it gets with `answers` in turn, one per connection,
    /// and returns the request bodies.
    async fn answering(
        answers: Vec<(&'static str, &'static str)>,
    ) -> (Config, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = vec![];
            for (status, body) in answers {
                let (mut sock, _) = listener.accept().await.unwrap();
                bodies.push(read_request(&mut sock).await);
                let res = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(res.as_bytes()).await.unwrap();
            }
            bodies
        });
        let config = Config {
            url: format!("http://{addr}"),
            token: "rlv_test".into(),
            cert: None,
            adapters: vec![],
        };
        (config, server)
    }

    fn body() -> serde_json::Map<String, Value> {
        let mut body = serde_json::Map::new();
        body.insert("thread".into(), "r_x".into());
        body.insert("text".into(), "hi".into());
        body
    }

    #[tokio::test]
    async fn send_retries_a_gateway_error_and_says_the_note_may_be_written() {
        let ok = r#"{"thread":"r_x","note":"n_x","status":"open","claimed":"this-session"}"#;
        let (config, server) = answering(vec![
            ("502 Bad Gateway", "<html>bad gateway</html>"),
            ("200 OK", ok),
        ])
        .await;
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        let out = client.send(body()).await.unwrap();
        assert_eq!(out["note"], "n_x");
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0], bodies[1]);

        let gateway = ("504 Gateway Timeout", "<html>timeout</html>");
        let (config, server) = answering(vec![gateway, gateway]).await;
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        let err = client.send(body()).await.unwrap_err().to_string();
        assert!(err.contains("504"), "{err}");
        assert!(err.contains("the note may have been written"), "{err}");
        server.await.unwrap();

        // Relevo's own errors are final, and say nothing of the kind.
        let busy = r#"{"error":{"code":"THREAD_BUSY","message":"busy"}}"#;
        let (config, server) = answering(vec![("429 Too Many Requests", busy)]).await;
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        let err = client.send(body()).await.unwrap_err().to_string();
        assert_eq!(err, "THREAD_BUSY: busy");
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn who_we_are_comes_from_the_answers_and_is_kept() {
        let people = r#"{"team":[],"you":"ana"}"#;
        let (config, server) = answering(vec![("200 OK", people)]).await;
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        assert_eq!(client.known_you(), None);
        assert_eq!(client.you().await.as_deref(), Some("ana"));
        // Kept: no second request.
        assert_eq!(client.you().await.as_deref(), Some("ana"));
        assert_eq!(server.await.unwrap().len(), 1);

        let inbox = r#"{"team":[],"session":{"label":"x","context":null,"you":"bob"},"threads":[],"more":0}"#;
        let (config, server) = answering(vec![("200 OK", inbox)]).await;
        let client = Client::new(&config, Some(String::new()), std::env::temp_dir()).unwrap();
        client.inbox(false, 0).await.unwrap();
        assert_eq!(client.known_you().as_deref(), Some("bob"));
        server.await.unwrap();
    }

    #[test]
    fn segments_are_encoded() {
        assert_eq!(encode_segment("r_abc"), "r_abc");
        assert_eq!(encode_segment("../x y"), "%2E%2E%2Fx%20y");
    }
}
