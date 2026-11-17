//! Bounded HTTP/1 serving for the sandbox's three processes (D-054, review N1).
//! **SIMULATED MONEY ONLY.**
//!
//! `axum::serve` builds hyper's connection without a timer, so hyper's header-read timeout
//! never runs, and nothing bounds idle keep-alive connections, the number of connections,
//! or a client that stops reading. [`serve`] replaces it:
//!
//! - **a connection cap before accept** ([`Limits::max_connections`]): past it, new
//!   connections wait in the kernel's listen backlog instead of taking descriptors;
//! - **a read timeout** ([`Limits::read_timeout`]): a request head must arrive within it
//!   (hyper restarts this timer whenever a keep-alive connection waits for its next request,
//!   so it is also the idle timeout), and a request body may not pause longer than it;
//! - **a write deadline** ([`Limits::write_timeout`]): a connection whose response write has
//!   made no progress for this long is closed;
//! - **per-connection limits** ([`Limits::max_requests`], [`Limits::max_age`]): past
//!   either, the next response carries `Connection: close`, so a client never reuses a
//!   connection the server is about to drop;
//! - **a bounded drain**: on shutdown, accepting stops, every connection is asked to close
//!   after its current request, and stragglers are cut off after the grace period.
//!
//! A client's pooled idle connections must be shorter-lived than the server's read timeout,
//! or a client may reuse one the server is closing (the sandbox's own clients keep theirs at
//! most [`CLIENT_POOL_IDLE`]).

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderValue, Request, header};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tower::ServiceExt as _;
use tower_http::timeout::RequestBodyTimeoutLayer;

/// Longest a sandbox client keeps an idle pooled connection: well under every server's
/// read timeout, so it never sends on a connection the server is closing.
pub const CLIENT_POOL_IDLE: Duration = Duration::from_secs(4);

/// The bounds of one listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Connections served at once; more wait in the listen backlog.
    pub max_connections: usize,
    /// Longest wait for a request head (also the idle keep-alive timeout) and longest pause
    /// inside a request body.
    pub read_timeout: Duration,
    /// Longest a response write may make no progress.
    pub write_timeout: Duration,
    /// Requests served on one connection before it is closed.
    pub max_requests: u32,
    /// Age after which a connection is closed after its next response.
    pub max_age: Duration,
}

impl Limits {
    /// The defaults with `max_connections` connections: 10 s read and write timeouts,
    /// 10,000 requests and 10 minutes per connection. Chosen, not measured (D-054).
    pub const fn with_connections(max_connections: usize) -> Self {
        Self {
            max_connections,
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            max_requests: 10_000,
            max_age: Duration::from_secs(600),
        }
    }

    /// Reads `<prefix>MAX_CONNECTIONS`, `<prefix>READ_TIMEOUT_MS`, `<prefix>WRITE_TIMEOUT_MS`,
    /// `<prefix>MAX_REQUESTS_PER_CONNECTION` and `<prefix>MAX_CONNECTION_AGE_MS` through
    /// `lookup`, over `defaults`. Every problem is reported.
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
        defaults: Self,
    ) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let mut number = |name: &str, default: u64| -> u64 {
            let key = format!("{prefix}{name}");
            match lookup(&key) {
                None => default,
                Some(raw) => match raw.parse::<u64>() {
                    Ok(0) | Err(_) => {
                        errors.push(format!("{key} must be a whole number of at least 1"));
                        default
                    }
                    Ok(value) => value,
                },
            }
        };
        let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let limits = Self {
            max_connections: usize::try_from(number(
                "MAX_CONNECTIONS",
                u64::try_from(defaults.max_connections).unwrap_or(u64::MAX),
            ))
            .unwrap_or(usize::MAX),
            read_timeout: Duration::from_millis(number(
                "READ_TIMEOUT_MS",
                millis(defaults.read_timeout),
            )),
            write_timeout: Duration::from_millis(number(
                "WRITE_TIMEOUT_MS",
                millis(defaults.write_timeout),
            )),
            max_requests: u32::try_from(number(
                "MAX_REQUESTS_PER_CONNECTION",
                u64::from(defaults.max_requests),
            ))
            .unwrap_or(u32::MAX),
            max_age: Duration::from_millis(number(
                "MAX_CONNECTION_AGE_MS",
                millis(defaults.max_age),
            )),
        };
        if errors.is_empty() {
            Ok(limits)
        } else {
            Err(errors)
        }
    }
}

