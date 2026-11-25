//! The server's transport (`relevo::serve`): HTTP/1.1, plain or over TLS, and the limits of
//! `docs/protocol.md` on what one client can hold. Short limits stand in for the real ones.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use relevo::serve::{Limits, serve};
use relevo::{App, Clock, api, db, tls};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

struct Server {
    addr: SocketAddr,
    token: String,
    /// The certificate (PEM), when it speaks TLS.
    pem: Option<String>,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A server with `ana` (whose token it has) and `bob`, on a random port of loopback; over TLS (a certificate for
/// `localhost` and `127.0.0.1`) with `with_tls`.
async fn start(limits: Limits, with_tls: bool) -> Server {
    start_on(IpAddr::from([127, 0, 0, 1]), limits, with_tls).await
}

/// The same, listening on `ip`, so that its clients come from `ip` too.
async fn start_on(ip: IpAddr, limits: Limits, with_tls: bool) -> Server {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = App::new(db::open(":memory:").unwrap(), Clock::system(), 5).unwrap();
    let token = db::invite(&app.db(), "ana", "Ana", None, "test", db::now_unix())
        .unwrap()
        .secret;
    db::invite(&app.db(), "bob", "Bob", None, "test", db::now_unix()).unwrap();
    let dir = std::env::temp_dir().join(db::random_id("relevo-transport-"));
    std::fs::create_dir(&dir).unwrap();
    let (config, pem) = if with_tls {
        let names = ["localhost".to_string(), "127.0.0.1".to_string()];
        let certs = dir.join("relevo.db.tls");
        let (cert, _) = tls::stored_or_new(&certs, &names, Vec::new).unwrap();
        let pem = std::fs::read_to_string(certs.join("cert.pem")).unwrap();
        (Some(cert.server_config().unwrap()), Some(pem))
    } else {
        (None, None)
    };
    let listener = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, api::router(app), config, limits));
    Server {
        addr,
        token,
        pem,
        dir,
    }
}

impl Server {
    fn url(&self, path: &str) -> String {
        let scheme = if self.pem.is_some() { "https" } else { "http" };
        format!("{scheme}://{}{path}", self.addr)
    }

    /// A client that trusts the server's certificate alone.
    fn client(&self) -> reqwest::Client {
        let builder = reqwest::Client::builder();
        match &self.pem {
            Some(pem) => builder.tls_backend_preconfigured(tls::pinned(pem).unwrap()),
            None => builder.tls_certs_only([]),
        }
        .build()
        .unwrap()
    }

    /// A connection that has shaken hands, when TLS is on.
    async fn stream(&self) -> Box<dyn Stream> {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        match &self.pem {
            None => Box::new(tcp),
            Some(pem) => {
                let connector =
                    tokio_rustls::TlsConnector::from(Arc::new(tls::pinned(pem).unwrap()));
                let name = "127.0.0.1".try_into().unwrap();
                Box::new(connector.connect(name, tcp).await.unwrap())
            }
        }
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Short limits for the timeouts, the real ones for the rest.
fn short() -> Limits {
    Limits {
        head: Duration::from_secs(1),
        body: Duration::from_secs(1),
        idle: Duration::from_secs(30),
        request: Duration::from_secs(5),
        ..Limits::default()
    }
}

/// How long until the server closes `s` (an error counts as closed), reading and dropping what
/// it sends; panics after `within`.
async fn closed_after(s: &mut (impl AsyncRead + Unpin + ?Sized), within: Duration) -> Duration {
    let start = Instant::now();
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout_at((start + within).into(), s.read(&mut buf)).await {
            Err(_) => panic!("still open after {within:?}"),
            Ok(Ok(0)) | Ok(Err(_)) => return start.elapsed(),
            Ok(Ok(_)) => {}
        }
    }
}

/// Reads one HTTP response (head and a `content-length` body) from `s`.
async fn response(s: &mut (impl AsyncRead + Unpin + ?Sized)) -> String {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("an answer in time")
            .unwrap();
        assert!(n > 0, "closed before answering");
        got.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&got).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .map(|n| n.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            if got.len() >= end + 4 + length {
                return text;
            }
        }
    }
}

const HEALTH: &[u8] = b"GET /health HTTP/1.1\r\nhost: relevo\r\n\r\n";

