//! M2: the payment lifecycle against a real simulated network (its own HTTP server and
//! SQLite store). Every test ends with a clean reconciliation: zero journal imbalance,
//! every payment's sums equal to its journal entries, one entry per succeeded operation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use serde_json::{Value, json};

async fn setup() -> (TestSim, TestApp, String) {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, admin) = app.project("store").await;
    (sim, app, admin)
}

async fn teardown(sim: TestSim, app: TestApp) {
    app.cleanup().await;
    sim.remove().await;
}

fn id(reply: &Value) -> String {
    reply["payment"]["id"].as_str().unwrap().to_owned()
}

fn balance(balances: &Value, currency: &str, account: &str) -> i64 {
    balances["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["currency"] == currency && b["account"] == account)
        .map_or(0, |b| b["balance_minor"].as_i64().unwrap())
}

#[tokio::test]
async fn a_purchase_authorize_capture_partial_refund_balances_and_reconciles() {
    let (sim, app, key) = setup().await;
    let (status, auth) = app
        .authorize(&key, "order-1", 1_000, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{auth}");
    assert_eq!(auth["payment"]["status"], "authorized");
    assert_eq!(auth["operation"]["state"], "succeeded");
    assert!(
        auth["operation"]["network_ref"]
            .as_str()
            .unwrap()
            .starts_with("simnet_")
    );
    let pid = id(&auth);

    let (status, cap) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("cap-1"),
            json!({"amount": 600}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{cap}");
    assert_eq!(cap["payment"]["status"], "partially_captured");
    let (status, refund) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            &key,
            Some("ref-1"),
            json!({"amount": 200}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{refund}");
    assert_eq!(refund["payment"]["status"], "partially_refunded");
    assert_eq!(refund["payment"]["refundable"], 400);
    let (status, void) = app
        .post(
            &format!("/v1/payments/{pid}/void"),
            &key,
            Some("void-1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{void}");
    assert_eq!(
        void["operation"]["amount"], 400,
        "the void releases exactly the uncaptured remainder"
    );

    let (_, balances) = app.get("/v1/balances", &key).await;
    assert_eq!(balance(&balances, "USD", "payer_held"), 0);
    assert_eq!(balance(&balances, "USD", "merchant_pending"), 400);
    assert_eq!(balance(&balances, "USD", "payer_funding"), 400);
    let (_, journal) = app.get(&format!("/v1/payments/{pid}/journal"), &key).await;
    let recipes: Vec<&str> = journal["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["recipe"].as_str().unwrap())
        .collect();
    assert_eq!(recipes, ["authorize", "capture", "refund", "void"]);
    let report = app.assert_reconciled(&key).await;
    assert_eq!(report["report"]["entries_checked"], 4);
    assert_eq!(
        sim.store.count().await.unwrap(),
        4,
        "one network operation per identity"
    );
    teardown(sim, app).await;
}

#[tokio::test]
async fn the_same_key_with_a_different_amount_is_a_conflict_and_the_same_request_replays() {
    let (sim, app, key) = setup().await;
    let (status, first) = app
        .authorize(&key, "order-7", 500, "EUR", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    for (amount, currency, token) in [
        (501, "EUR", "tok_sim_approve"),
        (500, "USD", "tok_sim_approve"),
        (500, "EUR", "tok_sim_decline_do_not_honor"),
    ] {
        let (status, reply) = app
            .authorize(&key, "order-7", amount, currency, token)
            .await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("idempotency_key_conflict")),
            "{amount} {currency} {token}"
        );
    }
    let (status, again) = app
        .authorize(&key, "order-7", 500, "EUR", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["replayed"], true);
    assert_eq!(id(&again), id(&first));
    let pid = id(&first);
    // Children bind their body too.
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, reply) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 200}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_key_conflict"))
    );
    let (status, reply) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 100}),
        )
        .await;
    assert_eq!(
        (status, reply["replayed"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(
        payment["payment"]["captured"], 100,
        "a replay never captures twice"
    );
    assert_eq!(sim.store.count().await.unwrap(), 2);
    app.assert_reconciled(&key).await;
    teardown(sim, app).await;
}

#[tokio::test]
async fn declines_are_recorded_and_a_declined_payment_cannot_be_captured() {
    let (sim, app, key) = setup().await;
    for (token, code) in [
        ("tok_sim_decline_insufficient_funds", "insufficient_funds"),
        ("tok_sim_decline_do_not_honor", "do_not_honor"),
        ("tok_sim_decline_expired", "expired_payment_method"),
    ] {
        let (status, reply) = app.authorize(&key, token, 900, "USD", token).await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
        assert_eq!(reply["payment"]["status"], "declined");
        assert_eq!(reply["payment"]["decline_code"], code);
        let pid = id(&reply);
        let (status, reply) = app
            .post(
                &format!("/v1/payments/{pid}/captures"),
                &key,
                Some(&format!("c-{token}")),
                json!({"amount": 1}),
            )
            .await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("invalid_state"))
        );
    }
    let (status, reply) = app
        .authorize(&key, "bad", 900, "USD", "tok_sim_teleport")
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("invalid_payment_token")
        )
    );
    let (_, balances) = app.get("/v1/balances", &key).await;
    assert_eq!(
        balances["balances"].as_array().unwrap().len(),
        0,
        "declines post nothing"
    );
    app.assert_reconciled(&key).await;
    teardown(sim, app).await;
}

