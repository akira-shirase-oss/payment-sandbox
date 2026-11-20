//! The Tower stack. `ServiceBuilder` lists layers outermost first: a request travels down
//! the list, its response back up (rust-mastery chapter 12, "Middleware with Tower").

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::{self as axum_mw, Next},
    response::{IntoResponse, Response},
};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower_http::{
    LatencyUnit,
    catch_panic::CatchPanicLayer,
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, Span};

use crate::config::Config;
use crate::error::{self, ApiError, PublicError};

/// Request id header, both directions.
pub const REQUEST_ID_HEADER: &str = "x-request-id";
/// Longest client request id kept.
pub const MAX_REQUEST_ID_LEN: usize = 64;
/// Every response says what it is (D-009, D-025).
pub const SIMULATED_HEADER: HeaderName = HeaderName::from_static("sandbox-simulated");

/// UUIDv7 request ids.
#[derive(Debug, Clone, Copy, Default)]
pub struct MakeRequestUuidV7;

impl MakeRequestId for MakeRequestUuidV7 {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        HeaderValue::from_str(&uuid::Uuid::now_v7().to_string())
            .ok()
            .map(RequestId::new)
    }
}

/// True for client ids safe to copy into logs and back onto responses: canonical UUIDs only
/// (review finding 8). Free-form ids let a card number reach logs and response headers.
pub fn is_acceptable_request_id(value: &str) -> bool {
    // A UUID made only of digits is card-like (review N7): one letter at least.
    value.len() == 36
        && uuid::Uuid::try_parse(value).is_ok()
        && value.bytes().any(|b| b.is_ascii_alphabetic())
}

async fn sanitize_request_id(mut request: Request) -> Request {
    let acceptable = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .map(|value| value.to_str().is_ok_and(is_acceptable_request_id));
    if acceptable == Some(false) {
        request.headers_mut().remove(REQUEST_ID_HEADER);
    }
    request
}

/// Root span: method, matched route template (low cardinality) and request id. Never the
/// concrete path: a path segment can carry anything a client typed (review finding 8).
fn make_span(request: &Request<Body>) -> Span {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", MatchedPath::as_str);
    tracing::info_span!(
        "request",
        method = %request.method(),
        route,
        request_id = %request_id,
    )
}

/// Renders every 4xx/5xx leaving the server as the envelope, with the request id.
async fn error_envelope(request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let public = match response.extensions().get::<PublicError>() {
        Some(public) => public.clone(),
        None if error::is_json(response.headers()) => return response,
        None => PublicError::from_status(status),
    };
    let (mut parts, _discarded) = response.into_parts();
    let rendered = error::render(status, &public, request_id.as_deref());
    parts.headers.remove(header::CONTENT_LENGTH);
    let (rendered_parts, body) = rendered.into_parts();
    parts.headers.extend(rendered_parts.headers);
    parts.extensions.extend(rendered_parts.extensions);
    Response::from_parts(parts, body)
}

/// Refuses a request whose query string, percent-decoded, carries a card number (review N3,
/// D-056): bodies, keys, paths and request ids were guarded, query strings were not.
async fn refuse_card_query(request: Request, next: Next) -> Response {
    if let Some(query) = request.uri().query()
        && sandbox_domain::card_guard::contains_pan_like(&percent_decode(query))
    {
        return ApiError::RawCardData.into_response();
    }
    next.run(request).await
}

/// `%XX` and `+` decoded (invalid escapes kept as they are), as text.
fn percent_decode(query: &str) -> String {
    let bytes = query.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A caught panic becomes a 500 envelope; the payload is logged, never returned.
fn panic_response(payload: Box<dyn Any + Send + 'static>) -> Response {
    let message = match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    };
    ApiError::Internal(anyhow::anyhow!("handler panicked: {message}")).into_response()
}

/// Request-admission budgets (D-099): non-read requests share `max_in_flight` and are
/// shed at once; `GET`/`HEAD` get their own permits and may wait `read_wait` for one —
/// an overloaded write path can neither hold a read's permit nor make a read wait on a
/// write's. A read refused anyway is a fast `Overloaded`, never a quiet queue.
#[derive(Debug)]
pub struct LoadGate {
    write: Arc<Semaphore>,
    read: Arc<Semaphore>,
    read_wait: Duration,
}

