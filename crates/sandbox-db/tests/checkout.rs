//! Checkout sessions against a real PostgreSQL.
//!
//! Needs `PS_TEST_DATABASE_URL` (`scripts/stack.sh up`, then `eval "$(scripts/stack.sh env)"`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::testing::TestDb;
use sandbox_db::{DbError, ProjectScope, checkout, projects};
use sandbox_domain::Amount;
use sandbox_domain::checkout::CaptureMethod;
use sqlx::PgPool;
use uuid::Uuid;

async fn project(pool: &PgPool, slug: &str) -> ProjectScope {
    projects::create(pool, Uuid::now_v7(), slug, "Test store")
        .await
        .unwrap()
        .scope()
}

/// N7 + round 3: `checkout::create` binds `scope.epoch` into the session — a scope a
/// lagging cache still hands out after a cross-process reset must be refused by
/// `ensure_epoch`, never written into the archived epoch. Before the guard the INSERT
/// landed there, `expired` on arrival and invisible to the new epoch's listings.
#[tokio::test]
async fn a_session_scoped_to_an_archived_epoch_is_refused() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "checkout-epoch").await;

    let new = |id: Uuid| checkout::NewSession {
        id,
        secret_hash: [7; 32],
        amount: Amount::new(4_200).unwrap(),
        currency: sandbox_domain::Currency::Usd,
        description: None,
        capture_method: CaptureMethod::Automatic,
        embed_origins: &[],
        return_url: None,
        lifetime_seconds: 3_600,
    };
    let first = Uuid::now_v7();
    checkout::create(&db.pool, scope, &new(first))
        .await
        .expect("a session in the current epoch must be created");

    // A reset opens the next epoch; `scope` is what a stale cache still hands out.
    let reset = projects::reset(
        &db.pool,
        scope.project_id,
        scope.epoch,
        "checkout-epoch",
        "test",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(
        matches!(reset, projects::ResetOutcome::Reset { .. }),
        "{reset:?}"
    );

    let err = checkout::create(&db.pool, scope, &new(Uuid::now_v7()))
        .await
        .expect_err("a stale epoch must never insert");
    assert!(matches!(err, DbError::EpochMoved), "{err:?}");
    // Nothing of it persisted: the archived epoch holds only the first session.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM checkout_sessions WHERE project_id = $1 AND dataset_epoch = $2",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        count, 1,
        "a refused create left a row in the archived epoch"
    );

    db.cleanup().await;
}
