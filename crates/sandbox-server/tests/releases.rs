//! Migration and restore tests per release (D-072; `END_GOAL.md`, "Release and upkeep").
//!
//! Each release leaves a fixture, `fixtures/releases/<version>.json`: every row of a
//! database that release's code wrote (two projects, purchases, a decline, a lost reply
//! resolved by readback, a void, refunds, a checkout session, webhook events and deliveries,
//! and a dataset reset), its last migration, and every account's balance per project and
//! epoch. The test restores each fixture into a database migrated only as far as its release,
//! applies this release's remaining migrations, and requires the result to be one this
//! release's server accepts: the role and protections check passes, every epoch reconciles
//! with the same balances, every balance explains line by line, a request repeated under
//! an old idempotency key replays its stored answer, and new money moves.
//!
//! A new release adds its fixture (`PS_WRITE_RELEASE_FIXTURE=<version> cargo test -p
//! sandbox-server --test releases -- --ignored`) and keeps every earlier one: a migration
//! that cannot apply to an earlier release's data fails here before it ships.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use sandbox_db::ProjectScope;
use sandbox_db::testing::TestDb;
use serde_json::{Value, json};
use uuid::Uuid;

/// Every release with a fixture, oldest first. Never remove one.
// 1.0.0 is the first release (v1.0.0). Its fixture is at its own newest migration, so today
// no migration is applied to it; the first migration after v1.0.0 is the first one tested
// against a release's data (D-072, D-087).
const RELEASES: &[&str] = &["1.0.0"];

/// Tables in an order that satisfies every foreign key.
const TABLES: &[&str] = &[
    "projects",
    "accounts",
    "api_keys",
    "audit_events",
    "payments",
    "operations",
    "journal_entries",
    "journal_lines",
    "webhook_endpoints",
    "events",
    "deliveries",
    "checkout_sessions",
    "checkout_attempts",
];

const SECRET_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn fixture_path(version: &str) -> String {
    format!(
        "{}/../../fixtures/releases/{version}.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Balances of every project and epoch: `{project: {epoch: {"USD/merchant_pending": n}}}`.
async fn balances(pool: &sqlx::PgPool) -> Value {
    let rows: Vec<(Uuid, i32, String, String, i64)> = sqlx::query_as(
        "SELECT project_id, dataset_epoch, currency, account_kind, sum(amount_minor)::bigint
           FROM journal_lines GROUP BY 1, 2, 3, 4 HAVING sum(amount_minor) <> 0 ORDER BY 1, 2, 3, 4",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out: BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>> = BTreeMap::new();
    for (project, epoch, currency, account, sum) in rows {
        out.entry(project.to_string())
            .or_default()
            .entry(epoch.to_string())
            .or_default()
            .insert(format!("{currency}/{account}"), sum);
    }
    json!(out)
}

/// The scopes (project, epoch) that hold any payment.
async fn scopes(pool: &sqlx::PgPool) -> Vec<ProjectScope> {
    let rows: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT DISTINCT project_id, dataset_epoch FROM payments ORDER BY 1, 2")
            .fetch_all(pool)
            .await
            .unwrap();
    rows.into_iter()
        .map(|(project_id, epoch)| ProjectScope { project_id, epoch })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_releases_data_restores_and_migrates_to_this_release() {
    for version in RELEASES {
        let fixture: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture_path(version)).unwrap()).unwrap();
        let migrations = fixture["migrations"].as_i64().unwrap();
        let db = TestDb::unmigrated().await;
        sandbox_db::MIGRATOR
            .run_to(migrations, &db.pool)
            .await
            .unwrap();

        // The restore: every row, in one transaction, as the owner, through the journal's
        // own triggers (no trigger is switched off).
        let mut tx = db.pool.begin().await.unwrap();
        for table in TABLES {
            let rows = fixture["tables"][table].clone();
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {table} OVERRIDING SYSTEM VALUE
                 SELECT * FROM jsonb_populate_recordset(NULL::{table}, $1)"
            )))
            .bind(rows)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("{version}: restoring {table}: {e}"));
        }
        tx.commit().await.unwrap();

        // This release's remaining migrations, on that release's data.
        sandbox_db::MIGRATOR
            .run(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("{version}: migrating its data to this release: {e}"));
        let app_pool = sqlx::PgPool::connect(&db.url()).await.unwrap();
        sandbox_db::roles::check_runtime(&app_pool)
            .await
            .unwrap_or_else(|e| panic!("{version}: the server would refuse the result: {e}"));
        app_pool.close().await;

        // The same ledger: balances per project and epoch, every epoch clean, every balance
        // explained to its last line.
        assert_eq!(
            balances(&db.pool).await,
            fixture["balances"],
            "{version}: balances"
        );
        for scope in scopes(&db.pool).await {
            let report = sandbox_db::reconcile::run(&db.pool, scope).await.unwrap();
            assert!(report.is_clean(), "{version}: {scope:?}: {report:?}");
        }

        // Old idempotency keys still replay; new money still moves.
        let sim = TestSim::start().await;
        let app = TestApp::on(
            db,
            &common::sim_vars(&sim, &[("PS_SECRET_KEY", SECRET_KEY)]),
        );
        let key = common::insert_test_key(&app, &fixture["probe"]["project_id"]).await;
        let probe = &fixture["probe"];
        let (status, replay) = app
            .authorize(
                &key,
                probe["idempotency_key"].as_str().unwrap(),
                probe["amount"].as_i64().unwrap(),
                probe["currency"].as_str().unwrap(),
                "tok_sim_approve",
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{version}: {replay}");
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["payment"]["id"], probe["payment_id"]);
        let (status, fresh) = app
            .authorize(&key, "after-upgrade-1", 2_500, "USD", "tok_sim_approve")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{version}: {fresh}");
        app.assert_reconciled(&key).await;
        app.cleanup().await;
        sim.remove().await;
    }
}