impl LoadGate {
    /// `write`/`read` in-flight permits; a read waits at most `read_wait` for its own.
    pub fn new(write: usize, read: usize, read_wait: Duration) -> Self {
        Self {
            write: Arc::new(Semaphore::new(write)),
            read: Arc::new(Semaphore::new(read)),
            read_wait,
        }
    }

    /// The write budget, for tests that fill it to observe a shed.
    pub fn write(&self) -> &Arc<Semaphore> {
        &self.write
    }

    /// The read budget, for tests that fill it to observe the bounded read wait.
    pub fn read(&self) -> &Arc<Semaphore> {
        &self.read
    }
}

/// True when `method` is a read — `GET`/`HEAD`. One definition shared by request
/// admission (D-099), pool classification (D-101) and `AppState::db_for` (D-100), so
/// the layers always agree. Everything else is a write; a read-only `POST` exists only
/// through [`post_read`], whose [`ReadOnly`] marker is the compile-checked opt-in.
pub fn is_read_method(method: &Method) -> bool {
    matches!(method, &Method::GET | &Method::HEAD)
}

/// The pool this request's database work must use (D-101), chosen once by
/// [`classify_pool`] — handlers extract `Extension<DbPool>` and never name a pool
/// themselves, so the method classification cannot drift per handler.
#[derive(Debug, Clone)]
pub struct DbPool(pub sqlx::PgPool);

/// The marker a read-only `POST` route carries (D-101): naming the type is the
/// compile-checked opt-in. Inserted by [`mark_read`] via [`post_read`] only.
#[derive(Debug, Clone, Copy)]
pub struct ReadOnly;

/// Chooses the request's pool once, in this one place, by the same [`is_read_method`]
/// the load gate applies to admission (D-101): a `GET`/`HEAD` draws `read_db`, anything
/// else `write_db`. Runs on the same `route_layer` as [`shed_load`] so every `/v1`
/// request — and every `/v1` auth lookup — carries `DbPool` before extraction.
pub async fn classify_pool(
    State(state): State<crate::state::AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let pool = if is_read_method(request.method()) {
        state.read_db.clone()
    } else {
        state.write_db.clone()
    };
    request.extensions_mut().insert(DbPool(pool));
    next.run(request).await
}

/// Marks the request read-only (D-101): overwrites `DbPool` with the read pool after
/// [`classify_pool`] classified the `POST` as a write, and inserts [`ReadOnly`] so the
/// opt-in is visible to anything that inspects the request.
async fn mark_read(
    State(state): State<crate::state::AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(ReadOnly);
    request
        .extensions_mut()
        .insert(DbPool(state.read_db.clone()));
    next.run(request).await
}

/// Registers a read-only `POST` route — the only way a `POST` reaches the read pool
/// (D-101, Colophon's D-225 rule): a handler that wants `read_db` for a `POST` must be
/// registered through here, which is a statement the compiler checks (`post(handler)`
/// cannot produce the marker). Admission still pays the write-side budget — a `POST`
/// body is shaped like a write — while its database work draws `read_db`.
///
/// No read-only `POST` exists today; the route exists so adding one is a deliberate,
/// reviewable act rather than an accident of which pool name a handler typed.
pub fn post_read<H, T>(
    state: &crate::state::AppState,
    handler: H,
) -> axum::routing::MethodRouter<crate::state::AppState>
where
    H: axum::handler::Handler<T, crate::state::AppState>,
    T: 'static,
{
    axum::routing::post(handler).route_layer(axum_mw::from_fn_with_state(state.clone(), mark_read))
}