#[tokio::test]
async fn zero_and_three_decimal_currencies_post_exact_minor_units() {
    let (sim, app, key) = setup().await;
    let (_, yen) = app
        .authorize(&key, "jpy", 500, "JPY", "tok_sim_approve")
        .await;
    let (_, dinar) = app
        .authorize(&key, "kwd", 1_234, "KWD", "tok_sim_approve")
        .await;
    app.post(
        &format!("/v1/payments/{}/captures", id(&yen)),
        &key,
        Some("jc"),
        json!({"amount": 500}),
    )
    .await;
    app.post(
        &format!("/v1/payments/{}/captures", id(&dinar)),
        &key,
        Some("kc"),
        json!({"amount": 1}),
    )
    .await;
    let (_, balances) = app.get("/v1/balances", &key).await;
    let row = |c: &str, a: &str| {
        balances["balances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["currency"] == c && b["account"] == a)
            .cloned()
            .unwrap()
    };
    assert_eq!(row("JPY", "merchant_pending")["balance"], "500");
    assert_eq!(row("KWD", "merchant_pending")["balance"], "0.001");
    assert_eq!(row("KWD", "payer_held")["balance"], "1.233");
    for bad in [0, -1, 1_000_000_000_001] {
        let (status, _) = app
            .authorize(&key, &format!("bad-{bad}"), bad, "USD", "tok_sim_approve")
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    let (status, _) = app
        .authorize(&key, "gbp", 100, "GBP", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    app.assert_reconciled(&key).await;
    teardown(sim, app).await;
}

/// D-027's named example, the hot-payment race: 64 concurrent partial refunds of 10 against
/// 300 captured. Exactly 30 succeed; 34 fail with `insufficient_capacity`.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn sixty_four_concurrent_refunds_of_ten_against_three_hundred_admit_exactly_thirty() {
    let (sim, app, key) = setup().await;
    let (_, auth) = app
        .authorize(&key, "hot", 300, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    app.post(
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("cap"),
        json!({"amount": 300}),
    )
    .await;
    let app = Arc::new(app);
    let mut tasks = Vec::new();
    for n in 0..64 {
        let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
        tasks.push(tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{pid}/refunds"),
                &key,
                Some(&format!("refund-{n}")),
                json!({"amount": 10}),
            )
            .await
        }));
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for task in tasks {
        let (status, reply) = task.await.unwrap();
        let label = reply["error"]["code"]
            .as_str()
            .map_or_else(|| status.as_u16().to_string(), str::to_owned);
        *counts.entry(label).or_default() += 1;
    }
    assert_eq!(counts.get("201"), Some(&30), "{counts:?}");
    assert_eq!(counts.get("insufficient_capacity"), Some(&34), "{counts:?}");
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(payment["payment"]["refunded"], 300);
    assert_eq!(payment["payment"]["status"], "refunded");
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

/// The hot-account race: one merchant account, many concurrent refunds across many
/// payments. 40 payments of 100 captured, 4 concurrent refunds of 30 each: exactly 3 per
/// payment succeed, and the merchant's pending balance ends at 40 × 10.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_hot_merchant_account_takes_concurrent_refunds_across_many_payments() {
    let (sim, app, key) = setup().await;
    let mut pids = Vec::new();
    for n in 0..40 {
        let (_, auth) = app
            .authorize(&key, &format!("p{n}"), 100, "USD", "tok_sim_approve")
            .await;
        let pid = id(&auth);
        app.post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some(&format!("c{n}")),
            json!({"amount": 100}),
        )
        .await;
        pids.push(pid);
    }
    let app = Arc::new(app);
    let mut tasks = Vec::new();
    for (n, pid) in pids.iter().enumerate() {
        for r in 0..4 {
            let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
            tasks.push(tokio::spawn(async move {
                app.post(
                    &format!("/v1/payments/{pid}/refunds"),
                    &key,
                    Some(&format!("r{n}-{r}")),
                    json!({"amount": 30}),
                )
                .await
                .0
            }));
        }
    }
    let mut ok = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            StatusCode::CREATED => ok += 1,
            StatusCode::CONFLICT => refused += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!((ok, refused), (120, 40));
    let (_, balances) = app.get("/v1/balances", &key).await;
    assert_eq!(balance(&balances, "USD", "merchant_pending"), 400);
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_captures_never_exceed_the_authorization() {
    let (sim, app, key) = setup().await;
    let (_, auth) = app
        .authorize(&key, "cc", 100, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    let app = Arc::new(app);
    let mut tasks = Vec::new();
    for n in 0..10 {
        let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
        tasks.push(tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                &key,
                Some(&format!("cap-{n}")),
                json!({"amount": 15}),
            )
            .await
            .0
        }));
    }
    let mut ok = 0;
    for task in tasks {
        if task.await.unwrap() == StatusCode::CREATED {
            ok += 1;
        }
    }
    assert_eq!(ok, 6, "6 × 15 = 90 fits in 100; a seventh would overrun");
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(payment["payment"]["captured"], 90);
    assert_eq!(payment["payment"]["capturable"], 10);
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

