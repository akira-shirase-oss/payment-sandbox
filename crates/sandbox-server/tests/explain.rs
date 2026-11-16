//! M5: every synthetic balance can be explained from journal entries (the ship gate's second
//! half), and reconciliation detects a planted drift.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use serde_json::json;

async fn purchases(app: &TestApp, key: &str) -> Vec<String> {
    let mut pids = Vec::new();
    for (n, (amount, capture, refund)) in [
        (1_000, 600, 100),
        (500, 500, 0),
        (250, 0, 0),
        (900, 300, 300),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, auth) = app
            .authorize(key, &format!("o{n}"), amount, "USD", "tok_sim_approve")
            .await;
        let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
        if capture > 0 {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                key,
                Some(&format!("c{n}")),
                json!({"amount": capture}),
            )
            .await;
        }
        if refund > 0 {
            app.post(
                &format!("/v1/payments/{pid}/refunds"),
                key,
                Some(&format!("r{n}")),
                json!({"amount": refund}),
            )
            .await;
        }
        pids.push(pid);
    }
    pids
}

#[tokio::test]
async fn every_balance_is_explained_line_by_line_across_pages() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    purchases(&app, &key).await;
    let (_, balances) = app.get("/v1/balances", &key).await;
    let balances = balances["balances"].as_array().unwrap().clone();
    assert_eq!(balances.len(), 3);
    for balance in balances {
        let account = balance["account"].as_str().unwrap();
        let mut after = 0_i64;
        let mut sum = 0_i64;
        let mut lines = 0_i64;
        let mut last_running = None;
        let mut pages = 0;
        loop {
            let (status, page) = app
                .get(
                    &format!("/v1/balances/USD/{account}/explain?after_seq={after}&limit=3"),
                    &key,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["balance_minor"], balance["balance_minor"], "{account}");
            for line in page["explanation"]["lines"].as_array().unwrap() {
                sum += line["amount_minor"].as_i64().unwrap();
                lines += 1;
                assert_eq!(
                    line["running_minor"].as_i64().unwrap(),
                    sum,
                    "the running total is the sum so far"
                );
                last_running = Some(sum);
            }
            pages += 1;
            if let Some(next) = page["explanation"]["next_after_seq"].as_i64() {
                after = next;
            } else {
                assert_eq!(page["explanation"]["line_count"].as_i64().unwrap(), lines);
                break;
            }
        }
        assert!(pages > 1, "{account}: paged");
        assert_eq!(Some(sum), last_running);
        assert_eq!(
            sum,
            balance["signed_minor"].as_i64().unwrap(),
            "{account}: the lines sum to the balance"
        );
    }
    let (status, _) = app.get("/v1/balances/GBP/payer_held/explain", &key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.get("/v1/balances/USD/cash/explain", &key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    app.cleanup().await;
    sim.remove().await;
}

/// A planted drift — a payment's committed sums changed behind the ledger's back (raw SQL
/// that the CHECK constraints still allow) — is found exactly, and only it.
#[tokio::test]
async fn a_planted_drift_is_detected_exactly() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let pids = purchases(&app, &key).await;
    app.assert_reconciled(&key).await;
    sqlx::query("UPDATE payments SET refunded_minor = refunded_minor + 50 WHERE id = $1::uuid")
        .bind(&pids[0])
        .execute(&app.db.pool)
        .await
        .unwrap();
    let (_, report) = app.get("/v1/reconciliation", &key).await;
    assert_eq!(report["clean"], false, "{report}");
    assert_eq!(report["report"]["payment_mismatches"], json!([pids[0]]));
    let accounts: Vec<String> = report["report"]["account_mismatches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m[1].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(accounts, ["merchant_pending", "payer_funding"], "{report}");
    sqlx::query("UPDATE payments SET refunded_minor = refunded_minor - 50 WHERE id = $1::uuid")
        .bind(&pids[0])
        .execute(&app.db.pool)
        .await
        .unwrap();
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}
