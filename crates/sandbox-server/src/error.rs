//! One typed error and one JSON envelope for every error the server emits, including those
//! produced by middleware:
//!
//! ```json
//! {"error": {"code": "insufficient_capacity", "message": "...", "request_id": "..."}, "simulated": true}
//! ```
//!
//! `Display` is the public message. Internal causes are reachable only through `source()`
//! and are logged once, never returned. No message ever echoes card-like input (D-019).

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sandbox_db::DbError;
use serde::Serialize;

/// Stable error codes. Clients branch on these; renaming one is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// Generic malformed request.
    BadRequest,
    /// Body is not syntactically valid JSON.
    MalformedJson,
    /// Body does not match the schema or a business rule.
    InvalidBody,
    /// Wrong `Content-Type`.
    UnsupportedMediaType,
    /// Body larger than the limit.
    PayloadTooLarge,
    /// A money-moving request without an `Idempotency-Key`.
    IdempotencyKeyRequired,
    /// A malformed `Idempotency-Key`.
    InvalidIdempotencyKey,
    /// The same key was used with a different request (D-016).
    IdempotencyKeyConflict,
    /// The request carried card data or a card field (D-019).
    RawCardDataRejected,
    /// A token that is not a known `tok_sim_*` scenario.
    InvalidPaymentToken,
    /// Missing or wrong credentials.
    Unauthorized,
    /// The key lacks the scope.
    Forbidden,
    /// No route.
    RouteNotFound,
    /// The path exists, the method does not.
    MethodNotAllowed,
    /// The resource does not exist in this project.
    NotFound,
    /// Capacity would be overrun; the excess fails (D-014).
    InsufficientCapacity,
    /// The request conflicts with the resource's current state.
    InvalidState,
    /// A unique name is already taken.
    Conflict,
    /// Too many requests in flight.
    Overloaded,
    /// This project spent its report budget (D-047); retry after `Retry-After` seconds.
    RateLimited,
    /// Too many requests queued for one payment; retry with the same key.
    PaymentBusy,
    /// The request deadline elapsed.
    DeadlineExceeded,
    /// A dependency (the database) is unavailable.
    Unavailable,
    /// A defect. Details are in the logs only.
    Internal,
}

impl ErrorCode {
    /// Best code for a status produced by code we do not control.
    pub fn from_status(status: StatusCode) -> Self {
        match status {
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::FORBIDDEN => Self::Forbidden,
            StatusCode::NOT_FOUND => Self::RouteNotFound,
            StatusCode::METHOD_NOT_ALLOWED => Self::MethodNotAllowed,
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => Self::DeadlineExceeded,
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType,
            StatusCode::SERVICE_UNAVAILABLE => Self::Unavailable,
            s if s.is_client_error() => Self::BadRequest,
            _ => Self::Internal,
        }
    }
}

/// The client-visible part of an error, stored in response extensions so the edge
/// middleware can re-render it with the request id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicError {
    /// Stable code.
    pub code: ErrorCode,
    /// Safe message.
    pub message: String,
}

impl PublicError {
    /// A generic error for a bare status.
    pub fn from_status(status: StatusCode) -> Self {
        Self {
            code: ErrorCode::from_status(status),
            message: status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_owned(),
        }
    }
}

