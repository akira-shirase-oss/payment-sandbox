//! Extractors that enforce the sandbox's input rules before a handler runs:
//!
//! - [`IdempotencyKeyHeader`]: money-moving requests carry a valid `Idempotency-Key`.
//! - [`SandboxJson`]: JSON bodies are size-bounded (by the middleware), refused if they
//!   name a card field or carry a card-like number anywhere (D-019), and only then parsed
//!   into the handler's type, which denies unknown fields.

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Request},
    http::{header, request::Parts},
};
use sandbox_domain::card_guard;
use sandbox_domain::identity::{IdempotencyKey, KeyError};
use serde::de::DeserializeOwned;

use crate::error::{ApiError, MAX_DETAIL, truncate};

/// The `Idempotency-Key` header name.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// A validated `Idempotency-Key`.
#[derive(Debug, Clone)]
pub struct IdempotencyKeyHeader(pub IdempotencyKey);

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKeyHeader {
    type Rejection = ApiError;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "axum's trait is async; reading a header needs no await"
    )]
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let mut values = parts.headers.get_all(IDEMPOTENCY_KEY).iter();
        let Some(value) = values.next() else {
            return Err(ApiError::IdempotencyKeyRequired);
        };
        if values.next().is_some() {
            return Err(ApiError::InvalidIdempotencyKey(
                "send exactly one Idempotency-Key header".to_owned(),
            ));
        }
        let text = value.to_str().map_err(|_| {
            ApiError::InvalidIdempotencyKey(
                "Idempotency-Key may contain only visible ASCII characters".to_owned(),
            )
        })?;
        match IdempotencyKey::parse(text) {
            Ok(key) => Ok(Self(key)),
            Err(KeyError::CardLike) => Err(ApiError::RawCardData),
            Err(other) => Err(ApiError::InvalidIdempotencyKey(other.to_string())),
        }
    }
}

/// A UUID path parameter. Parsed here rather than by axum's `Path<Uuid>`, whose rejection
/// is logged (at TRACE) with the raw segment — which may be a card number (review 8).
#[derive(Debug, Clone, Copy)]
pub struct PathId(pub uuid::Uuid);

impl<S: Send + Sync> FromRequestParts<S> for PathId {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let axum::extract::Path(raw): axum::extract::Path<String> =
            axum::extract::Path::from_request_parts(parts, state)
                .await
                .map_err(|_| ApiError::BadRequest("malformed path".to_owned()))?;
        uuid::Uuid::try_parse(&raw)
            .map(Self)
            .map_err(|_| ApiError::NotFound("resource"))
    }
}

/// A JSON body that passed the card guard.
#[derive(Debug, Clone)]
pub struct SandboxJson<T>(pub T);

impl<T, S> FromRequest<S> for SandboxJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let is_json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                let mime = v.split(';').next().unwrap_or("").trim();
                mime.eq_ignore_ascii_case("application/json")
            });
        if !is_json {
            return Err(ApiError::UnsupportedMediaType);
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|_| ApiError::PayloadTooLarge)?;
        parse_guarded(&bytes).map(SandboxJson)
    }
}

/// Parses `bytes` as JSON, runs the card guard over every key and string, then converts
/// to `T`. Exposed for tests and for the simulator's client.
pub fn parse_guarded<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        // Syntax errors report positions, not values.
        ApiError::MalformedJson(truncate(&e.to_string(), MAX_DETAIL))
    })?;
    if carries_card_data(&value) {
        return Err(ApiError::RawCardData);
    }
    serde_json::from_value(value).map_err(|_| {
        // Fixed text: serde's messages quote the offending value (review 7).
        ApiError::InvalidBody(
            "the request body does not match this endpoint's schema (field names, types or ranges)"
                .to_owned(),
        )
    })
}

/// True when any object key is a card field, or any string (key or value) is card-like.
pub fn carries_card_data(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => card_guard::contains_pan_like(text),
        serde_json::Value::Array(items) => items.iter().any(carries_card_data),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            card_guard::is_card_field_name(key)
                || card_guard::contains_pan_like(key)
                || carries_card_data(value)
        }),
        // A card number sent as a JSON number is card data too (review 7).
        serde_json::Value::Number(n) => card_guard::contains_pan_like(&n.to_string()),
        serde_json::Value::Bool(_) | serde_json::Value::Null => false,
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "fields exist to be parsed")]
    struct Body {
        amount: i64,
        token: String,
        description: Option<String>,
    }

    #[test]
    fn card_fields_and_numbers_are_refused_anywhere() {
        let refused = [
            r#"{"amount": 5, "token": "tok_sim_approve", "number": "x"}"#,
            r#"{"amount": 5, "token": "tok_sim_approve", "card": {"exp_month": 1}}"#,
            r#"{"amount": 5, "token": "4242424242424242"}"#,
            r#"{"amount": 5, "token": "tok_sim_approve", "description": "my card 4242 4242 4242 4242"}"#,
            r#"{"amount": 5, "token": "tok_sim_approve", "nested": [{"cvc": "123"}]}"#,
        ];
        for body in refused {
            let result = parse_guarded::<Body>(body.as_bytes());
            assert!(
                matches!(result, Err(ApiError::RawCardData)),
                "{body}: {result:?}"
            );
        }
    }

    #[test]
    fn clean_bodies_parse_and_unknown_fields_fail() {
        let ok = parse_guarded::<Body>(br#"{"amount": 5, "token": "tok_sim_approve"}"#);
        assert!(ok.is_ok(), "{ok:?}");
        let unknown = parse_guarded::<Body>(br#"{"amount": 5, "token": "t", "extra": 1}"#);
        assert!(
            matches!(unknown, Err(ApiError::InvalidBody(_))),
            "{unknown:?}"
        );
        let syntax = parse_guarded::<Body>(b"{\"amount\": ");
        assert!(
            matches!(syntax, Err(ApiError::MalformedJson(_))),
            "{syntax:?}"
        );
    }
}