/// Rejects at once when `max_in_flight` `/v1` requests are running, instead of queueing
/// until every request times out.
pub async fn shed_load(
    State(gate): State<Arc<LoadGate>>,
    request: Request,
    next: Next,
) -> Response {
    let permit = if is_read_method(request.method()) {
        match tokio::time::timeout(gate.read_wait, gate.read.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            // The gate is never closed: a miss is the deadline, i.e. overloaded.
            _ => return ApiError::Overloaded.into_response(),
        }
    } else {
        match gate.write.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return ApiError::Overloaded.into_response(),
        }
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

/// `503` rather than `408` when the deadline elapses: the client was not slow, we were.
pub const TIMEOUT_STATUS: StatusCode = StatusCode::SERVICE_UNAVAILABLE;

/// Applies the global stack with the hot path's request deadline.
pub fn apply(router: Router, config: &Config) -> Router {
    apply_with_timeout(router, config, config.request_timeout)
}

/// Applies the global stack with a specific request deadline (reports scan a whole
/// dataset and get a longer, still bounded, one).
pub fn apply_with_timeout(router: Router, config: &Config, timeout: std::time::Duration) -> Router {
    let stack = ServiceBuilder::new()
        // 0. Authorization and the idempotency key are redacted from any Debug output.
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            HeaderName::from_static("idempotency-key"),
        ]))
        // 1. Drop unsafe client ids before anything logs them.
        .layer(axum_mw::map_request(sanitize_request_id))
        // 2. Assign an id when absent.
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuidV7))
        // 3. Copy it onto every response, including those generated below.
        .layer(PropagateRequestIdLayer::x_request_id())
        // 4. Label every response as simulated.
        .layer(SetResponseHeaderLayer::overriding(
            SIMULATED_HEADER,
            HeaderValue::from_static("true"),
        ))
        // 5. Root span, which sees the id.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_span)
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::DEBUG)
                        .latency_unit(LatencyUnit::Micros),
                ),
        )
        // 6. One error envelope for everything below.
        .layer(axum_mw::from_fn(error_envelope))
        // 7. Panics become 500 envelopes.
        .layer(CatchPanicLayer::custom(panic_response))
        // 7b. Card numbers in query strings are refused like everywhere else.
        .layer(axum_mw::from_fn(refuse_card_query))
        // 8. Deadline for response headers.
        .layer(TimeoutLayer::with_status_code(TIMEOUT_STATUS, timeout))
        // 9–10. Body limits, by Content-Length up front and while streaming.
        .layer(RequestBodyLimitLayer::new(config.max_body_bytes))
        .layer(DefaultBodyLimit::max(config.max_body_bytes));
    router.layer(stack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_policy() {
        for (input, expected) in [
            ("018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01", true),
            ("trace.42_a", false),
            ("4111111111111111", false),
            // Review N7: an all-digit UUID passes the card guard's UUID exemption.
            ("42424242-4242-4242-4242-424242424242", false),
            ("", false),
            ("has space", false),
            ("new\nline", false),
            (&*"a".repeat(65), false),
        ] {
            assert_eq!(is_acceptable_request_id(input), expected, "{input:?}");
        }
    }

    #[test]
    fn queries_are_percent_decoded_before_the_card_guard() {
        assert_eq!(percent_decode("a=4242%204242+42%2"), "a=4242 4242 42%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert!(sandbox_domain::card_guard::contains_pan_like(
            &percent_decode("d=4242%2E4242%2E4242%2E4242")
        ));
    }

    /// D-099: a write flood can hold every write permit and a read still answers from
    /// its own budget; a read with no read permit left waits `read_wait`, then `503`s.
    #[tokio::test(start_paused = true)]
    async fn reads_have_their_own_budget_when_writes_saturate() {
        use axum::routing::get;
        use tower::ServiceExt as _;
        let gate = Arc::new(LoadGate::new(1, 1, Duration::from_millis(100)));
        let router = || {
            Router::new()
                .route("/", get(|| async { "ok" }).post(|| async { "ok" }))
                .route_layer(axum_mw::from_fn_with_state(gate.clone(), shed_load))
        };
        let held_write = gate.write.clone().try_acquire_owned().expect("permit");
        let refused = router()
            .oneshot(Request::post("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        let answered = router()
            .oneshot(Request::get("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(answered.status(), StatusCode::OK);
        drop(held_write);

        // Exhaust the read budget too: the next read waits `read_wait`, then a fast 503.
        let held_read = gate.read.clone().try_acquire_owned().expect("permit");
        let started = tokio::time::Instant::now();
        let refused = router()
            .oneshot(Request::get("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(started.elapsed(), Duration::from_millis(100));
        drop(held_read);
    }

    #[test]
    fn panic_payloads_become_500s() {
        for payload in [
            Box::new("static") as Box<dyn Any + Send>,
            Box::new(String::from("owned")),
            Box::new(7_u8),
        ] {
            assert_eq!(
                panic_response(payload).status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
        }
    }
}
