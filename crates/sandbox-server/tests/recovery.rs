//! M3: uncertain outcomes and recovery, against a real simulated network.
//!
//! - A lost reply stays `uncertain` until a fenced readback, then applies exactly once.
//! - An identity the network never executed is fenced; its reservation is released and a
//!   late delivery is refused.
//! - The network restarts mid-operation and its record survives.
//! - Drill (b), the simulator half: virtual-clock jumps leave posted history unchanged.
//! - Drill (c): the server's database restored from an older backup while the network is
//!   newer; reverse reconciliation reports the gap and client retries close it with no
//!   second charge.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

/// The network deadline of the tests that lose replies on purpose: under the networks' 3 s
/// hold, so a lost reply stays lost, and far above a durable write on a busy CI runner (one
/// took over 300 ms), so an answer is never mistaken for a lost one.
const SIM_DEADLINE_MS: &str = "2000";

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, TestSim, sim_vars};
use sandbox_db::testing::TestDb;
use sandbox_domain::network::ExecuteRequest;
use sandbox_domain::{Currency, OperationKind};
use sandbox_server::reconciler;
use serde_json::{Value, json};

fn id(reply: &Value) -> String {
    reply["payment"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_lost_reply_stays_uncertain_until_readback_and_then_applies_once() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_SIM_TIMEOUT_MS", SIM_DEADLINE_MS)]).await;
    let (_, key) = app.project("store").await;
    let (status, first) = app
        .authorize(&key, "lost", 800, "USD", "tok_sim_lost_reply")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["operation"]["state"], "uncertain");
    assert_eq!(first["payment"]["status"], "authorizing");
    let report = app.assert_reconciled(&key).await;
    assert_eq!(
        report["report"]["open_operations"], 1,
        "uncertain is reported, not hidden"
    );

    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(
        (sweep.examined, sweep.resolved_from_record),
        (1, 1),
        "{sweep:?}"
    );
    let pid = id(&first);
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(payment["payment"]["status"], "authorized");
    // The client's retry replays; nothing executes twice anywhere.
    let (status, again) = app
        .authorize(&key, "lost", 800, "USD", "tok_sim_lost_reply")
        .await;
    assert_eq!(
        (status, again["replayed"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert_eq!(sim.store.count().await.unwrap(), 1);
    let again = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(again.examined, 0, "nothing left open");
    let report = app.assert_reconciled(&key).await;
    assert_eq!(report["report"]["entries_checked"], 1);
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn a_capture_whose_reply_is_lost_keeps_its_reservation_until_readback() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_SIM_TIMEOUT_MS", SIM_DEADLINE_MS)]).await;
    let (_, key) = app.project("store").await;
    let (_, auth) = app
        .authorize(&key, "a", 100, "EUR", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    sim.fault(json!({"kind": "capture", "target": null, "action": "lost_reply", "count": 1}));
    let (status, cap) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 70}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cap}");
    assert_eq!(
        cap["payment"]["reserved"]["capture"], 70,
        "the reservation is held while uncertain"
    );
    // The held reservation still counts: a capture of the unreserved remainder plus one fails.
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c2"),
            json!({"amount": 31}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    reconciler::sweep(&app.state).await.unwrap();
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(payment["payment"]["captured"], 70);
    assert_eq!(payment["payment"]["reserved"]["capture"], 0);
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn an_identity_the_network_never_executed_is_fenced_and_its_reservation_released() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let (_, auth) = app
        .authorize(&key, "a", 100, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    sim.fault(json!({"kind": "capture", "target": null, "action": "unavailable", "count": 1}));
    let (status, cap) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 40}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cap}");
    let operation_id: uuid::Uuid = cap["operation"]["id"].as_str().unwrap().parse().unwrap();
    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(sweep.fenced, 1, "{sweep:?}");
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(payment["payment"]["reserved"]["capture"], 0, "released");
    assert_eq!(payment["payment"]["capturable"], 100);
    let state = payment["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "capture")
        .unwrap()["state"]
        .clone();
    assert_eq!(state, "not_executed");
    // A delayed delivery of the fenced identity can never execute now.
    let (_, project) = app.get("/v1/project", &key).await;
    let late = ExecuteRequest {
        id: operation_id,
        project_id: project["project"]["id"].as_str().unwrap().parse().unwrap(),
        kind: OperationKind::Capture,
        parent_id: Some(
            auth["payment"]["authorize_operation_id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
        ),
        amount_minor: 40,
        currency: Currency::Usd,
        token: None,
    };
    assert!(matches!(
        sim.store.execute(&late).await,
        Err(sandbox_sim::store::StoreError::Fenced)
    ));
    // The same key replays the honest result; a new key captures normally.
    let (status, replay) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c1"),
            json!({"amount": 40}),
        )
        .await;
    assert_eq!(
        (status, replay["operation"]["state"].as_str()),
        (StatusCode::OK, Some("not_executed"))
    );
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("c2"),
            json!({"amount": 40}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "authorize and the second capture only"
    );
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}

/// The network restarts after it executed an operation whose reply it never sent: its
/// record survives the restart, and readback applies it once.
#[tokio::test]
async fn the_networks_record_survives_its_restart_mid_operation() {
    let sim = TestSim::start().await;
    let addr = sim.addr();
    let app = TestApp::with_sim(
        &sim,
        &[
            ("PS_SIM_TIMEOUT_MS", "300"),
            ("PS_RECONCILE_INTERVAL_MS", "200"),
        ],
    )
    .await;
    let (_, key) = app.project("store").await;
    let (status, first) = app
        .authorize(&key, "r", 450, "JPY", "tok_sim_lost_reply")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let path = sim.stop().await;
    // While the network is down, readback fails and the operation stays open, honestly.
    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(sweep.left_open, 1, "{sweep:?}");
    let sim = TestSim::start_on(&addr, path, Duration::from_secs(3)).await;
    // A failed check waits out its backoff (one sweep interval, D-048) before the next.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(sweep.resolved_from_record, 1, "{sweep:?}");
    let (_, payment) = app.get(&format!("/v1/payments/{}", id(&first)), &key).await;
    assert_eq!(payment["payment"]["status"], "authorized");
    assert_eq!(sim.store.count().await.unwrap(), 1);
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}

async fn journal_digest(app: &TestApp) -> String {
    sqlx::query_scalar(
        "SELECT md5(string_agg(e.id::text || '|' || e.seq || '|' || e.recipe || '|' || e.currency || '|'
                               || e.posted_at::text || '|' || l.line_no || '|' || l.account_kind || '|' || l.amount_minor,
                               ',' ORDER BY e.seq, l.line_no))
           FROM journal_entries e JOIN journal_lines l ON l.project_id = e.project_id AND l.entry_id = e.id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

/// Drill (b), simulator half (D-026): the virtual clock jumps 90 days forward and 180 back;
/// posted journal history and the network's records are byte-identical afterwards, and the
/// sandbox keeps working.
#[tokio::test]
async fn drill_b_virtual_clock_jumps_never_change_posted_history() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let (_, auth) = app
        .authorize(&key, "a", 1_000, "KWD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    app.post(
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("c"),
        json!({"amount": 600}),
    )
    .await;
    app.post(
        &format!("/v1/payments/{pid}/refunds"),
        &key,
        Some("r"),
        json!({"amount": 250}),
    )
    .await;
    let journal_before = journal_digest(&app).await;
    let network_before = sim.store.list(0, 1_000).await.unwrap();

    let http = reqwest::Client::new();
    for seconds in [90 * 86_400_i64, -180 * 86_400] {
        let response = http
            .post(format!("{}/v1/clock", sim.url))
            .bearer_auth(common::SIM_CONTROL_TOKEN)
            .json(&json!({"seconds": seconds}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
    }
    assert_eq!(
        journal_digest(&app).await,
        journal_before,
        "posted history changed under a clock jump"
    );
    assert_eq!(
        sim.store.list(0, 1_000).await.unwrap(),
        network_before,
        "the network's records changed"
    );

    let (status, after) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            &key,
            Some("r2"),
            json!({"amount": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{after}");
    let listed = sim.store.list(0, 1_000).await.unwrap();
    let newest = listed.operations.last().unwrap();
    assert_eq!(
        newest.virtual_time,
        sandbox_sim::store::CLOCK_EPOCH - 90 * 86_400,
        "new work uses the jumped clock"
    );
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}

/// Drill (c) (D-026): the server's database is restored from a backup taken before two
/// operations, while the network keeps its newer records. Reverse reconciliation reports
/// exactly the gap; client retries under the same keys re-derive the same identities and
/// apply the network's records once — no second charge, no second capture.
#[tokio::test]
async fn drill_c_restoring_an_older_server_backup_reconciles_without_a_second_charge() {
    let sim = TestSim::start().await;
    let vars = sim_vars(&sim, &[]);
    let first = TestApp::on(TestDb::new().await, &vars);
    let (_, key) = first.project("store").await;
    let (_, p1) = first
        .authorize(&key, "order-1", 500, "USD", "tok_sim_approve")
        .await;
    let p1 = id(&p1);

    // Backup, taken with no connection open.
    first.state.db.close().await;
    let TestApp { db: mut live, .. } = first;
    let backup = live.snapshot().await;

    // Life goes on after the backup: a second purchase and a capture of the first.
    let after = TestApp::on(live, &vars);
    let (_, p2) = after
        .authorize(&key, "order-2", 900, "USD", "tok_sim_approve")
        .await;
    let p2 = id(&p2);
    let (status, _) = after
        .post(
            &format!("/v1/payments/{p1}/captures"),
            &key,
            Some("cap-1"),
            json!({"amount": 300}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(sim.store.count().await.unwrap(), 3);

    // Disaster: the server comes back on the older backup; the network is newer.
    after.state.db.close().await;
    let restored = TestApp::on(backup, &vars);
    let (_, report) = restored.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(report["clean"], false, "{report}");
    assert_eq!(
        report["network"]["unknown_to_server"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{report}"
    );
    assert_eq!(
        restored.get(&format!("/v1/payments/{p2}"), &key).await.0,
        StatusCode::NOT_FOUND
    );

    // The client retries with the keys it already used.
    let (status, again) = restored
        .authorize(&key, "order-2", 900, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(id(&again), p2, "the same key derives the same payment");
    assert_eq!(again["payment"]["status"], "authorized");
    let (status, cap) = restored
        .post(
            &format!("/v1/payments/{p1}/captures"),
            &key,
            Some("cap-1"),
            json!({"amount": 300}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{cap}");
    assert_eq!(cap["payment"]["captured"], 300);
    assert_eq!(
        sim.store.count().await.unwrap(),
        3,
        "no second charge and no second capture at the network"
    );

    let (_, report) = restored.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(report["clean"], true, "{report}");
    assert_eq!(
        report["network"]["unknown_to_server"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    after.cleanup().await;
    restored.cleanup().await;
    sim.remove().await;
}

/// Regression (first M3 load smoke): reverse reconciliation pages through the network's
/// listing; a full page of 1,000 operations must parse (it exceeded the single-answer cap).
#[tokio::test]
async fn reverse_reconciliation_pages_through_more_than_one_full_listing_page() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let (_, project) = app.get("/v1/project", &key).await;
    let project_id: uuid::Uuid = project["project"]["id"].as_str().unwrap().parse().unwrap();
    // 1,050 network operations of this project that the server never recorded (as after a
    // restore of an old backup), written straight into the network's store.
    for n in 0..1_050_u128 {
        let request = ExecuteRequest {
            id: uuid::Uuid::from_u128(0xfeed_0000 + n),
            project_id,
            kind: OperationKind::Authorize,
            parent_id: None,
            amount_minor: 100,
            currency: Currency::Usd,
            token: Some("tok_sim_approve".to_owned()),
        };
        sim.store.execute(&request).await.unwrap();
    }
    let (status, report) = app.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["clean"], false);
    assert_eq!(
        report["network"]["unknown_to_server"]
            .as_array()
            .unwrap()
            .len(),
        1_000,
        "bounded at 1,000"
    );
    app.cleanup().await;
    sim.remove().await;
}

/// Review finding 3 (experiment R1): after the server's database is restored, the same key
/// with a different amount must be a conflict — never the network's outcome applied to the
/// server's new amount.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one restore scenario, read top to bottom"
)]
async fn after_a_restore_the_same_key_with_another_amount_is_a_conflict_not_a_reinterpretation() {
    let sim = TestSim::start().await;
    let vars = sim_vars(&sim, &[]);
    let first = TestApp::on(TestDb::new().await, &vars);
    let (_, key) = first.project("store").await;
    let (_, p1) = first
        .authorize(&key, "order-1", 1_000, "USD", "tok_sim_approve")
        .await;
    let p1 = id(&p1);
    first.state.db.close().await;
    let TestApp { db: mut live, .. } = first;
    let backup = live.snapshot().await;

    let after = TestApp::on(live, &vars);
    let (status, p9) = after
        .authorize(&key, "order-9", 100, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let p9 = id(&p9);
    let (status, _) = after
        .post(
            &format!("/v1/payments/{p1}/captures"),
            &key,
            Some("cap-x"),
            json!({"amount": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    after.state.db.close().await;

    let restored = TestApp::on(backup, &vars);
    let (status, reply) = restored
        .authorize(&key, "order-9", 500, "USD", "tok_sim_approve")
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_key_conflict")),
        "{reply}"
    );
    let (status, reply) = restored
        .post(
            &format!("/v1/payments/{p1}/captures"),
            &key,
            Some("cap-x"),
            json!({"amount": 700}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_key_conflict")),
        "{reply}"
    );
    let sweep = reconciler::sweep(&restored.state).await.unwrap();
    assert_eq!(
        sweep.examined, 0,
        "nothing is left open to be reinterpreted: {sweep:?}"
    );

    let (_, payment) = restored.get(&format!("/v1/payments/{p9}"), &key).await;
    assert_eq!(payment["payment"]["authorized"], 0, "{payment}");
    assert_eq!(
        payment["payment"]["decline_code"],
        "network_identity_conflict"
    );
    let (_, payment) = restored.get(&format!("/v1/payments/{p1}"), &key).await;
    assert_eq!(payment["payment"]["captured"], 0, "{payment}");
    assert_eq!(
        payment["payment"]["reserved"]["capture"], 0,
        "the reservation is released"
    );
    assert_eq!(
        sim.store.count().await.unwrap(),
        3,
        "the network executed nothing new"
    );
    let (_, report) = restored.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(
        report["clean"], false,
        "server 500/700 vs network 100/100 is not clean: {report}"
    );
    // Each conflicted operation differs on its amount and on its outcome.
    let fields: std::collections::BTreeSet<(String, String)> = report["network"]["disagreements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["field"].as_str().unwrap().to_owned(),
                d["server"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("amount_minor", "500"),
            ("amount_minor", "700"),
            ("outcome", "conflict")
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect(),
        "{report}"
    );
    after.cleanup().await;
    restored.cleanup().await;
    sim.remove().await;
}

async fn fill_network(sim: &TestSim, project_id: uuid::Uuid, count: u128, base: u128) {
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(2_000));
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..count {
        let store = sim.store.clone();
        let permit = permits.clone().acquire_owned().await.unwrap();
        tasks.spawn(async move {
            let request = ExecuteRequest {
                id: uuid::Uuid::from_u128(base + n),
                project_id,
                kind: OperationKind::Authorize,
                parent_id: None,
                amount_minor: 100,
                currency: Currency::Usd,
                token: Some("tok_sim_approve".to_owned()),
            };
            loop {
                match store.execute(&request).await {
                    Err(sandbox_sim::store::StoreError::Overloaded) => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    other => {
                        other.unwrap();
                        break;
                    }
                }
            }
            drop(permit);
        });
    }
    while let Some(done) = tasks.join_next().await {
        done.unwrap();
    }
}

/// Review finding 4 (experiment N1): agreement with the network is scoped to the project,
/// scanned to completion (or reported incomplete, never clean), and checked both ways,
/// field by field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_agreement_is_project_scoped_complete_and_checked_both_ways() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let (_, project) = app.get("/v1/project", &key).await;
    let project_id: uuid::Uuid = project["project"]["id"].as_str().unwrap().parse().unwrap();
    // 100,000 operations of another project, then one of this project the server never saw.
    fill_network(&sim, uuid::Uuid::from_u128(0xb), 100_000, 0x1_0000_0000).await;
    fill_network(&sim, project_id, 1, 0x9_0000_0000).await;
    let (status, report) = app.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["clean"], false, "{report}");
    assert_eq!(report["network"]["complete"], true, "{report}");
    assert_eq!(
        report["network"]["unknown_to_server"],
        json!([uuid::Uuid::from_u128(0x9_0000_0000)]),
        "{report}"
    );

    // Server successes the network never recorded: point the same database at an empty network.
    let (_, auth) = app
        .authorize(&key, "a", 300, "USD", "tok_sim_approve")
        .await;
    let pid = id(&auth);
    app.post(
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("c"),
        json!({"amount": 300}),
    )
    .await;
    let empty = TestSim::start().await;
    let TestApp { db, .. } = app;
    let elsewhere = TestApp::on(db, &sim_vars(&empty, &[]));
    let (_, report) = elsewhere.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(report["clean"], false, "{report}");
    assert_eq!(
        report["network"]["unknown_to_network"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{report}"
    );

    // A scan that runs out of pages says so and is never clean.
    let TestApp { db, .. } = elsewhere;
    let capped = TestApp::on(
        db,
        &sim_vars(&sim, &[("PS_RECONCILE_NETWORK_MAX_PAGES", "1")]),
    );
    fill_network(&sim, project_id, 1_000, 0xa_0000_0000).await;
    let (_, report) = capped.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(
        (
            report["clean"].as_bool(),
            report["network"]["complete"].as_bool()
        ),
        (Some(false), Some(false)),
        "{report}"
    );
    capped.cleanup().await;
    sim.remove().await;
    empty.remove().await;
}
