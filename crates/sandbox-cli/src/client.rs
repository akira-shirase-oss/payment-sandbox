//! The HTTP client every command and the scenario runner use (D-068).
//!
//! - Every money-moving request carries an `Idempotency-Key`; a retry reuses it, so the
//!   server replays or re-drives the same operation (D-015, D-016) and can never charge twice.
//! - Retries are bounded: at most [`RetryPolicy::max_attempts`] attempts, exponential backoff
//!   with full jitter, never past the overall deadline, and only for answers that say "try
//!   again" (no answer at all, `429`, and `503` busy/overloaded/unavailable/deadline).
//! - An answer is honest: `202` with an `uncertain` operation is returned as such, never as
//!   success; a money-moving request that got no answer at all is [`ClientError::Unknown`],
//!   naming the key to retry with, never "failed".
//! - Every answer must come from a sandbox (`Sandbox-Simulated: true`), or the client stops.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::Config;

/// The header every sandbox response carries.
pub const SIMULATED_HEADER: &str = "sandbox-simulated";

/// How hard a request is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in all, the first included.
    pub max_attempts: u32,
    /// The first backoff's ceiling; each later one doubles, up to `cap`.
    pub base: Duration,
    /// The largest backoff ceiling.
    pub cap: Duration,
    /// Deadline of one attempt (connect, send, the whole answer).
    pub attempt_timeout: Duration,
    /// Deadline of the request, retries included.
    pub deadline: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(2),
            attempt_timeout: Duration::from_secs(10),
            deadline: Duration::from_secs(30),
        }
    }
}

/// Largest answer body read (the API's own answers are far smaller).
const MAX_BODY: usize = 4 * 1024 * 1024;

/// A sandbox answer: its status and JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// HTTP status.
    pub status: u16,
    /// The JSON body.
    pub body: Value,
    /// The `Idempotency-Key` sent (money-moving requests).
    pub idempotency_key: Option<String>,
    /// Attempts made.
    pub attempts: u32,
}

/// Why a request did not produce an answer the caller can use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The API answered with its error envelope.
    #[error("{message} ({code}, HTTP {status})")]
    Refused {
        /// HTTP status.
        status: u16,
        /// Stable code.
        code: String,
        /// The server's message.
        message: String,
    },
    /// A money-moving request got no usable answer: it may or may not have been applied.
    #[error(
        "no answer from the sandbox after {attempts} attempts ({cause}); the request may or may not have been applied: retry it with --idempotency-key {idempotency_key} to find out, never with a new key"
    )]
    Unknown {
        /// The key to retry with.
        idempotency_key: String,
        /// Attempts made.
        attempts: u32,
        /// The last failure.
        cause: String,
    },
    /// A read got no answer.
    #[error(
        "the sandbox at {url} did not answer ({cause}); check that sandbox-server is running (scripts/stack.sh, README \"Run it\")"
    )]
    Unreachable {
        /// The API URL.
        url: String,
        /// The last failure.
        cause: String,
    },
    /// Something answered, but not a sandbox.
    #[error(
        "{url} answered without Sandbox-Simulated: true, so it is not a Payment Sandbox; check the API URL"
    )]
    NotASandbox {
        /// The API URL.
        url: String,
    },
    /// An answer that is not the API's JSON.
    #[error("the sandbox sent an unreadable answer (HTTP {status})")]
    Unreadable {
        /// HTTP status.
        status: u16,
    },
}

impl ClientError {
    /// The stable code of a refusal, if this is one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// One attempt's result, before the retry decision.
enum Attempt {
    Answered {
        status: u16,
        body: Value,
        retry_after: Option<Duration>,
    },
    Failed(String),
}

/// The client.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    config: Config,
    retry: RetryPolicy,
}

