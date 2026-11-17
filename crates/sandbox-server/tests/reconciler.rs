//! Review finding 11: the reconciler took the 200 oldest open operations of *every* project
//! and read them back one by one, and an operation whose readback failed was picked first
//! again on every sweep. One project's backlog (or its unreachable operations) starved every
//! other project's recovery while their reservations stayed held. Now a sweep takes the
//! projects in turn, a failed check backs off, and readbacks run concurrently, bounded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, TestSim, sim_vars};
use sandbox_server::reconciler;
use serde_json::{Value, json};

fn authorize_state(payment: &Value) -> String {
    payment["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "authorize")
        .unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn one_projects_backlog_cannot_starve_another_projects_recovery() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_RECONCILE_BATCH", "5")]).await;
    let (_, busy) = app.project("backlog").await;
    let (_, quiet) = app.project("quiet").await;
    // The network is unavailable for 21 authorizations: each stays uncertain.
    sim.fault(json!({"kind": "authorize", "target": null, "action": "unavailable", "count": 21}));
    for n in 0..20 {
        let (status, body) = app
            .authorize(&busy, &format!("b-{n}"), 100, "USD", "tok_sim_approve")
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (status, body) = app
        .authorize(&quiet, "q-1", 100, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let quiet_payment = body["payment"]["id"].as_str().unwrap().to_owned();

    // One bounded sweep: five operations, and the quiet project's is among them although
    // twenty of the backlog's are older.
    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(sweep.examined, 5, "{sweep:?}");
    let (_, payment) = app
        .get(&format!("/v1/payments/{quiet_payment}"), &quiet)
        .await;
    assert_eq!(authorize_state(&payment), "not_executed", "{payment}");
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn a_failed_check_backs_off_instead_of_heading_every_sweep() {
    let sim = TestSim::start().await;
    let addr = sim.addr();
    let app = TestApp::with_sim(&sim, &[("PS_RECONCILE_INTERVAL_MS", "400")]).await;
    let (_, key) = app.project("store").await;
    sim.fault(json!({"kind": "authorize", "target": null, "action": "unavailable", "count": 1}));
    let (status, body) = app
        .authorize(&key, "a-1", 100, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let pid = body["payment"]["id"].as_str().unwrap().to_owned();

    // The network goes away: the readback fails and the operation stays open...
    let path = sim.stop().await;
    let first = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!((first.examined, first.left_open), (1, 1), "{first:?}");
    // ...and is not taken again at once: it waits out its backoff.
    let again = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(again.examined, 0, "{again:?}");

    // The network is back; after the backoff the operation is resolved.
    let sim = TestSim::start_on(&addr, path, Duration::from_secs(3)).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    let later = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!((later.examined, later.fenced), (1, 1), "{later:?}");
    let (_, payment) = app.get(&format!("/v1/payments/{pid}"), &key).await;
    assert_eq!(authorize_state(&payment), "not_executed", "{payment}");
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn sweeps_read_back_concurrently_within_their_bound() {
    let sim = TestSim::start().await;
    let app = TestApp::on(
        sandbox_db::testing::TestDb::new().await,
        &sim_vars(&sim, &[("PS_RECONCILE_CONCURRENCY", "4")]),
    );
    let (_, key) = app.project("store").await;
    sim.fault(json!({"kind": "authorize", "target": null, "action": "unavailable", "count": 12}));
    for n in 0..12 {
        let (status, _) = app
            .authorize(&key, &format!("c-{n}"), 100, "USD", "tok_sim_approve")
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let sweep = reconciler::sweep(&app.state).await.unwrap();
    assert_eq!((sweep.examined, sweep.fenced), (12, 12), "{sweep:?}");
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}