/// Writes this release's fixture. Run once per release, then commit the file:
/// `PS_WRITE_RELEASE_FIXTURE=1.0.0 cargo test -p sandbox-server --test releases -- --ignored`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "writes fixtures/releases/<version>.json when PS_WRITE_RELEASE_FIXTURE names it"]
#[expect(
    clippy::too_many_lines,
    reason = "one history, written in the order it happens, then dumped"
)]
async fn write_this_releases_fixture() {
    let Ok(version) = std::env::var("PS_WRITE_RELEASE_FIXTURE") else {
        return;
    };
    assert_eq!(
        version,
        env!("CARGO_PKG_VERSION"),
        "a fixture is written by its own release"
    );
    let sim = TestSim::start_at(
        std::env::temp_dir().join(format!("ps-release-{}.redb", Uuid::now_v7().simple())),
        std::time::Duration::from_secs(2),
    )
    .await;
    let app = TestApp::with_sim(
        &sim,
        &[
            ("PS_SECRET_KEY", SECRET_KEY),
            ("PS_SIM_TIMEOUT_MS", "300"),
            ("PS_RECONCILE_GRACE_MS", "1"),
        ],
    )
    .await;
    let (project_a, a) = app.project("release-a").await;
    let (_, b) = app.project("release-b").await;
    // Webhook events with deliveries recorded (the endpoint never needs to answer).
    let endpoint = app
        .post(
            "/v1/webhook_endpoints",
            &a,
            None,
            json!({"url": "http://127.0.0.1:56199/hooks"}),
        )
        .await;
    assert_eq!(endpoint.0, StatusCode::CREATED, "{}", endpoint.1);
    for (key, n) in [(&a, 1), (&b, 2)] {
        let (_, auth) = app
            .authorize(key, &format!("order-{n}"), 4_200, "USD", "tok_sim_approve")
            .await;
        let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
        app.post(
            &format!("/v1/payments/{pid}/captures"),
            key,
            Some(&format!("capture-{n}")),
            json!({"amount": 4_200}),
        )
        .await;
        app.post(
            &format!("/v1/payments/{pid}/refunds"),
            key,
            Some(&format!("refund-{n}")),
            json!({"amount": 1_200}),
        )
        .await;
    }
    app.authorize(&a, "declined-1", 999, "EUR", "tok_sim_decline_do_not_honor")
        .await;
    let (_, held) = app
        .authorize(&a, "held-1", 10_000, "JPY", "tok_sim_approve")
        .await;
    let held = held["payment"]["id"].as_str().unwrap().to_owned();
    app.post(
        &format!("/v1/payments/{held}/captures"),
        &a,
        Some("held-capture"),
        json!({"amount": 6_000}),
    )
    .await;
    app.post(
        &format!("/v1/payments/{held}/void"),
        &a,
        Some("held-void"),
        json!({}),
    )
    .await;
    let (status, _) = app
        .authorize(&a, "lost-1", 1_250, "KWD", "tok_sim_lost_reply")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    sandbox_server::reconciler::sweep(&app.state).await.unwrap();
    let (status, session) = app
        .post(
            "/v1/checkout_sessions",
            &a,
            None,
            json!({"amount": 1_500, "currency": "USD"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    // A reset, then the probe: a purchase of the new epoch whose key the test repeats.
    let (status, _) = app
        .post(
            "/v1/project/reset",
            &a,
            None,
            json!({"confirm": "release-a", "epoch": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, probe) = app
        .authorize(&a, "probe-1", 3_300, "EUR", "tok_sim_approve")
        .await;
    app.assert_reconciled(&a).await;
    app.assert_reconciled(&b).await;

    let migrations: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let mut tables = serde_json::Map::new();
    for table in TABLES {
        // Journal entries in posting order: a restore re-numbers them in insertion order.
        let order = if *table == "journal_entries" {
            "t.seq"
        } else {
            "t"
        };
        let rows: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}), '[]'::jsonb) FROM {table} t"
        )))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
        tables.insert((*table).to_owned(), rows);
    }
    let fixture = json!({
        "_note": "Every row a database of this release held after the fixture's operations (SIMULATED MONEY ONLY; synthetic keys, hashed; secrets sealed under a test key). Restored by tests/releases.rs into every later release.",
        "release": version,
        "migrations": migrations,
        "balances": balances(&app.db.pool).await,
        "probe": {
            "project_id": project_a,
            "idempotency_key": "probe-1",
            "amount": 3_300,
            "currency": "EUR",
            "payment_id": probe["payment"]["id"],
        },
        "tables": tables,
    });
    std::fs::create_dir_all(format!(
        "{}/../../fixtures/releases",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    std::fs::write(
        fixture_path(&version),
        serde_json::to_string_pretty(&fixture).unwrap() + "\n",
    )
    .unwrap();
    app.cleanup().await;
    sim.remove().await;
}
