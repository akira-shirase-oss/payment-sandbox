//! Bounded HTTP/1 serving for the sandbox's three processes (D-054, review N1).
//! **SIMULATED MONEY ONLY.**
//!
//! `axum::serve` builds hyper's connection without a timer, so hyper's header-read timeout
//! never runs, and nothing bounds idle keep-alive connections, the number of connections,
//! or a client that stops reading. [`serve`] replaces it:
//!
//! - **a connection cap with a bounded wait** ([`Limits::max_connections`],
//!   [`Limits::accept_wait`], [`Limits::max_pending`]): every connection is accepted, and
//!   past the cap it waits for a serving place at most [`Limits::accept_wait`] while at
//!   most [`Limits::max_pending`] connections wait; otherwise it is refused with `503`
//!   and `Retry-After` and closed — a request never waits in the kernel's listen backlog
//!   until the client gives up (D-094);
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
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, ReadBuf};
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
    /// Connections served at once.
    pub max_connections: usize,
    /// Longest a connection past the cap waits for a serving place before it is refused.
    pub accept_wait: Duration,
    /// Connections waiting for a place at once; the rest are refused at once.
    pub max_pending: usize,
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
    /// The defaults with `max_connections` connections: a 2 s bounded wait for a place
    /// with as many waiting connections as the cap, 10 s read and write timeouts,
    /// 10,000 requests and 10 minutes per connection. Chosen, not measured (D-054; the
    /// wait's bound is D-094).
    pub const fn with_connections(max_connections: usize) -> Self {
        Self {
            max_connections,
            accept_wait: Duration::from_secs(2),
            max_pending: max_connections,
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            max_requests: 10_000,
            max_age: Duration::from_secs(600),
        }
    }

    /// Reads `<prefix>MAX_CONNECTIONS`, `<prefix>ACCEPT_WAIT_MS`, `<prefix>MAX_PENDING`,
    /// `<prefix>READ_TIMEOUT_MS`, `<prefix>WRITE_TIMEOUT_MS`,
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
            accept_wait: Duration::from_millis(number(
                "ACCEPT_WAIT_MS",
                millis(defaults.accept_wait),
            )),
            max_pending: usize::try_from(number(
                "MAX_PENDING",
                u64::try_from(defaults.max_pending).unwrap_or(u64::MAX),
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

/// What a bounded listener has seen (D-093): counters an operator can scrape. Every value
/// is approximate telemetry — atomics updated without coordination, never per-request data.
#[derive(Debug)]
pub struct ServeStats {
    accepted_total: AtomicU64,
    refused_total: AtomicU64,
    served_total: AtomicU64,
    active: AtomicU64,
    active_peak: AtomicU64,
    place_wait_ns_total: AtomicU64,
    place_wait_ns_max: AtomicU64,
    born: std::time::Instant,
    last_warn_ms: AtomicU64,
}

/// One reading of [`ServeStats`]; milliseconds where the counters are durations.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServeStatsSnapshot {
    /// TCP connections accepted.
    pub accepted: u64,
    /// Connections refused over the cap (after `accept_wait`, or over `max_pending` at once).
    pub refused: u64,
    /// Connections that won a serving place.
    pub served: u64,
    /// Serving places held right now.
    pub active: u64,
    /// Most serving places ever held at once.
    pub active_peak: u64,
    /// Total time accepted connections spent waiting for a place.
    pub place_wait_ms_total: u64,
    /// Longest single wait for a place.
    pub place_wait_ms_max: u64,
}

/// At most one cap warning this often.
const WARN_EVERY: Duration = Duration::from_secs(10);

impl Default for ServeStats {
    fn default() -> Self {
        Self {
            accepted_total: AtomicU64::new(0),
            refused_total: AtomicU64::new(0),
            served_total: AtomicU64::new(0),
            active: AtomicU64::new(0),
            active_peak: AtomicU64::new(0),
            place_wait_ns_total: AtomicU64::new(0),
            place_wait_ns_max: AtomicU64::new(0),
            born: std::time::Instant::now(),
            last_warn_ms: AtomicU64::new(0),
        }
    }
}

impl ServeStats {
    fn accepted(&self) {
        self.accepted_total.fetch_add(1, Ordering::Relaxed);
    }

    fn served(&self) {
        self.served_total.fetch_add(1, Ordering::Relaxed);
        let active = self
            .active
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.active_peak.fetch_max(active, Ordering::Relaxed);
    }

    fn released(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    fn waited(&self, wait: Duration) {
        let nanos = u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX);
        self.place_wait_ns_total.fetch_add(nanos, Ordering::Relaxed);
        self.place_wait_ns_max.fetch_max(nanos, Ordering::Relaxed);
    }

    /// Counts one refusal; true when a warning is due (at most one per [`WARN_EVERY`]).
    fn refused(&self) -> bool {
        self.refused_total.fetch_add(1, Ordering::Relaxed);
        let now = u64::try_from(self.born.elapsed().as_millis()).unwrap_or(u64::MAX);
        let warn_ms = u64::try_from(WARN_EVERY.as_millis()).unwrap_or(u64::MAX);
        let last = self.last_warn_ms.load(Ordering::Relaxed);
        now.saturating_sub(last) >= warn_ms
            && self
                .last_warn_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    /// One reading of the counters.
    pub fn snapshot(&self) -> ServeStatsSnapshot {
        let ms = |nanos: u64| nanos / 1_000_000;
        ServeStatsSnapshot {
            accepted: self.accepted_total.load(Ordering::Relaxed),
            refused: self.refused_total.load(Ordering::Relaxed),
            served: self.served_total.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
            active_peak: self.active_peak.load(Ordering::Relaxed),
            place_wait_ms_total: ms(self.place_wait_ns_total.load(Ordering::Relaxed)),
            place_wait_ms_max: ms(self.place_wait_ns_max.load(Ordering::Relaxed)),
        }
    }
}

/// Serves `router` on `listener` within `limits` until `shutdown` resolves, then drains for
/// at most `grace`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    limits: Limits,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send,
    stats: Arc<ServeStats>,
) -> io::Result<ShutdownOutcome> {
    let service = tower::ServiceBuilder::new()
        .layer(RequestBodyTimeoutLayer::new(limits.read_timeout))
        .service(router);
    let permits = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    let pending = Arc::new(Semaphore::new(limits.max_pending.max(1)));
    let (stop, stopping) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        // Every offer is accepted at once (D-094): a connection past the cap waits for a
        // serving place as a task — bounded in count by `pending` and in time by
        // `accept_wait` — instead of sitting in the kernel's listen backlog, where nothing
        // bounded how long a request waited before it was ever looked at.
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
        stats.accepted();
        connections.spawn(wait_for_place(
            stream,
            service.clone(),
            limits,
            stopping.clone(),
            permits.clone(),
            pending.clone(),
            stats.clone(),
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

/// A connection's path to a serving place: at most `max_pending` connections may wait for
/// one, each at most `accept_wait`; then the refusal is a bounded, honest `503` (D-094).
async fn wait_for_place(
    stream: TcpStream,
    service: BoundedService,
    limits: Limits,
    stopping: watch::Receiver<bool>,
    permits: Arc<Semaphore>,
    pending: Arc<Semaphore>,
    stats: Arc<ServeStats>,
) {
    let Ok(waiting) = pending.try_acquire_owned() else {
        refuse(stream, limits, &stats).await;
        return;
    };
    let started = Instant::now();
    if let Ok(Ok(permit)) = tokio::time::timeout(limits.accept_wait, permits.acquire_owned()).await
    {
        stats.waited(started.elapsed());
        // The waiting place is freed; the serving place is the permit's.
        drop(waiting);
        connection(stream, service, limits, stopping, permit, stats).await;
    } else {
        stats.waited(started.elapsed());
        refuse(stream, limits, &stats).await;
    }
}

/// One bounded, honest refusal: `503`, `Retry-After` and `Connection: close`.
async fn refuse(mut stream: TcpStream, limits: Limits, stats: &Arc<ServeStats>) {
    if stats.refused() {
        tracing::warn!(
            cap = limits.max_connections,
            pending = limits.max_pending,
            "the connection cap is full; refusing the excess"
        );
    }
    let _ = tokio::time::timeout(limits.write_timeout, stream.write_all(OVER_CAP_RESPONSE)).await;
}

/// What a refused connection reads.
const OVER_CAP_RESPONSE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\nretry-after: 1\r\nsandbox-simulated: true\r\n\r\n";

/// One accepted connection; its place is released when it ends.
async fn connection(
    stream: TcpStream,
    service: BoundedService,
    limits: Limits,
    stopping: watch::Receiver<bool>,
    permit: OwnedSemaphorePermit,
    stats: Arc<ServeStats>,
) {
    stats.served();
    serve_connection(stream, service, limits, stopping).await;
    stats.released();
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
    use tokio::io::AsyncReadExt as _;

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
        let bad = [
            ("X_WRITE_TIMEOUT_MS", "0"),
            ("X_MAX_CONNECTIONS", "many"),
            ("X_ACCEPT_WAIT_MS", "0"),
        ];
        let lookup = |k: &str| {
            bad.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        };
        assert_eq!(
            Limits::from_lookup("X_", lookup, Limits::with_connections(64))
                .unwrap_err()
                .len(),
            3
        );
    }

    /// A connection past the cap is refused within `accept_wait`, not left waiting for the
    /// client to give up (D-094): the first GitHub full runs' 60 s tails were requests
    /// queued in the kernel's listen backlog until k6's own deadline.
    #[tokio::test]
    async fn a_connection_past_the_cap_is_refused_with_a_bounded_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let limits = Limits {
            max_connections: 1,
            accept_wait: Duration::from_millis(150),
            max_pending: 1,
            ..Limits::with_connections(1)
        };
        let stats = Arc::new(ServeStats::default());
        let router = Router::new().route("/", get(|| async { "ok" }));
        let (_stop, stopping) = watch::channel(false);
        let serving = {
            let stats = stats.clone();
            tokio::spawn(async move {
                serve(
                    listener,
                    router,
                    limits,
                    Duration::from_millis(200),
                    async move {
                        let mut rx = stopping;
                        let _ = rx.wait_for(|s| *s).await;
                    },
                    stats,
                )
                .await
            })
        };
        // The one place is held by an idle keep-alive connection.
        let holder = TcpStream::connect(addr).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while stats.snapshot().active == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(stats.snapshot().active, 1);

        // The next connection waits for the place only until `accept_wait`, then reads a
        // bounded 503 — before the fix it waited in the backlog until the client gave up.
        // Nothing is sent: the refusal is at the connection level, not the request's.
        let mut second = TcpStream::connect(addr).await.unwrap();
        let started = std::time::Instant::now();
        let mut answer = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(3), second.read_to_end(&mut answer)).await;
        let answered = read
            .expect("the over-cap connection was never refused")
            .expect("reading the refusal failed");
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(2),
            "the refusal took {waited:?}"
        );
        assert!(answered > 0);
        let text = String::from_utf8_lossy(&answer);
        assert!(text.starts_with("HTTP/1.1 503"), "{text}");
        assert!(text.contains("retry-after"), "{text}");

        // A third connection, over the pending bound, is refused at once.
        let mut third = TcpStream::connect(addr).await.unwrap();
        let mut early = Vec::new();
        let answer_at = tokio::time::timeout(Duration::from_secs(3), third.read_to_end(&mut early))
            .await
            .expect("the over-pending connection was never refused")
            .expect("reading the refusal failed");
        assert!(answer_at > 0);
        assert!(String::from_utf8_lossy(&early).starts_with("HTTP/1.1 503"));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.refused, 2, "{snapshot:?}");
        assert_eq!(snapshot.served, 1, "{snapshot:?}");
        drop(holder);
        drop(serving);
    }
}
