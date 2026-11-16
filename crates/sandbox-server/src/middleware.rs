//! The Tower stack. `ServiceBuilder` lists layers outermost first: a request travels down
//! the list, its response back up (rust-mastery chapter 12, "Middleware with Tower").

use std::any::Any;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
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

/// True for client ids safe to copy into logs: 1–64 of `[A-Za-z0-9._-]`.
pub fn is_acceptable_request_id(value: &str) -> bool {
    (1..=MAX_REQUEST_ID_LEN).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
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

/// Root span: method, matched route (low cardinality), path (never the query string), id.
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
        path = %request.uri().path(),
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

/// Rejects at once when `max_in_flight` `/v1` requests are running, instead of queueing
/// until every request times out.
pub async fn shed_load(
    State(gate): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(permit) = gate.try_acquire() else {
        return ApiError::Overloaded.into_response();
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
            ("trace.42_a", true),
            ("", false),
            ("has space", false),
            ("new\nline", false),
            (&*"a".repeat(65), false),
        ] {
            assert_eq!(is_acceptable_request_id(input), expected, "{input:?}");
        }
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