/// Capture versus void, with the capture held in flight by a slow network: the void is
/// refused while the capture is open, then releases exactly what is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_void_racing_an_in_flight_capture_waits_for_it_and_then_releases_the_rest() {
    let (sim, app, key) = setup().await;
    let (_, auth) = app
        .authorize(&key, "cv", 100, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    sim.fault(
        json!({"kind": "capture", "target": null, "action": "delay", "delay_ms": 1500, "count": 1}),
    );
    let app = Arc::new(app);
    let capture = {
        let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
        tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                &key,
                Some("cap"),
                json!({"amount": 30}),
            )
            .await
        })
    };
    wait_for_reservation(&app, &key, &pid, "capture").await;
    let (status, reply) = app
        .post(
            &format!("/v1/payments/{pid}/void"),
            &key,
            Some("void"),
            json!({}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_state")),
        "{reply}"
    );
    let (status, _) = capture.await.unwrap();
    assert_eq!(status, StatusCode::CREATED);
    let (status, reply) = app
        .post(
            &format!("/v1/payments/{pid}/void"),
            &key,
            Some("void-2"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    assert_eq!(reply["operation"]["amount"], 70);
    assert_eq!(reply["payment"]["status"], "captured");
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("late"),
            json!({"amount": 1}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "nothing is capturable after the void"
    );
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

/// A refund cannot draw on a capture that is still in flight: only committed captures are
/// refundable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refund_cannot_draw_on_a_capture_still_in_flight() {
    let (sim, app, key) = setup().await;
    let (_, auth) = app
        .authorize(&key, "rp", 100, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    app.post(
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("c1"),
        json!({"amount": 40}),
    )
    .await;
    sim.fault(
        json!({"kind": "capture", "target": null, "action": "delay", "delay_ms": 1500, "count": 1}),
    );
    let app = Arc::new(app);
    let capture = {
        let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
        tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                &key,
                Some("c2"),
                json!({"amount": 60}),
            )
            .await
        })
    };
    wait_for_reservation(&app, &key, &pid, "capture").await;
    let (status, reply) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            &key,
            Some("r1"),
            json!({"amount": 41}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("insufficient_capacity")),
        "{reply}"
    );
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            &key,
            Some("r2"),
            json!({"amount": 40}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the committed 40 is refundable"
    );
    assert_eq!(capture.await.unwrap().0, StatusCode::CREATED);
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            &key,
            Some("r3"),
            json!({"amount": 60}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "once committed, the second capture is refundable"
    );
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