/// Every way a request can fail.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Malformed JSON.
    #[error("malformed JSON body: {0}")]
    MalformedJson(String),
    /// Schema or rule violation; the message is already safe.
    #[error("{0}")]
    InvalidBody(String),
    /// Other malformed request.
    #[error("{0}")]
    BadRequest(String),
    /// Wrong content type.
    #[error("expected Content-Type: application/json")]
    UnsupportedMediaType,
    /// Body too large.
    #[error("request body exceeds the configured limit")]
    PayloadTooLarge,
    /// No `Idempotency-Key`.
    #[error("this request moves (simulated) money and needs an Idempotency-Key header")]
    IdempotencyKeyRequired,
    /// Bad `Idempotency-Key`.
    #[error("{0}")]
    InvalidIdempotencyKey(String),
    /// Same key, different request.
    #[error(
        "this Idempotency-Key was already used with a different request; it is never reinterpreted"
    )]
    IdempotencyKeyConflict,
    /// Card data present. The value is never echoed.
    #[error("raw card data is never accepted; use a tok_sim_* scenario token")]
    RawCardData,
    /// Unknown or malformed token.
    #[error("{0}")]
    InvalidPaymentToken(String),
    /// No or bad credentials.
    #[error("missing or invalid sandbox key")]
    Unauthorized,
    /// Missing scope.
    #[error("this sandbox key lacks the {0} scope")]
    Forbidden(&'static str),
    /// No route.
    #[error("no route matches this path")]
    RouteNotFound,
    /// Missing resource.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// Capacity refusal.
    #[error("{0}")]
    InsufficientCapacity(String),
    /// State refusal.
    #[error("{0}")]
    InvalidState(String),
    /// Uniqueness refusal.
    #[error("{0}")]
    Conflict(String),
    /// Shed.
    #[error("too many requests in flight; retry shortly")]
    Overloaded,
    /// A project's report budget is spent (D-047).
    #[error("this project's report budget is spent; retry after the Retry-After seconds")]
    RateLimited {
        /// Seconds until the budget covers the request.
        retry_after: u64,
    },
    /// One payment's queue is too long.
    #[error(
        "this payment is busy with other requests; retry shortly with the same Idempotency-Key"
    )]
    PaymentBusy,
    /// Dependency down.
    #[error("the database is unavailable; retry shortly")]
    Unavailable(#[source] anyhow::Error),
    /// Statement or lock deadline.
    #[error("the request ran out of time in the database; retry with the same Idempotency-Key")]
    DatabaseTimeout(#[source] anyhow::Error),
    /// Defect.
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

/// Longest client-derived detail ever placed in a message.
pub const MAX_DETAIL: usize = 200;

/// Truncates on a char boundary.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let end = text.floor_char_boundary(max);
    format!("{}...", &text[..end])
}

impl ApiError {
    /// HTTP status.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::MalformedJson(_)
            | Self::BadRequest(_)
            | Self::IdempotencyKeyRequired
            | Self::InvalidIdempotencyKey(_) => StatusCode::BAD_REQUEST,
            Self::InvalidBody(_) | Self::RawCardData | Self::InvalidPaymentToken(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::IdempotencyKeyConflict
            | Self::InsufficientCapacity(_)
            | Self::InvalidState(_)
            | Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::RouteNotFound | Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Overloaded
            | Self::PaymentBusy
            | Self::Unavailable(_)
            | Self::DatabaseTimeout(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::MalformedJson(_) => ErrorCode::MalformedJson,
            Self::InvalidBody(_) => ErrorCode::InvalidBody,
            Self::BadRequest(_) => ErrorCode::BadRequest,
            Self::UnsupportedMediaType => ErrorCode::UnsupportedMediaType,
            Self::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            Self::IdempotencyKeyRequired => ErrorCode::IdempotencyKeyRequired,
            Self::InvalidIdempotencyKey(_) => ErrorCode::InvalidIdempotencyKey,
            Self::IdempotencyKeyConflict => ErrorCode::IdempotencyKeyConflict,
            Self::RawCardData => ErrorCode::RawCardDataRejected,
            Self::InvalidPaymentToken(_) => ErrorCode::InvalidPaymentToken,
            Self::Unauthorized => ErrorCode::Unauthorized,
            Self::Forbidden(_) => ErrorCode::Forbidden,
            Self::RouteNotFound => ErrorCode::RouteNotFound,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::InsufficientCapacity(_) => ErrorCode::InsufficientCapacity,
            Self::InvalidState(_) => ErrorCode::InvalidState,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::Overloaded => ErrorCode::Overloaded,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::PaymentBusy => ErrorCode::PaymentBusy,
            Self::Unavailable(_) => ErrorCode::Unavailable,
            Self::DatabaseTimeout(_) => ErrorCode::DeadlineExceeded,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// The client-visible projection.
    pub fn public(&self) -> PublicError {
        PublicError {
            code: self.code(),
            message: self.to_string(),
        }
    }

    /// Logs once, at a level matching who is at fault. Never logs request bodies.
    fn log(&self) {
        let code = self.code();
        match self {
            Self::Internal(cause) => {
                let cause = format!("{cause:#}");
                tracing::error!(?code, %cause, "request failed");
            }
            Self::Unavailable(cause) | Self::DatabaseTimeout(cause) => {
                let cause = format!("{cause:#}");
                tracing::warn!(?code, %cause, "dependency failure");
            }
            Self::Overloaded | Self::PaymentBusy => tracing::warn!(?code, "request shed"),
            Self::RateLimited { .. } => tracing::info!(?code, "report refused: budget spent"),
            // The message of these is fixed text; the rejected value is never logged.
            Self::RawCardData => tracing::warn!(?code, "request with card data rejected"),
            _ => tracing::debug!(?code, "request rejected"),
        }
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::NotFound => Self::NotFound("resource"),
            DbError::Unavailable(_) => Self::Unavailable(error.into()),
            DbError::Timeout(_) | DbError::Retryable(_) => Self::DatabaseTimeout(error.into()),
            other => Self::Internal(other.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.log();
        let mut response = render(self.status(), &self.public(), None);
        if let Self::RateLimited { retry_after } = &self {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(*retry_after));
        } else if matches!(
            self,
            Self::Overloaded | Self::PaymentBusy | Self::Unavailable(_) | Self::DatabaseTimeout(_)
        ) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: EnvelopeBody<'a>,
    simulated: bool,
}

#[derive(Serialize)]
struct EnvelopeBody<'a> {
    code: ErrorCode,
    message: &'a str,
    request_id: Option<&'a str>,
}

/// Renders the envelope; the only function that knows its shape.
pub fn render(status: StatusCode, error: &PublicError, request_id: Option<&str>) -> Response {
    let envelope = Envelope {
        error: EnvelopeBody {
            code: error.code,
            message: &error.message,
            request_id,
        },
        simulated: true,
    };
    let mut response = (status, Json(envelope)).into_response();
    response.extensions_mut().insert(error.clone());
    response
}

/// True when the response declares a JSON body.
pub fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_causes_never_reach_the_message() {
        let err = ApiError::Internal(anyhow::anyhow!("password=hunter2 at db.rs:12"));
        assert_eq!(err.public().message, "internal error");
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn card_rejections_have_fixed_text() {
        let message = ApiError::RawCardData.public().message;
        assert!(!message.chars().any(|c| c.is_ascii_digit()), "{message}");
    }

    #[test]
    fn database_outages_are_503_with_retry_after() {
        let response =
            ApiError::from(DbError::Unavailable(sqlx::Error::PoolTimedOut)).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }
}
