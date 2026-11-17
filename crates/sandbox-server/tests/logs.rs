//! "No secrets in logs" as a test, not a sentence: every log event at every level, from the
//! server, Tower and sqlx, is captured while keys, the bootstrap token and card numbers flow
//! through the API, and the capture is searched for each of them.
//!
//! Its own test binary on purpose: `tracing` caches each callsite's interest process-wide,
//! so a test running in parallel on another thread could make a thread-local capture miss
//! events. Alone in its process, the capture sees everything (a canary proves it).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::http::Method;
use common::{BOOTSTRAP, TestApp};
use serde_json::json;

/// Collects everything logged while it is the thread's default subscriber.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Fails if any secret shape reaches the logs at any level: keys, the bootstrap token,
/// card numbers (gap pattern "no secrets in logs needs a test, not a sentence").
#[tokio::test(flavor = "current_thread")]
async fn no_secret_ever_reaches_the_logs() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("store").await;
    let (reader_id, reader) = app.key(&admin, &["read"]).await;
    app.get("/v1/project", &reader).await;
    app.post(
        &format!("/v1/keys/{reader_id}/revoke"),
        &admin,
        None,
        json!({}),
    )
    .await;
    app.get("/v1/project", &reader).await;
    app.send(
        Method::POST,
        "/v1/projects",
        Some("not-the-bootstrap-token-but-long-enough"),
        None,
        Some(json!({"slug": "x", "name": "x"})),
    )
    .await;
    app.post(
        "/v1/keys",
        &admin,
        Some("4242424242424242"),
        json!({"label": "4000056655665556", "scopes": ["read"]}),
    )
    .await;
    app.post(
        "/v1/keys",
        &admin,
        None,
        json!({"label": "x", "number": "5555555555554444", "scopes": ["read"]}),
    )
    .await;

    // Review finding 8: a card number in the path or in X-Request-Id is never logged or
    // echoed back.
    let request = axum::http::Request::get("/v1/payments/4242424242424242")
        .header("authorization", format!("Bearer {admin}"))
        .body(axum::body::Body::empty())
        .unwrap();
    tower::ServiceExt::oneshot(app.router.clone(), request)
        .await
        .unwrap();
    let request = axum::http::Request::post("/v1/payments")
        .header("authorization", format!("Bearer {admin}"))
        .header("idempotency-key", "rid-1")
        .header("x-request-id", "4111111111111111")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            r#"{"amount": 100, "currency": "USD", "token": "tok_sim_approve"}"#,
        ))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.router.clone(), request)
        .await
        .unwrap();
    assert_ne!(
        response.headers()["x-request-id"],
        "4111111111111111",
        "a card-like request id was echoed"
    );

    // Canary: proves the capture sees events from the code under test's thread.
    tracing::debug!(canary = "capture-canary-7f3a", "canary");
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("capture-canary-7f3a"),
        "the capture is not seeing events"
    );
    assert!(
        logs.contains("project created"),
        "the capture must see server events"
    );
    assert!(
        logs.len() > 200,
        "the capture must have seen the requests: {logs}"
    );
    for secret in [
        admin.as_str(),
        reader.as_str(),
        &admin[7..],
        BOOTSTRAP,
        "not-the-bootstrap-token-but-long-enough",
        "4242424242424242",
        "4000056655665556",
        "5555555555554444",
        "4111111111111111",
        "/v1/payments/4242424242424242",
    ] {
        let line = logs.lines().find(|l| l.contains(secret)).unwrap_or("");
        assert!(
            !logs.contains(secret),
            "a secret reached the logs: {secret} in {line}"
        );
    }
    drop(guard);
    app.cleanup().await;
}