async fn wait_for_reservation(app: &TestApp, key: &str, pid: &str, kind: &str) {
    for _ in 0..200 {
        let (_, payment) = app.get(&format!("/v1/payments/{pid}"), key).await;
        if payment["payment"]["reserved"][kind].as_i64().unwrap_or(0) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the {kind} never reserved its capacity");
}

/// D-019: card data is refused before parsing, and nothing about it is stored anywhere.
#[tokio::test]
async fn card_data_is_rejected_and_never_persisted() {
    let (sim, app, key) = setup().await;
    let pans = ["4242424242424242", "5555 5555 5555 4444", "378282246310005"];
    let bodies = [
        json!({"amount": 100, "currency": "USD", "token": "4242424242424242"}),
        json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "number": "4242424242424242"}),
        json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "card": {"cvc": "123"}}),
        json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "description": "card 5555 5555 5555 4444"}),
        json!({"amount": 4_242_424_242_424_242_i64, "currency": "USD", "token": "tok_sim_approve"}),
    ];
    for (n, body) in bodies.iter().enumerate() {
        let (status, reply) = app
            .post(
                "/v1/payments",
                &key,
                Some(&format!("pan-{n}")),
                body.clone(),
            )
            .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {status} {reply}"
        );
        for pan in pans {
            assert!(!reply.to_string().contains(pan), "echoed {pan}");
        }
    }
    let (status, _) = app
        .post(
            "/v1/payments",
            &key,
            Some("378282246310005"),
            json!({"amount": 1, "currency": "USD", "token": "tok_sim_approve"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let stored: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM payments) + (SELECT count(*) FROM operations)",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(stored, 0, "nothing from a refused request is stored");
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT description AS t FROM payments UNION ALL SELECT idempotency_key FROM operations
                              UNION ALL SELECT label FROM api_keys) x
          WHERE t LIKE '%4242%' OR t LIKE '%5555%' OR t LIKE '%3782%'",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
    assert_eq!(
        sim.store.count().await.unwrap(),
        0,
        "nothing reached the network"
    );
    teardown(sim, app).await;
}

