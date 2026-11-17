//! Review finding 9: reports (the reconciliation, explain-balance pages) scan a whole
//! dataset, so any `read` key could spend the server's database time without bound. Each
//! project now has a budget (a token bucket; a reconciliation costs more than an explain
//! page), refused past it with `429` and `Retry-After`, and one project's spending never
//! touches another's.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use tower::ServiceExt;

/// Status and `Retry-After` of one GET.
async fn get_with_retry_after(app: &TestApp, path: &str, key: &str) -> (StatusCode, Option<u64>) {
    let request = Request::get(path)
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    (response.status(), retry_after)
}

/// Sends `path` until the project is refused; returns how many were answered first and
/// the refusal's `Retry-After`. Bounded, and robust to refills on a slow machine.
async fn until_refused(app: &TestApp, path: &str, key: &str) -> (usize, Option<u64>) {
    for answered in 0..400 {
        let (status, retry_after) = get_with_retry_after(app, path, key).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            return (answered, retry_after);
        }
        assert_eq!(status, StatusCode::OK, "{path}");
    }
    panic!("{path}: 400 reports and never refused");
}

#[tokio::test]
async fn reports_are_rate_limited_per_project() {
    // A budget of 50 units refilled at 10 per second; a reconciliation costs 10, an
    // explain page 1.
    let app = TestApp::new(&[
        ("PS_REPORT_BURST", "50"),
        ("PS_REPORT_REFILL_PER_MINUTE", "600"),
    ])
    .await;
    let (_, a) = app.project("reports-a").await;
    let (_, b) = app.project("reports-b").await;

    let (answered, retry_after) = until_refused(&app, "/v1/reconciliation", &a).await;
    assert!(answered >= 5, "the burst admits five: {answered}");
    assert!(
        retry_after.is_some_and(|s| (1..=2).contains(&s)),
        "{retry_after:?}"
    );
    let (status, body) = app.get("/v1/reconciliation", &a).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    // Hot-path routes never count against it.
    let (status, _) = app.get("/v1/payments", &a).await;
    assert_eq!(status, StatusCode::OK);

    // Another project's budget is its own, and explain pages spend it too.
    let (status, report) = app.get("/v1/reconciliation", &b).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let (answered, _) = until_refused(&app, "/v1/balances/USD/payer_funding/explain", &b).await;
    assert!(answered >= 40, "explain pages cost 1 each: {answered}");

    // The budget refills: a second later, one more reconciliation fits.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let (status, report) = app.get("/v1/reconciliation", &a).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    app.cleanup().await;
}
