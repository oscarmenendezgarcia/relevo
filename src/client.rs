//! A client of the HTTP API (`docs/protocol.md`), as one session of one token.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;

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
}

#[derive(Debug)]
pub enum Error {
    /// No answer from the server (could not connect, timed out, cut off).
    Network(String),
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
            Error::Api { code, message, .. } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl Client {
    /// Fails only when TLS cannot be set up (no CA certificates on the system) for an `https://`
    /// server; a plain `http://` one needs none.
    pub fn new(
        config: &Config,
        fixed_context: Option<String>,
        dir: PathBuf,
    ) -> anyhow::Result<Self> {
        // reqwest is built without a default TLS provider; ring is the one this binary carries.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let builder =
            || reqwest::Client::builder().user_agent(concat!("relevo/", env!("CARGO_PKG_VERSION")));
        let http = match builder().build() {
            Ok(http) => http,
            // The system's roots are loaded up front even for plain http; without any, an empty
            // root store still serves http (and refuses every https certificate).
            Err(_) if config.url.starts_with("http://") => builder()
                .tls_certs_only([])
                .build()
                .map_err(|e| anyhow::anyhow!("setting up HTTP: {}", causes(&e)))?,
            Err(e) => anyhow::bail!("TLS roots could not be loaded: {}", causes(&e)),
        };
        Ok(Client {
            http,
            url: config.url.clone(),
            token: config.token.clone(),
            session: crate::db::random_id("mcp_"),
            fixed_context,
            dir,
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
            .timeout(timeout);
        if let Some(context) = self.context().await {
            req = req.header("relevo-context", context);
        }
        let res = req.send().await.map_err(network)?;
        let status = res.status();
        let bytes = res.bytes().await.map_err(network)?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
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

    pub async fn people(&self) -> Result<Value, Error> {
        let req = self.http.get(format!("{}/api/people", self.url));
        self.call(req, TIMEOUT).await
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
        self.call(req, TIMEOUT + Duration::from_secs(wait)).await
    }

    pub async fn thread(&self, id: &str) -> Result<Value, Error> {
        // A thread id is `r_` + base32; anything else is sent encoded and the server says it is
        // not found.
        let req = self
            .http
            .get(format!("{}/api/threads/{}", self.url, encode_segment(id)));
        self.call(req, TIMEOUT).await
    }

    /// `POST /api/send` with a fresh idempotency key, sent again once with the same key when the
    /// first try got no answer.
    pub async fn send(&self, mut body: serde_json::Map<String, Value>) -> Result<Value, Error> {
        body.insert("key".into(), crate::db::random_id("k_").into());
        let body = Value::Object(body);
        let post = || self.http.post(format!("{}/api/send", self.url)).json(&body);
        match self.call(post(), TIMEOUT).await {
            Err(Error::Network(_)) => self.call(post(), TIMEOUT).await,
            other => other,
        }
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

    #[test]
    fn segments_are_encoded() {
        assert_eq!(encode_segment("r_abc"), "r_abc");
        assert_eq!(encode_segment("../x y"), "%2E%2E%2Fx%20y");
    }
}