#[tokio::test]
async fn one_project_cannot_read_or_move_anothers_payment() {
    let (sim, app, key_a) = setup().await;
    let (_, key_b) = app.project("store-b").await;
    let (_, auth) = app
        .authorize(&key_a, "a1", 500, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    assert_eq!(
        app.get(&format!("/v1/payments/{pid}"), &key_b).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get(&format!("/v1/payments/{pid}/journal"), &key_b)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for path in ["captures", "refunds"] {
        let (status, _) = app
            .post(
                &format!("/v1/payments/{pid}/{path}"),
                &key_b,
                Some("x"),
                json!({"amount": 1}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    // The same key in another project is a different identity.
    let (status, other) = app
        .authorize(&key_b, "a1", 500, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(id(&other), pid);
    let (_, balances_b) = app.get("/v1/balances", &key_b).await;
    assert_eq!(
        balance(&balances_b, "USD", "payer_held"),
        500,
        "B sees only its own hold"
    );
    app.assert_reconciled(&key_a).await;
    app.assert_reconciled(&key_b).await;
    teardown(sim, app).await;
}

#[tokio::test]
async fn moving_money_needs_the_write_scope_and_a_key() {
    let (sim, app, admin) = setup().await;
    let (_, reader) = app.key(&admin, &["read"]).await;
    let (status, _) = app
        .authorize(&reader, "r", 100, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, reply) = app
        .post(
            "/v1/payments",
            &admin,
            None,
            json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve"}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("idempotency_key_required"))
    );
    assert_eq!(app.get("/v1/balances", &reader).await.0, StatusCode::OK);
    teardown(sim, app).await;
}

/// An unavailable network executes nothing: the operation stays `uncertain` (honestly
/// reported, reservation-free for an authorize), and a retry under the same key drives the
/// same identity to its answer.
#[tokio::test]
async fn an_unavailable_network_leaves_the_operation_uncertain_until_a_retry_resolves_it() {
    let (sim, app, key) = setup().await;
    sim.fault(json!({"kind": "authorize", "target": null, "action": "unavailable", "count": 1}));
    let (status, first) = app
        .authorize(&key, "flaky", 700, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["operation"]["state"], "uncertain");
    assert_eq!(first["payment"]["status"], "authorizing");
    assert_eq!(sim.store.count().await.unwrap(), 0);
    let (status, second) = app
        .authorize(&key, "flaky", 700, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["payment"]["status"], "authorized");
    assert_eq!(second["operation"]["attempts"], 2);
    assert_eq!(sim.store.count().await.unwrap(), 1);
    app.assert_reconciled(&key).await;
    teardown(sim, app).await;
}

/// Random histories: whatever sequence of operations and outcomes, the journal stays
/// balanced and every payment equals its entries (the reconciliation report is clean), and
/// every balance equals the sum the payments imply.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_histories_keep_the_journal_balanced_and_every_payment_equal_to_its_entries() {
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    let (sim, app, key) = setup().await;
    let tokens = [
        "tok_sim_approve",
        "tok_sim_approve",
        "tok_sim_approve",
        "tok_sim_decline_do_not_honor",
    ];
    let currencies = ["USD", "EUR", "JPY", "KWD"];
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let steps = prop::collection::vec(
        (0usize..4, 0usize..6, 1i64..400, 0usize..4, 0usize..4),
        60..90,
    )
    .new_tree(&mut runner)
    .unwrap()
    .current();
    let mut payments: Vec<String> = Vec::new();
    for (n, (op, target, amount, t, c)) in steps.into_iter().enumerate() {
        let idem = format!("step-{n}");
        if op == 0 || payments.is_empty() {
            let (status, reply) = app
                .authorize(&key, &idem, amount * 2, currencies[c], tokens[t])
                .await;
            assert_eq!(status, StatusCode::CREATED, "{reply}");
            payments.push(id(&reply));
            continue;
        }
        let pid = &payments[target % payments.len()];
        let (path, body) = match op {
            1 => ("captures", json!({"amount": amount})),
            2 => ("refunds", json!({"amount": amount})),
            _ => ("void", json!({})),
        };
        let (status, reply) = app
            .post(
                &format!("/v1/payments/{pid}/{path}"),
                &key,
                Some(&idem),
                body,
            )
            .await;
        assert!(
            matches!(status, StatusCode::CREATED | StatusCode::CONFLICT),
            "{status} {reply}"
        );
    }
    app.assert_reconciled(&key).await;
    let (_, balances) = app.get("/v1/balances", &key).await;
    let mut expected: HashMap<(String, &str), i64> = HashMap::new();
    for pid in &payments {
        let (_, p) = app.get(&format!("/v1/payments/{pid}"), &key).await;
        let p = &p["payment"];
        let currency = p["currency"].as_str().unwrap().to_owned();
        let n = |f: &str| p[f].as_i64().unwrap();
        *expected
            .entry((currency.clone(), "payer_held"))
            .or_default() += n("authorized") - n("captured") - n("voided");
        *expected
            .entry((currency.clone(), "merchant_pending"))
            .or_default() += n("captured") - n("refunded");
        *expected.entry((currency, "payer_funding")).or_default() +=
            n("authorized") - n("voided") - n("refunded");
    }
    for ((currency, account), want) in &expected {
        assert_eq!(
            balance(&balances, currency, account),
            *want,
            "{currency} {account}"
        );
    }
    teardown(sim, app).await;
}

/// Regression (first refund-race smoke): waiters for one hot payment must not hold pooled
/// connections, or they starve every other payment. With a pool of 4 and 48 concurrent
/// refunds on one payment, an unrelated payment's operations still succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_hot_payment_cannot_starve_the_pool_for_other_payments() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(
        &sim,
        &[
            ("PS_DB_MAX_CONNECTIONS", "4"),
            // Long enough for 48 concurrent key lookups on a busy machine; without the
            // per-payment queue the cold requests still starve (the blocker holds the hot
            // row until they finish), so the test keeps its teeth (review 14).
            ("PS_DB_ACQUIRE_TIMEOUT_MS", "3000"),
            ("PS_PAYMENT_QUEUE_WAIT_MS", "20000"),
        ],
    )
    .await;
    let (_, key) = app.project("store").await;
    let (_, hot) = app
        .authorize(&key, "hot", 1_000, "USD", "tok_sim_approve")
        .await;
    let (_, cold) = app
        .authorize(&key, "cold", 1_000, "USD", "tok_sim_approve")
        .await;
    let (hot, cold) = (id(&hot), id(&cold));
    app.post(
        &format!("/v1/payments/{hot}/captures"),
        &key,
        Some("hc"),
        json!({"amount": 1000}),
    )
    .await;
    // Another session holds the hot payment's row lock, so its refunds really queue.
    let mut blocker = app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM payments WHERE id = $1::uuid FOR UPDATE")
        .bind(&hot)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let app = Arc::new(app);
    let mut hot_tasks = Vec::new();
    for n in 0..48 {
        let (app, key, hot) = (app.clone(), key.clone(), hot.clone());
        hot_tasks.push(tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{hot}/refunds"),
                &key,
                Some(&format!("hr{n}")),
                json!({"amount": 10}),
            )
            .await
            .0
        }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut cold_statuses = Vec::new();
    for n in 0..5 {
        let (status, reply) = app
            .post(
                &format!("/v1/payments/{cold}/captures"),
                &key,
                Some(&format!("cc{n}")),
                json!({"amount": 10}),
            )
            .await;
        assert_ne!(
            reply["error"]["code"], "unavailable",
            "pool starved: {reply}"
        );
        cold_statuses.push(status);
    }
    assert!(
        cold_statuses.iter().all(|s| *s == StatusCode::CREATED),
        "{cold_statuses:?}"
    );
    blocker.commit().await.unwrap();
    for task in hot_tasks {
        assert_eq!(task.await.unwrap(), StatusCode::CREATED);
    }
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

/// Regression (first refund-race smoke): when recording the network's approval hits a
/// transient database failure (here a lock timeout), it is retried with backoff instead of
/// leaving the operation open with its reservation held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recording_an_approval_survives_a_transient_lock_timeout() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_DB_LOCK_TIMEOUT_MS", "100")]).await;
    let (_, key) = app.project("store").await;
    let (_, auth) = app
        .authorize(&key, "lt", 100, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    sim.fault(
        json!({"kind": "capture", "target": null, "action": "delay", "delay_ms": 200, "count": 1}),
    );
    let app = Arc::new(app);
    let capture = {
        let (app, key, pid) = (app.clone(), key.clone(), pid.clone());
        tokio::spawn(async move {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                &key,
                Some("cap"),
                json!({"amount": 60}),
            )
            .await
        })
    };
    wait_for_reservation(&app, &key, &pid, "capture").await;
    // Another session holds the payment's row lock through the moment the answer arrives.
    let mut blocker = app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM payments WHERE id = $1::uuid FOR UPDATE")
        .bind(&pid)
        .execute(&mut *blocker)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(450)).await;
    blocker.commit().await.unwrap();
    let (status, reply) = capture.await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    assert_eq!(reply["operation"]["state"], "succeeded");
    assert_eq!(reply["payment"]["captured"], 60);
    app.assert_reconciled(&key).await;
    let app = Arc::into_inner(app).unwrap();
    teardown(sim, app).await;
}

/// Review finding 6 (experiment G1): the card shapes the first guard missed. Every one is
/// refused, as a description and as an Idempotency-Key, and none is stored or echoed.
#[tokio::test]
async fn every_common_card_number_shape_is_refused_and_never_stored() {
    let (sim, app, key) = setup().await;
    let shapes = [
        "4242.4242.4242.4242",
        "4242 4242  4242 4242",
        "4242\u{a0}4242\u{a0}4242\u{a0}4242",
        "\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}",
        "4 2 4 2 4 2 4 2 4 2 4 2 4 2 4 2",
        "4242/4242/4242/4242",
        "4242_4242_4242_4242",
        "37828224631000 5",
    ];
    for (n, shape) in shapes.iter().enumerate() {
        let body = json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "description": format!("card {shape}")});
        let (status, reply) = app
            .post("/v1/payments", &key, Some(&format!("g{n}")), body)
            .await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("raw_card_data_rejected")
            ),
            "{shape:?}: {reply}"
        );
    }
    let (status, reply) = app
        .post(
            "/v1/payments",
            &key,
            Some("4242.4242.4242.4242"),
            json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reply}");
    // UUIDs, dates and ordinary numbers still pass.
    let (status, reply) = app
        .post(
            "/v1/payments",
            &key,
            Some("018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01"),
            json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "description": "order 1234 of 2026-11-16, ref 018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT description AS t FROM payments UNION ALL SELECT idempotency_key FROM operations) x
          WHERE t LIKE '%4242%' OR t LIKE '%3782%' OR t LIKE '%\u{ff14}%'",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);
    teardown(sim, app).await;
}

