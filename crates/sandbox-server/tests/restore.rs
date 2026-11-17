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

/// Advances the cluster behind `admin` so that the next transaction id it assigns is `next`
/// (subtransactions each take one), if it is not past it already. Returns false when it is.
async fn advance_xids_to(admin: &mut sqlx::PgConnection, next: i64) -> bool {
    use sqlx::Executor as _;
    let current: i64 = sqlx::query_scalar("SELECT pg_current_xact_id()::text::bigint")
        .fetch_one(&mut *admin)
        .await
        .unwrap();
    // The block's own transaction takes `current + 1`; each loop takes one more.
    let burn = next - current - 2;
    if burn < 0 {
        return burn == -1;
    }
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "CREATE TEMP TABLE IF NOT EXISTS burn (x int);
             DO $$ BEGIN FOR i IN 1..{burn} LOOP
                 BEGIN INSERT INTO pg_temp.burn VALUES (1); RAISE EXCEPTION 'burn';
                 EXCEPTION WHEN raise_exception THEN NULL; END;
             END LOOP; END $$;"
        )))
        .await
        .unwrap();
    true
}

/// Review N2: `created_xact` holds transaction ids, which mean something only inside one
/// cluster. Restored into another cluster, explain filtered every restored entry out as
/// "not yet committed" (0 lines and a 0 balance while `/v1/balances` showed the money and
/// reconciliation was clean), and once the new cluster's counter reached a restored entry's
/// `created_xact`, the runtime role added lines to that posted entry. The restored history
/// must explain exactly as before, and stay closed whatever the counter says.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one cross-cluster restore, read top to bottom"
)]
async fn a_restore_into_another_cluster_explains_the_same_and_keeps_posted_entries_closed() {
    use sqlx::Connection as _;
    let other = std::env::var("PS_TEST_RESTORE_DATABASE_URL").expect(
        "PS_TEST_RESTORE_DATABASE_URL names the second cluster's administrator (scripts/stack.sh env)",
    );
    let other_container = std::env::var("PS_PG_RESTORE_CONTAINER")
        .unwrap_or_else(|_| "payment-sandbox-postgres-restore-1".to_owned());
    let sim = TestSim::start().await;
    let vars = sim_vars(&sim, &[]);
    let source = TestApp::on(TestDb::new().await, &vars);
    let (_, key) = source.project("store").await;
    let (_, p0) = source
        .authorize(&key, "x0", 1_000, "USD", "tok_sim_approve")
        .await;
    let p0 = id(&p0);
    for (path, idem, body) in [
        (
            format!("/v1/payments/{p0}/captures"),
            "xc",
            json!({"amount": 600}),
        ),
        (
            format!("/v1/payments/{p0}/refunds"),
            "xr",
            json!({"amount": 100}),
        ),
    ] {
        let (status, reply) = source.post(&path, &key, Some(idem), body).await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
    }
    source
        .authorize(&key, "x1", 250, "USD", "tok_sim_approve")
        .await;

    // The other cluster: its own roles, then the copy.
    let mut admin = sqlx::PgConnection::connect(&other).await.unwrap();
    sandbox_db::roles::bootstrap(&mut admin).await.unwrap();
    let target = format!("ps_test_{}", uuid::Uuid::now_v7().simple());
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/restore-rehearsal.sh"
    );
    let rehearsal = std::process::Command::new(script)
        .arg(source.db.name())
        .arg(&target)
        .env("PS_PG_TARGET_CONTAINER", &other_container)
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&rehearsal.stdout),
        String::from_utf8_lossy(&rehearsal.stderr)
    );
    assert!(rehearsal.status.success(), "the rehearsal failed: {said}");
    let restored = TestApp::on(TestDb::existing(&other, &target).await, &vars);

    // Every balance explains to the same lines on the copy.
    let (_, balances) = restored.get("/v1/balances", &key).await;
    for balance in balances["balances"].as_array().unwrap() {
        let account = balance["account"].as_str().unwrap();
        let currency = balance["currency"].as_str().unwrap();
        let path = format!("/v1/balances/{currency}/{account}/explain?limit=1000");
        let (_, here) = restored.get(&path, &key).await;
        let (_, there) = source.get(&path, &key).await;
        assert_eq!(
            here["balance_minor"], balance["balance_minor"],
            "{account}: {here}"
        );
        assert_eq!(
            here["explanation"]["lines"], there["explanation"]["lines"],
            "{account}"
        );
    }

    // The copy's counter walks up to each restored entry's created_xact; at each, the
    // runtime role tries to add a balanced pair of lines to that posted entry.
    let entries: Vec<(i64, uuid::Uuid, uuid::Uuid, String, i32)> = sqlx::query_as(
        "SELECT created_xact::text::bigint, project_id, id, currency, dataset_epoch
           FROM journal_entries ORDER BY created_xact DESC LIMIT 3",
    )
    .fetch_all(&restored.db.pool)
    .await
    .unwrap();
    let mut entries = entries;
    entries.reverse();
    let mut hits = 0;
    for (xact, project, entry, currency, epoch) in entries {
        if !advance_xids_to(&mut admin, xact).await {
            continue;
        }
        let mut tx = restored.state.db.begin().await.unwrap();
        let now: i64 = sqlx::query_scalar("SELECT pg_current_xact_id()::text::bigint")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        if now != xact {
            continue;
        }
        hits += 1;
        let late = sqlx::query(
            "INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             VALUES ($1, $2, 7, $3, $4, 'payer_funding', 99999), ($1, $2, 8, $3, $4, 'payer_held', -99999)",
        )
        .bind(project)
        .bind(entry)
        .bind(&currency)
        .bind(epoch)
        .execute(&mut *tx)
        .await;
        assert!(
            late.is_err(),
            "the runtime role added lines to a restored, posted entry at xid {xact}"
        );
        let _ = tx.rollback().await;
    }
    assert!(
        hits > 0,
        "the counter never landed on a restored entry's xid"
    );
    restored.assert_reconciled(&key).await;
    admin.close().await.ok();
    restored.cleanup().await;
    source.cleanup().await;
    sim.remove().await;
}