#[tokio::test]
async fn plain_http_serves_as_before() {
    let s = start(Limits::default(), false).await;
    let client = s.client();
    // Twice, on one kept-alive connection.
    for _ in 0..2 {
        let res = client.get(s.url("/health")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.json::<Value>().await.unwrap(), json!({ "ok": true }));
    }
    let res = client
        .post(s.url("/api/send"))
        .bearer_auth(&s.token)
        .json(&json!({ "to": ["bob"], "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.json::<Value>().await.unwrap()["thread"].is_string());
    let res = client.get(s.url("/nowhere")).send().await.unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn https_serves_the_api() {
    let s = start(Limits::default(), true).await;
    let client = s.client();
    let res = client.get(s.url("/health")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let res = client
        .get(s.url("/api/people"))
        .bearer_auth(&s.token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.json::<Value>().await.unwrap()["you"], "ana");
    // Plain HTTP to the TLS port gets no answer.
    let mut tcp = TcpStream::connect(s.addr).await.unwrap();
    tcp.write_all(HEALTH).await.unwrap();
    closed_after(&mut tcp, Duration::from_secs(3)).await;
}

#[tokio::test]
async fn a_slow_request_head_is_dropped_after_the_head_timeout() {
    for with_tls in [false, true] {
        let s = start(short(), with_tls).await;

        // Part of a head, and nothing more.
        let mut c = s.stream().await;
        c.write_all(b"GET /health HTTP/1.1\r\nhost: relevo\r\n")
            .await
            .unwrap();
        let after = closed_after(&mut c, Duration::from_secs(4)).await;
        assert!(after >= Duration::from_millis(800), "{with_tls}: {after:?}");

        // A connection that never even says hello (no TLS handshake, or no byte).
        let start = Instant::now();
        let mut tcp = TcpStream::connect(s.addr).await.unwrap();
        closed_after(&mut tcp, Duration::from_secs(4)).await;
        assert!(start.elapsed() >= Duration::from_millis(800));

        // After a request, the connection is idle (30 s here); a new head that starts there
        // has the head's time, not the idle's.
        let mut c = s.stream().await;
        c.write_all(HEALTH).await.unwrap();
        assert!(response(&mut c).await.starts_with("HTTP/1.1 200"));
        c.write_all(b"GET /health HTTP/1.1\r\n").await.unwrap();
        let after = closed_after(&mut c, Duration::from_secs(4)).await;
        assert!(after < Duration::from_secs(3), "{with_tls}: {after:?}");

        // A whole head in time is answered, however slowly it came.
        let mut c = s.stream().await;
        for part in HEALTH.chunks(10) {
            c.write_all(part).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(response(&mut c).await.starts_with("HTTP/1.1 200"));
    }
}

#[tokio::test]
async fn a_request_head_over_its_size_is_refused() {
    for with_tls in [false, true] {
        let s = start(Limits::default(), with_tls).await;
        let head = |pad: usize| {
            format!(
                "GET /health HTTP/1.1\r\nhost: relevo\r\nx-pad: {}\r\n\r\n",
                "a".repeat(pad)
            )
        };
        // A head near the limit is answered.
        let mut c = s.stream().await;
        c.write_all(head(Limits::default().head_size - 200).as_bytes())
            .await
            .unwrap();
        let got = response(&mut c).await;
        assert!(got.starts_with("HTTP/1.1 200"), "{with_tls}: {got}");
        // One over it is refused, whole or still coming, without waiting for the rest.
        for whole in [true, false] {
            let mut c = s.stream().await;
            let mut text = head(Limits::default().head_size + 1024);
            if !whole {
                text.truncate(text.len() - 4);
            }
            let _ = c.write_all(text.as_bytes()).await;
            let got = response(&mut c).await;
            assert!(got.starts_with("HTTP/1.1 431"), "{with_tls} {whole}: {got}");
        }
    }
}

#[tokio::test]
async fn a_slow_body_is_dropped_after_the_body_timeout() {
    for with_tls in [false, true] {
        let s = start(short(), with_tls).await;
        let mut c = s.stream().await;
        let head = format!(
            "POST /api/send HTTP/1.1\r\nhost: relevo\r\nauthorization: Bearer {}\r\n\
             content-type: application/json\r\ncontent-length: 100\r\n\r\n{{\"to\":",
            s.token
        );
        c.write_all(head.as_bytes()).await.unwrap();
        let after = closed_after(&mut c, Duration::from_secs(4)).await;
        assert!(after >= Duration::from_millis(800), "{with_tls}: {after:?}");
    }
}

#[tokio::test]
async fn an_idle_connection_is_closed_after_the_idle_timeout() {
    for with_tls in [false, true] {
        let s = start(
            Limits {
                idle: Duration::from_secs(1),
                ..Limits::default()
            },
            with_tls,
        )
        .await;
        // Kept alive after its answer, then closed once idle for the idle time.
        let mut c = s.stream().await;
        c.write_all(HEALTH).await.unwrap();
        assert!(response(&mut c).await.starts_with("HTTP/1.1 200"));
        let after = closed_after(&mut c, Duration::from_secs(4)).await;
        assert!(after >= Duration::from_millis(800), "{with_tls}: {after:?}");
        assert!(after < Duration::from_secs(3), "{with_tls}: {after:?}");

        // A request in time keeps it: the idle time runs again from its answer.
        let mut c = s.stream().await;
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            c.write_all(HEALTH).await.unwrap();
            assert!(
                response(&mut c).await.starts_with("HTTP/1.1 200"),
                "{with_tls}"
            );
        }
    }
}

#[tokio::test]
async fn beyond_the_connection_cap_new_connections_wait() {
    for with_tls in [false, true] {
        let limits = Limits {
            connections: 2,
            ..Limits::default()
        };
        let s = Arc::new(start(limits, with_tls).await);
        let mut held = vec![];
        for _ in 0..2 {
            let mut c = s.stream().await;
            c.write_all(HEALTH).await.unwrap();
            assert!(response(&mut c).await.starts_with("HTTP/1.1 200"));
            held.push(c);
        }
        // A third waits (not refused): no answer while the two are open...
        let third = {
            let s = s.clone();
            tokio::spawn(async move {
                let mut c = s.stream().await;
                c.write_all(HEALTH).await.unwrap();
                response(&mut c).await
            })
        };
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!third.is_finished(), "{with_tls}: answered beyond the cap");
        // ...and its answer once one of them closes.
        drop(held.pop());
        let got = tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("answered once a connection closed")
            .unwrap();
        assert!(got.starts_with("HTTP/1.1 200"), "{with_tls}: {got}");
    }
}

/// Opens connections to `s` until one is answered, for at most `within`.
async fn answered_within(s: &Server, within: Duration) -> Box<dyn Stream> {
    let deadline = Instant::now() + within;
    loop {
        let mut c = s.stream().await;
        let _ = c.write_all(HEALTH).await;
        let mut buf = [0u8; 12];
        match tokio::time::timeout(Duration::from_secs(1), c.read(&mut buf)).await {
            Ok(Ok(12)) => {
                assert_eq!(&buf, b"HTTP/1.1 200");
                return c;
            }
            _ => assert!(Instant::now() < deadline, "no new connection taken"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn one_address_holds_at_most_its_share_of_connections() {
    // Loopback is not counted (see below): the clients must come from another address of this
    // machine.
    let Some(ip) = tls::network_addresses().into_iter().next() else {
        eprintln!("skipped: this machine has no address but loopback");
        return;
    };
    for with_tls in [false, true] {
        let limits = Limits {
            per_address: 2,
            ..Limits::default()
        };
        let s = start_on(ip, limits, with_tls).await;
        // Two connections, each surely taken (it answered).
        let mut held = vec![];
        for _ in 0..2 {
            let mut c = s.stream().await;
            c.write_all(HEALTH).await.unwrap();
            assert!(response(&mut c).await.starts_with("HTTP/1.1 200"));
            held.push(c);
        }
        // A third is closed without an answer.
        let mut third = TcpStream::connect(s.addr).await.unwrap();
        let _ = third.write_all(HEALTH).await;
        closed_after(&mut third, Duration::from_secs(2)).await;
        // Those held still work.
        held[0].write_all(HEALTH).await.unwrap();
        assert!(response(&mut held[0]).await.starts_with("HTTP/1.1 200"));

        // Once one goes, a new one is taken.
        drop(held.pop());
        answered_within(&s, Duration::from_secs(3)).await;
    }
}

/// Behind a proxy on this machine every connection comes from loopback: the whole team's.
#[tokio::test]
async fn loopback_is_not_held_to_one_addresss_share() {
    for with_tls in [false, true] {
        let limits = Limits {
            per_address: 2,
            ..Limits::default()
        };
        let s = start(limits, with_tls).await;
        let mut held = vec![];
        for _ in 0..5 {
            let mut c = s.stream().await;
            c.write_all(HEALTH).await.unwrap();
            assert!(
                response(&mut c).await.starts_with("HTTP/1.1 200"),
                "{with_tls}"
            );
            held.push(c);
        }
    }
}

#[tokio::test]
async fn a_waiting_inbox_over_tls_answers_in_time() {
    let s = start(Limits::default(), true).await;
    let client = s.client();
    let inbox = |wait: u64| {
        client
            .get(s.url(&format!("/api/inbox?wait={wait}")))
            .bearer_auth(&s.token)
            .send()
    };
    let began = Instant::now();
    let res = inbox(2).await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(began.elapsed() >= Duration::from_secs(2), "it waited");
    assert!(res.json::<Value>().await.unwrap()["threads"].is_array());

    // The longest wait the protocol allows (60 s) fits in a request (90 s).
    let l = Limits::default();
    assert!(l.request >= Duration::from_secs(60) + Duration::from_secs(10));

    // A request that takes longer than the request limit is cut off.
    let s = start(
        Limits {
            request: Duration::from_secs(1),
            ..Limits::default()
        },
        true,
    )
    .await;
    let client = s.client();
    let res = client
        .get(s.url("/api/inbox?wait=5"))
        .bearer_auth(&s.token)
        .send()
        .await;
    assert!(res.is_err(), "answered past the request limit");
}
