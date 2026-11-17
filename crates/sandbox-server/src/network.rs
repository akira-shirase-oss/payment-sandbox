//! The client to the simulated network. Every call has a deadline; anything other than a
//! recorded answer is [`Answer::Unknown`], because the network may or may not have executed
//! the operation — the honest state is `uncertain`, resolved by readback under the same
//! identity, never by guessing (D-004).

use std::time::Duration;

use reqwest::StatusCode;
use sandbox_domain::network::{
    ExecuteReply, ExecuteRequest, OperationPage, Outcome, PROTOCOL_VERSION, Readback,
};
use uuid::Uuid;

/// Largest single answer (execute, readback) accepted from the network.
const MAX_REPLY_BYTES: u64 = 64 * 1024;
/// Largest listing page accepted: 1,000 operations of about 300 bytes each, with room.
const MAX_PAGE_BYTES: u64 = 1024 * 1024;

/// What one call learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Recorded as approved.
    Approved(ExecuteReply),
    /// Recorded as declined.
    Declined(ExecuteReply),
    /// No usable answer: timeout, refused connection, 5xx, malformed reply.
    Unknown(&'static str),
    /// The network holds a *different* request under this identity (it answered 409, or
    /// its record's fingerprint differs): the same key was reused with other fields.
    /// Final: never applied, never retried.
    Conflict,
    /// The identity was fenced by an earlier readback: it never executed and never will.
    Fenced,
}

/// A network failure on a readback or listing call.
#[derive(Debug, thiserror::Error)]
#[error("simulated network call failed: {0}")]
pub struct NetworkError(pub String);

/// The shared HTTP client (one connection pool, bounded idle connections, no proxies).
#[derive(Debug, Clone)]
pub struct Network {
    http: reqwest::Client,
    base: String,
}

impl Network {
    /// Builds the client. Every request presents `credential` (D-046); the header is marked
    /// sensitive, so no debug output prints it.
    pub fn new(base: &str, credential: &str, timeout: Duration) -> Result<Self, NetworkError> {
        let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {credential}"))
            .map_err(|_| NetworkError("the network credential is not a header value".to_owned()))?;
        bearer.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, bearer);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(timeout.min(Duration::from_millis(500)))
            .timeout(timeout)
            .pool_max_idle_per_host(256)
            .pool_idle_timeout(sandbox_http::CLIENT_POOL_IDLE)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| NetworkError(e.to_string()))?;
        Ok(Self {
            http,
            base: base.to_owned(),
        })
    }

    /// Executes an operation under its identity.
    pub async fn execute(&self, request: &ExecuteRequest) -> Answer {
        let sent = self
            .http
            .post(format!("{}/v1/operations", self.base))
            .header("sandbox-network-protocol", PROTOCOL_VERSION)
            .json(request)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(e) if e.is_timeout() => return Answer::Unknown("timeout"),
            Err(e) if e.is_connect() => return Answer::Unknown("connect"),
            Err(_) => return Answer::Unknown("transport"),
        };
        match response.status() {
            StatusCode::OK => match read_json::<ExecuteReply>(response, MAX_REPLY_BYTES).await {
                Some(reply)
                    if reply.id == request.id && reply.fingerprint != request.fingerprint() =>
                {
                    tracing::warn!(operation = %request.id, "the network's record is of another request");
                    Answer::Conflict
                }
                Some(reply) if reply.id != request.id => {
                    tracing::error!(operation = %request.id, "network answered for another identity");
                    Answer::Unknown("wrong identity")
                }
                Some(reply) => match reply.outcome {
                    Outcome::Approved => Answer::Approved(reply),
                    Outcome::Declined => Answer::Declined(reply),
                },
                None => Answer::Unknown("malformed reply"),
            },
            StatusCode::GONE => Answer::Fenced,
            StatusCode::CONFLICT => {
                tracing::warn!(operation = %request.id, "the network holds another request under this identity");
                Answer::Conflict
            }
            status if status.is_server_error() => Answer::Unknown("unavailable"),
            status => {
                tracing::error!(operation = %request.id, %status, "network refused the request");
                Answer::Unknown("refused")
            }
        }
    }

    /// Reads back what the network recorded for `id`; `fence` tombstones an unknown id.
    pub async fn readback(&self, id: Uuid, fence: bool) -> Result<Readback, NetworkError> {
        let response = self
            .http
            .get(format!("{}/v1/operations/{id}?fence={fence}", self.base))
            .header("sandbox-network-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .map_err(|e| NetworkError(e.to_string()))?;
        if response.status() != StatusCode::OK {
            return Err(NetworkError(format!(
                "readback status {}",
                response.status()
            )));
        }
        read_json(response, MAX_REPLY_BYTES)
            .await
            .ok_or_else(|| NetworkError("malformed readback".to_owned()))
    }

    /// Lists one project's recorded operations after `after`.
    pub async fn list(
        &self,
        project: Uuid,
        after: i64,
        limit: i64,
    ) -> Result<OperationPage, NetworkError> {
        let response = self
            .http
            .get(format!(
                "{}/v1/operations?project_id={project}&after={after}&limit={limit}",
                self.base
            ))
            .header("sandbox-network-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .map_err(|e| NetworkError(e.to_string()))?;
        if response.status() != StatusCode::OK {
            return Err(NetworkError(format!("list status {}", response.status())));
        }
        read_json(response, MAX_PAGE_BYTES)
            .await
            .ok_or_else(|| NetworkError("malformed or oversized list".to_owned()))
    }
}

async fn read_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    max: u64,
) -> Option<T> {
    if response.content_length().is_some_and(|n| n > max) {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() as u64 > max {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