impl Client {
    /// A client for a checked configuration.
    pub fn new(config: Config, retry: RetryPolicy) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2).min(retry.attempt_timeout))
            .timeout(retry.attempt_timeout)
            // Shorter than the server's read timeout (D-054), so a pooled connection is never
            // reused just as the server closes it.
            .pool_idle_timeout(Duration::from_secs(4))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(concat!(
                "sandbox-cli/",
                env!("CARGO_PKG_VERSION"),
                " (simulated money only)"
            ))
            .build()
            .map_err(|e| ClientError::Unreachable {
                url: config.api_url.clone(),
                cause: e.to_string(),
            })?;
        Ok(Self {
            http,
            config,
            retry,
        })
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// `GET path`, retried while the answer says to try again.
    pub async fn get(&self, path: &str) -> Result<Answer, ClientError> {
        self.send(reqwest::Method::GET, path, None, None).await
    }

    /// `POST path` without an idempotency key (not money-moving: keys, resets, sessions).
    /// Retried only when no attempt can have reached the server, so it is never applied twice
    /// by the client: connection refused, or `503`/`429` answers.
    pub async fn post(&self, path: &str, body: &Value) -> Result<Answer, ClientError> {
        self.send(reqwest::Method::POST, path, Some(body), None)
            .await
    }

    /// A money-moving `POST` under `key` (a fresh one when `None`): retried under the same key.
    pub async fn post_money(
        &self,
        path: &str,
        body: &Value,
        key: Option<String>,
    ) -> Result<Answer, ClientError> {
        let key = key.unwrap_or_else(new_idempotency_key);
        self.send(reqwest::Method::POST, path, Some(body), Some(key))
            .await
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        key: Option<String>,
    ) -> Result<Answer, ClientError> {
        let started = Instant::now();
        let url = format!("{}{path}", self.config.api_url);
        let mut attempts = 0;
        let last_failure = loop {
            attempts += 1;
            let attempt = self.attempt(&method, &url, body, key.as_deref()).await?;
            let (retry_after, failure) = match attempt {
                Attempt::Answered {
                    status,
                    body,
                    retry_after,
                } => {
                    if (200..300).contains(&status) {
                        return Ok(Answer {
                            status,
                            body,
                            idempotency_key: key,
                            attempts,
                        });
                    }
                    let refusal = refusal(status, &body)?;
                    if !retryable(status, refusal.code().unwrap_or_default()) {
                        return Err(refusal);
                    }
                    (retry_after, refusal.to_string())
                }
                Attempt::Failed(cause) => {
                    // A plain POST that may have reached the server is never re-sent.
                    if method == reqwest::Method::POST
                        && key.is_none()
                        && !cause.starts_with("connect")
                    {
                        return Err(ClientError::Unreachable {
                            url: self.config.api_url.clone(),
                            cause,
                        });
                    }
                    (None, cause)
                }
            };
            let backoff = backoff(&self.retry, attempts, retry_after);
            if attempts >= self.retry.max_attempts
                || started.elapsed() + backoff > self.retry.deadline
            {
                break failure;
            }
            tokio::time::sleep(backoff).await;
        };
        Err(match key {
            Some(idempotency_key) => ClientError::Unknown {
                idempotency_key,
                attempts,
                cause: last_failure,
            },
            None => ClientError::Unreachable {
                url: self.config.api_url.clone(),
                cause: last_failure,
            },
        })
    }

    async fn attempt(
        &self,
        method: &reqwest::Method,
        url: &str,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> Result<Attempt, ClientError> {
        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(self.config.key.expose())
            .header("accept", "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return Ok(Attempt::Failed(describe(&error))),
        };
        let status = response.status().as_u16();
        let simulated = response
            .headers()
            .get(SIMULATED_HEADER)
            .is_some_and(|v| v.as_bytes() == b"true");
        if !simulated {
            return Err(ClientError::NotASandbox {
                url: self.config.api_url.clone(),
            });
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let bytes = match read_bounded(response).await {
            Ok(bytes) => bytes,
            Err(cause) => return Ok(Attempt::Failed(cause)),
        };
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| ClientError::Unreadable { status })?
        };
        Ok(Attempt::Answered {
            status,
            body,
            retry_after,
        })
    }
}

async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > MAX_BODY {
                    return Err("the answer exceeded 4 MiB".to_owned());
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(bytes),
            Err(error) => return Err(describe(&error)),
        }
    }
}

/// The error envelope of a non-2xx answer.
fn refusal(status: u16, body: &Value) -> Result<ClientError, ClientError> {
    let error = &body["error"];
    match (error["code"].as_str(), error["message"].as_str()) {
        (Some(code), Some(message)) => Ok(ClientError::Refused {
            status,
            code: code.to_owned(),
            message: message.to_owned(),
        }),
        _ => Err(ClientError::Unreadable { status }),
    }
}