/// Review finding 7: a card number sent as a JSON number is refused as card data, and no
/// schema error ever echoes the client's input.
#[tokio::test]
async fn card_numbers_as_json_numbers_are_refused_and_schema_errors_never_echo_input() {
    let (sim, app, key) = setup().await;
    let bodies = [
        json!({"amount": 100, "currency": "USD", "token": 4_242_424_242_424_242_i64}),
        json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve", "description": 4_000_056_655_665_556_i64}),
        json!({"amount": 100, "currency": 5_555_555_555_554_444_i64, "token": "tok_sim_approve"}),
    ];
    for (n, body) in bodies.into_iter().enumerate() {
        let (status, reply) = app
            .post("/v1/payments", &key, Some(&format!("n{n}")), body)
            .await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("raw_card_data_rejected")
            ),
            "{reply}"
        );
        for digits in ["4242424242424242", "4000056655665556", "5555555555554444"] {
            assert!(!reply.to_string().contains(digits), "echoed: {reply}");
        }
    }
    let (status, reply) = app
        .post(
            "/v1/payments",
            &key,
            Some("t"),
            json!({"amount": "ten thousand", "currency": "USD", "token": "tok_sim_approve"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        !reply.to_string().contains("ten thousand"),
        "a schema error echoed the input: {reply}"
    );
    teardown(sim, app).await;
}