/// How serving ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Every connection finished within the grace period.
    Graceful,
    /// The grace period elapsed with connections still open; they were cut off.
    DeadlineExceeded,
}

/// Serves `router` on `listener` within `limits` until `shutdown` resolves, then drains for
/// at most `grace`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    limits: Limits,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send,
) -> io::Result<ShutdownOutcome> {
    let service = tower::ServiceBuilder::new()
        .layer(RequestBodyTimeoutLayer::new(limits.read_timeout))
        .service(router);
    let permits = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    let (stop, stopping) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        // A place first: past the cap, connections wait in the backlog, unaccepted.
        let permit = tokio::select! {
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
            () = &mut shutdown => break,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    // EMFILE and friends: back off instead of spinning.
                    tracing::warn!(%error, "accept failed; retrying shortly");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        while connections.try_join_next().is_some() {}
        let _ = stream.set_nodelay(true);
        connections.spawn(connection(
            stream,
            service.clone(),
            limits,
            stopping.clone(),
            permit,
        ));
    }
    drop(listener);
    let _ = stop.send(true);
    let drained = tokio::time::timeout(grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_ok() {
        Ok(ShutdownOutcome::Graceful)
    } else {
        connections.abort_all();
        Ok(ShutdownOutcome::DeadlineExceeded)
    }
}

type BoundedService = tower_http::timeout::RequestBodyTimeout<Router>;

/// One accepted connection; its place is released when it ends.
async fn connection(
    stream: TcpStream,
    service: BoundedService,
    limits: Limits,
    stopping: watch::Receiver<bool>,
    permit: OwnedSemaphorePermit,
) {
    serve_connection(stream, service, limits, stopping).await;
    drop(permit);
}

/// Serves one connection over any byte stream (a `TcpStream` in production, an in-memory
/// pipe in tests) within `limits`.
pub(crate) async fn serve_connection<I>(
    io: I,
    service: BoundedService,
    limits: Limits,
    mut stopping: watch::Receiver<bool>,
) where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let born = Instant::now();
    let progress = Arc::new(WriteProgress::new(born));
    let watched = Watched {
        inner: io,
        progress: progress.clone(),
    };
    let served = Arc::new(AtomicU32::new(0));
    let expired = Arc::new(AtomicBool::new(false));
    let hyper_service = {
        let (served, expired) = (served.clone(), expired.clone());
        hyper::service::service_fn(move |request: Request<Incoming>| {
            let service = service.clone();
            let count = served.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            let last = count >= limits.max_requests || expired.load(Ordering::Relaxed);
            async move {
                let mut response = service.oneshot(request).await?;
                if last {
                    response
                        .headers_mut()
                        .insert(header::CONNECTION, HeaderValue::from_static("close"));
                }
                Ok::<_, Infallible>(response)
            }
        })
    };
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.read_timeout)
        .keep_alive(true);
    let conn = builder.serve_connection(TokioIo::new(watched), hyper_service);
    tokio::pin!(conn);
    let age = tokio::time::sleep(limits.max_age);
    tokio::pin!(age);
    let check = limits.write_timeout.min(Duration::from_millis(250)) / 2;
    let mut tick = tokio::time::interval(check.max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut closing = false;
    let mut aged = false;
    loop {
        tokio::select! {
            result = conn.as_mut() => {
                if let Err(error) = result {
                    tracing::debug!(%error, "connection ended with an error");
                }
                return;
            }
            () = &mut age, if !aged => {
                // Old: the next response says `Connection: close`. An idle one is closed
                // by the read timeout (hyper's head timer), and a request in progress is
                // bounded by the request and body deadlines.
                aged = true;
                expired.store(true, Ordering::Relaxed);
            }
            changed = stopping.changed(), if !closing => {
                let _ = changed;
                closing = true;
                conn.as_mut().graceful_shutdown();
            }
            _ = tick.tick() => {
                if progress.stalled_for(Instant::now()) > limits.write_timeout {
                    tracing::debug!("a response write made no progress within the write timeout; closing");
                    return;
                }
            }
        }
    }
}

