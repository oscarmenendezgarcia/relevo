//! The server's transport: HTTP/1.1 over TCP, plain or with TLS (rustls), with the limits of
//! `docs/protocol.md` on what one client can hold: time for the request head, the body, an idle
//! connection and the whole request, and connections at once, in all and per address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};

use http_body::{Body as _, Frame, SizeHint};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tower::ServiceExt;

/// What one client can hold.
#[derive(Debug, Clone)]
pub struct Limits {
    /// To send the request head (with the TLS handshake, on a new connection).
    pub head: Duration,
    /// To send the body, once the head is in.
    pub body: Duration,
    /// A connection with no request in it is closed after this.
    pub idle: Duration,
    /// For the whole request, from its head to its answer (a waiting `inbox` answers by 60 s).
    pub request: Duration,
    /// Connections at once; beyond, new ones wait.
    pub connections: usize,
    /// Connections at once from one address; beyond, new ones are refused.
    pub per_address: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            head: Duration::from_secs(10),
            body: Duration::from_secs(30),
            idle: Duration::from_secs(75),
            request: Duration::from_secs(90),
            connections: 1024,
            per_address: 64,
        }
    }
}

/// Serves `router` on `listener` until the future is dropped: over TLS with `tls`, else plain.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    tls: Option<Arc<rustls::ServerConfig>>,
    limits: Limits,
) {
    let limits = Arc::new(limits);
    let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
    let open = Arc::new(Semaphore::new(limits.connections));
    let per_address = Arc::new(Mutex::new(HashMap::new()));
    loop {
        // Taken before accepting: at the limit, new connections wait in the listen queue.
        let permit = open.clone().acquire_owned().await.expect("never closed");
        let (tcp, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                // Out of file descriptors, most likely: let some connections end.
                tracing::warn!(error = %e, "accepting a connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Some(slot) = AddressSlot::take(&per_address, peer.ip(), limits.per_address) else {
            tracing::debug!(peer = %peer.ip(), "too many connections from one address");
            continue;
        };
        let _ = tcp.set_nodelay(true);
        let (router, limits, acceptor) = (router.clone(), limits.clone(), acceptor.clone());
        tokio::spawn(async move {
            connection(tcp, acceptor, router, limits).await;
            drop((permit, slot));
        });
    }
}

/// One connection's place among those of its address.
struct AddressSlot {
    all: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ip: IpAddr,
}

impl AddressSlot {
    fn take(all: &Arc<Mutex<HashMap<IpAddr, usize>>>, ip: IpAddr, max: usize) -> Option<Self> {
        let mut map = all.lock().unwrap_or_else(|e| e.into_inner());
        let held = map.entry(ip).or_default();
        if *held >= max {
            return None;
        }
        *held += 1;
        Some(AddressSlot {
            all: all.clone(),
            ip,
        })
    }
}

impl Drop for AddressSlot {
    fn drop(&mut self) {
        let mut map = self.all.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(held) = map.get_mut(&self.ip) {
            *held -= 1;
            if *held == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

async fn connection(
    tcp: TcpStream,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    router: Router,
    limits: Arc<Limits>,
) {
    // The head's time runs from the connection, the TLS handshake included.
    let watch = Arc::new(Watch(Mutex::new(Phase::Head(Instant::now() + limits.head))));
    match acceptor {
        None => http(tcp, router, limits, watch).await,
        Some(acceptor) => {
            let shake = tokio::time::timeout_at(watch.deadline(), acceptor.accept(tcp)).await;
            if let Ok(Ok(tls)) = shake {
                http(tls, router, limits, watch).await;
            }
        }
    }
}

/// Serves HTTP/1.1 on `io` until the client closes it or misses a deadline of `watch`.
async fn http<S>(io: S, router: Router, limits: Arc<Limits>, watch: Arc<Watch>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(Watched {
        io,
        watch: watch.clone(),
        head: limits.head,
    });
    let in_service = watch.clone();
    let service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
        let (router, limits, watch) = (router.clone(), limits.clone(), in_service.clone());
        async move {
            watch.busy(req.body().is_end_stream(), &limits);
            let req = req.map(|body| {
                Body::new(Timed {
                    body,
                    watch: watch.clone(),
                })
            });
            let res = router.oneshot(req).await;
            // Writing the answer counts as idle: a client that does not read it is closed too.
            watch.set(Phase::Idle(Instant::now() + limits.idle));
            res
        }
    });
    let conn = http1::Builder::new().serve_connection(io, service);
    tokio::pin!(conn);
    loop {
        let deadline = watch.deadline();
        tokio::select! {
            _ = conn.as_mut() => return,
            _ = tokio::time::sleep_until(deadline) => {
                // The deadline may have moved while this slept.
                if watch.deadline() <= Instant::now() {
                    return;
                }
            }
        }
    }
}

/// Where a connection is, and until when it may stay there.
#[derive(Clone, Copy)]
enum Phase {
    /// Waiting for a request head.
    Head(Instant),
    /// A request is in, its body still coming (until `body`); the answer is due by `answer`.
    Body { body: Instant, answer: Instant },
    /// A request is in, with its body; its answer is due by then.
    Answer(Instant),
    /// No request in it.
    Idle(Instant),
}

struct Watch(Mutex<Phase>);

impl Watch {
    fn get(&self) -> Phase {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set(&self, phase: Phase) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = phase;
    }

    fn deadline(&self) -> Instant {
        match self.get() {
            Phase::Head(t) | Phase::Answer(t) | Phase::Idle(t) => t,
            Phase::Body { body, .. } => body,
        }
    }

    /// A request head is in.
    fn busy(&self, body_done: bool, limits: &Limits) {
        let now = Instant::now();
        let answer = now + limits.request;
        self.set(if body_done {
            Phase::Answer(answer)
        } else {
            Phase::Body {
                body: now + limits.body,
                answer,
            }
        });
    }

    /// Bytes came: on an idle connection, they start a request head.
    fn read(&self, head: Duration) {
        let mut phase = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Phase::Idle(_) = *phase {
            *phase = Phase::Head(Instant::now() + head);
        }
    }

    /// The request's body is all in.
    fn body_done(&self) {
        let mut phase = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Phase::Body { answer, .. } = *phase {
            *phase = Phase::Answer(answer);
        }
    }
}

/// The connection, telling its watch when bytes come.
struct Watched<S> {
    io: S,
    watch: Arc<Watch>,
    head: Duration,
}

impl<S: AsyncRead + Unpin> AsyncRead for Watched<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let res = Pin::new(&mut this.io).poll_read(cx, buf);
        if buf.filled().len() > before {
            this.watch.read(this.head);
        }
        res
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Watched<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

/// A request body, telling its watch when it is all in.
struct Timed {
    body: Incoming,
    watch: Arc<Watch>,
}

impl http_body::Body for Timed {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(frame, Poll::Ready(None)) || this.body.is_end_stream() {
            this.watch.body_done();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limits_are_the_protocols() {
        let l = Limits::default();
        let secs = |d: Duration| d.as_secs();
        assert_eq!(
            (secs(l.head), secs(l.body), secs(l.idle), secs(l.request)),
            (10, 30, 75, 90)
        );
        assert_eq!((l.connections, l.per_address), (1024, 64));
        // A waiting inbox (at most 60 s) fits in a request.
        assert!(l.request > Duration::from_secs(60));
    }
}