/// Answers that say "try again": `429` and `503` for load, busy payments and deadlines.
pub fn retryable(status: u16, code: &str) -> bool {
    match status {
        429 => true,
        503 => matches!(
            code,
            "payment_busy" | "overloaded" | "unavailable" | "deadline_exceeded"
        ),
        _ => false,
    }
}

/// The wait before attempt `attempts + 1`: full jitter over `min(cap, base × 2^(attempts−1))`,
/// and never less than the server's `Retry-After` (itself capped by `cap`).
pub fn backoff(policy: &RetryPolicy, attempts: u32, retry_after: Option<Duration>) -> Duration {
    let exponent = attempts.saturating_sub(1).min(16);
    let ceiling = policy
        .base
        .saturating_mul(1_u32 << exponent)
        .min(policy.cap);
    let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
    let jittered = Duration::from_millis(match getrandom::u64() {
        Ok(random) => random % ceiling_ms.saturating_add(1),
        Err(_) => ceiling_ms / 2,
    });
    retry_after
        .map(|r| r.min(policy.cap))
        .map_or(jittered, |floor| jittered.max(floor))
}

/// A new idempotency key: `cli_` and 32 letters `a`–`p` from 16 random bytes. No digits, so
/// the card guard can never mistake one for a card number (D-019).
pub fn new_idempotency_key() -> String {
    let mut bytes = [0_u8; 16];
    // Without OS randomness, fall back to the clock: still unique per call in practice, and
    // a collision would be a conflict the server reports, never a silent replay of another
    // request (D-016).
    if getrandom::fill(&mut bytes).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = now.to_le_bytes();
    }
    format!("cli_{}", sandbox_domain::letters::encode(&bytes))
}

/// A short, safe description of a transport failure (never a URL with a key, never a body).
fn describe(error: &reqwest::Error) -> String {
    if error.is_connect() {
        "connect: the connection was refused or failed".to_owned()
    } else if error.is_timeout() {
        "the attempt timed out".to_owned()
    } else if error.is_body() || error.is_decode() {
        "the answer was cut off".to_owned()
    } else {
        "the connection failed".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_try_again_answers_are_retried() {
        for (status, code) in [
            (503, "payment_busy"),
            (503, "overloaded"),
            (503, "unavailable"),
            (503, "deadline_exceeded"),
            (429, "rate_limited"),
        ] {
            assert!(retryable(status, code), "{status} {code}");
        }
        for (status, code) in [
            (409, "idempotency_key_conflict"),
            (409, "insufficient_capacity"),
            (422, "raw_card_data_rejected"),
            (401, "unauthorized"),
            (500, "internal"),
            (503, "something_else"),
        ] {
            assert!(!retryable(status, code), "{status} {code}");
        }
    }

    #[test]
    fn backoff_is_jittered_bounded_and_respects_retry_after() {
        let policy = RetryPolicy::default();
        for attempts in 1..=20 {
            for _ in 0..50 {
                let wait = backoff(&policy, attempts, None);
                let exponent = (attempts - 1).min(16);
                let ceiling = (policy.base * (1 << exponent)).min(policy.cap);
                assert!(wait <= ceiling, "{attempts}: {wait:?} > {ceiling:?}");
            }
        }
        assert!(backoff(&policy, 1, Some(Duration::from_secs(1))) >= Duration::from_secs(1));
        // A huge Retry-After is capped: the deadline decides whether to wait at all.
        assert!(backoff(&policy, 1, Some(Duration::from_secs(3_600))) <= policy.cap);
        // Jitter actually varies.
        let waits: std::collections::BTreeSet<_> =
            (0..50).map(|_| backoff(&policy, 4, None)).collect();
        assert!(waits.len() > 5, "{waits:?}");
    }

    #[test]
    fn idempotency_keys_are_unique_letters_only_and_valid() {
        let keys: std::collections::BTreeSet<_> =
            (0..1_000).map(|_| new_idempotency_key()).collect();
        assert_eq!(keys.len(), 1_000);
        for key in &keys {
            assert!(key.starts_with("cli_") && key.len() == 36, "{key}");
            assert!(!key.bytes().any(|b| b.is_ascii_digit()), "{key}");
            assert!(sandbox_domain::identity::IdempotencyKey::parse(key).is_ok());
        }
    }
}