/// When a write last made progress: a pending write records the first moment it waited.
#[derive(Debug)]
struct WriteProgress {
    born: Instant,
    /// Milliseconds since `born` at which the current write started waiting; 0 when none is.
    waiting_since: AtomicU64,
}

impl WriteProgress {
    fn new(born: Instant) -> Self {
        Self {
            born,
            waiting_since: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.born.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .max(1)
    }

    fn pending(&self) {
        let now = self.now_ms();
        let _ = self
            .waiting_since
            .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn progressed(&self) {
        self.waiting_since.store(0, Ordering::Relaxed);
    }

    fn stalled_for(&self, now: Instant) -> Duration {
        match self.waiting_since.load(Ordering::Relaxed) {
            0 => Duration::ZERO,
            since => {
                let now = u64::try_from(now.saturating_duration_since(self.born).as_millis())
                    .unwrap_or(u64::MAX);
                Duration::from_millis(now.saturating_sub(since))
            }
        }
    }
}

/// A byte stream that reports whether its writes are making progress.
struct Watched<I> {
    inner: I,
    progress: Arc<WriteProgress>,
}

impl<I> Watched<I> {
    fn track<T>(&self, poll: Poll<io::Result<T>>) -> Poll<io::Result<T>> {
        match &poll {
            Poll::Pending => self.progress.pending(),
            Poll::Ready(_) => self.progress.progressed(),
        }
        poll
    }
}

impl<I: AsyncRead + Unpin> AsyncRead for Watched<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for Watched<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.track(poll)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = Pin::new(&mut self.inner).poll_flush(cx);
        self.track(poll)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.track(poll)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.track(poll)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn big() -> Router {
        Router::new().route("/big", get(|| async { "x".repeat(256 * 1024) }))
    }

    /// A client that sends a request and never reads the answer cannot hold the connection
    /// (and its place) past the write timeout.
    #[tokio::test]
    async fn a_client_that_stops_reading_is_cut_off_at_the_write_timeout() {
        let (mut client, server) = tokio::io::duplex(1024);
        let limits = Limits {
            write_timeout: Duration::from_millis(300),
            ..Limits::with_connections(1)
        };
        let (_stop, stopping) = watch::channel(false);
        let service = tower::ServiceBuilder::new()
            .layer(RequestBodyTimeoutLayer::new(limits.read_timeout))
            .service(big());
        let connection = tokio::spawn(serve_connection(server, service, limits, stopping));
        client
            .write_all(b"GET /big HTTP/1.1\r\nHost: t\r\n\r\n")
            .await
            .unwrap();
        // Never read: the 256 KiB answer cannot fit the 1 KiB pipe.
        let ended = tokio::time::timeout(Duration::from_secs(5), connection).await;
        assert!(ended.is_ok(), "the stalled connection was never cut off");
        // Draining now finds a truncated answer, then the end.
        let mut seen = Vec::new();
        let _ = client.read_to_end(&mut seen).await;
        assert!(seen.len() < 256 * 1024);
    }

    #[test]
    fn limits_are_read_and_validated() {
        let vars = [("X_MAX_CONNECTIONS", "8"), ("X_READ_TIMEOUT_MS", "500")];
        let lookup = |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        };
        let limits = Limits::from_lookup("X_", lookup, Limits::with_connections(64)).unwrap();
        assert_eq!(limits.max_connections, 8);
        assert_eq!(limits.read_timeout, Duration::from_millis(500));
        assert_eq!(limits.max_requests, 10_000);
        let bad = [("X_WRITE_TIMEOUT_MS", "0"), ("X_MAX_CONNECTIONS", "many")];
        let lookup = |k: &str| {
            bad.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        };
        assert_eq!(
            Limits::from_lookup("X_", lookup, Limits::with_connections(64))
                .unwrap_err()
                .len(),
            2
        );
    }
}
