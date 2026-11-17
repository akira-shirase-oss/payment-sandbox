//! Review N12: a failing test leaked its `ps_test_*` database (and, for the D-039 test, a
//! `ps_shadow_*` role), because cleanup was a call at the end of the test that a panic
//! skipped. A test database is now dropped when its handle is, cleaned up or not.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::testing::TestDb;
use sqlx::{Connection, PgConnection};

async fn exists(name: &str) -> bool {
    let mut admin = PgConnection::connect(&std::env::var(sandbox_db::testing::ENV).unwrap())
        .await
        .unwrap();
    let found = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(&mut admin)
        .await
        .unwrap();
    admin.close().await.ok();
    found
}

#[tokio::test]
async fn a_test_database_is_dropped_even_when_its_test_never_cleans_up() {
    let name = {
        let db = TestDb::new().await;
        db.name().to_owned()
        // Dropped here, as a panicking test would drop it.
    };
    assert!(!exists(&name).await, "{name} outlived its handle");

    let panicked = tokio::spawn(async {
        let db = TestDb::new().await;
        let name = db.name().to_owned();
        let _keep = db;
        panic!("a failing test: {name}");
    })
    .await
    .unwrap_err();
    let message = panicked.into_panic();
    let message = message
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    let name = message.trim_start_matches("a failing test: ");
    assert!(name.starts_with("ps_test_"), "{message}");
    assert!(!exists(name).await, "{name} outlived a panicking test");
}
