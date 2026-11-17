//! The logical-restore rehearsal (D-026, D-053; review finding 16's "restore re-posts
//! history" is D-050). A live database is backed up with `pg_dump` and restored into a
//! fresh database by `scripts/restore-rehearsal.sh`, which proves the copy's fingerprint
//! (row counts, journal digests, triggers, pinned search paths, owners, grants, sequences)
//! equals the source's and that every entry balances. Then a server runs on the copy: the
//! role check passes, the reconciliation report, balances and explanation are the same, the
//! journal still refuses changes, and new postings follow the restored history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, TestSim, sim_vars};
use sandbox_db::testing::TestDb;
use serde_json::json;

fn id(reply: &serde_json::Value) -> String {
    reply["payment"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one rehearsal, read top to bottom: build, back up, restore, verify"
)]
async fn a_logical_restore_is_the_same_ledger_with_the_same_protections() {
    let sim = TestSim::start().await;
    let vars = sim_vars(&sim, &[]);
    let source = TestApp::on(TestDb::new().await, &vars);
    let (_, key) = source.project("store").await;

    // A small ledger: captures, a refund, a void and a decline.
    let (_, p0) = source
        .authorize(&key, "p0", 1_000, "USD", "tok_sim_approve")
        .await;
    let p0 = id(&p0);
    for (path, idem, body) in [
        (
            format!("/v1/payments/{p0}/captures"),
            "c0",
            json!({"amount": 600}),
        ),
        (
            format!("/v1/payments/{p0}/refunds"),
            "r0",
            json!({"amount": 100}),
        ),
    ] {
        let (status, reply) = source.post(&path, &key, Some(idem), body).await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
    }
    let (_, p1) = source
        .authorize(&key, "p1", 500, "EUR", "tok_sim_approve")
        .await;
    let (status, _) = source
        .post(
            &format!("/v1/payments/{}/void", id(&p1)),
            &key,
            Some("v1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = source
        .authorize(&key, "p2", 700, "USD", "tok_sim_decline_do_not_honor")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    for n in 3..6 {
        let (_, p) = source
            .authorize(&key, &format!("p{n}"), 300, "KWD", "tok_sim_approve")
            .await;
        let (status, _) = source
            .post(
                &format!("/v1/payments/{}/captures", id(&p)),
                &key,
                Some(&format!("c{n}")),
                json!({"amount": 300}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let before = source.assert_reconciled(&key).await;
    let (_, balances_before) = source.get("/v1/balances", &key).await;
    let explain = "/v1/balances/USD/merchant_pending/explain?limit=1000";
    let (_, explained_before) = source.get(explain, &key).await;

    // The backup is of the live database: the server's pool stays open.
    let target = format!("{}_restored", source.db.name());
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/restore-rehearsal.sh"
    );
    let rehearsal = std::process::Command::new(script)
        .arg(source.db.name())
        .arg(&target)
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&rehearsal.stdout),
        String::from_utf8_lossy(&rehearsal.stderr)
    );
    assert!(rehearsal.status.success(), "the rehearsal failed: {said}");
    assert!(said.contains("restore ok"), "{said}");

    let restored = TestApp::on(source.db.attach(&target).await, &vars);
    // A server may run on it: the runtime role is still neither superuser nor owner.
    sandbox_db::roles::check_runtime(&restored.state.db)
        .await
        .unwrap();
    let after = restored.assert_reconciled(&key).await;
    for field in ["payments_checked", "entries_checked", "open_operations"] {
        assert_eq!(after["report"][field], before["report"][field], "{field}");
    }
    let (_, balances_after) = restored.get("/v1/balances", &key).await;
    assert_eq!(balances_after["balances"], balances_before["balances"]);
    let (_, explained_after) = restored.get(explain, &key).await;
    assert_eq!(
        explained_after["explanation"]["lines"],
        explained_before["explanation"]["lines"]
    );
    // A restore is a new generation: cursors from the source are not valid here (D-050).
    assert_ne!(
        explained_after["explanation"]["generation"],
        explained_before["explanation"]["generation"]
    );

    // The journal's protections came back: even its owner cannot change a line.
    let refused = sqlx::query("UPDATE journal_lines SET amount_minor = amount_minor + 1")
        .execute(&restored.db.pool)
        .await
        .unwrap_err();
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("PSJ01"),
        "{refused}"
    );

    // Life goes on: a new posting follows every restored one.
    let max_before: i64 = sqlx::query_scalar("SELECT max(seq) FROM journal_entries")
        .fetch_one(&restored.db.pool)
        .await
        .unwrap();
    let (status, _) = restored
        .authorize(&key, "after-restore", 250, "USD", "tok_sim_approve")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let max_after: i64 = sqlx::query_scalar("SELECT max(seq) FROM journal_entries")
        .fetch_one(&restored.db.pool)
        .await
        .unwrap();
    assert!(max_after > max_before);
    restored.assert_reconciled(&key).await;
    restored.cleanup().await;
    source.cleanup().await;
    sim.remove().await;
}
