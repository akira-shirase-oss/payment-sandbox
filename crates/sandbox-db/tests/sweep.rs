//! Review N12: databases and roles that earlier runs leaked (a killed test process skips
//! every destructor) are swept when a test process first creates a test database: test
//! databases older than two hours and shadow roles older than one, judged by the UUIDv7
//! timestamp in their names. Its own test binary, so that first creation is this test's.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::testing::TestDb;
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

#[tokio::test]
async fn stale_test_databases_and_roles_are_swept() {
    let mut admin = PgConnection::connect(&std::env::var(sandbox_db::testing::ENV).unwrap())
        .await
        .unwrap();
    let hours_ago = |h: u64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        format!("{:012x}{}", now - u128::from(h) * 3_600_000, "0".repeat(20))
    };
    let stale_db = format!("ps_test_{}", hours_ago(3));
    let fresh_db = format!("ps_test_{}", hours_ago(0));
    let stale_role = format!("ps_shadow_{}", hours_ago(2));
    for statement in [
        format!("CREATE DATABASE {stale_db}"),
        format!("CREATE DATABASE {fresh_db}"),
        format!("CREATE ROLE {stale_role}"),
    ] {
        admin.execute(AssertSqlSafe(statement)).await.unwrap();
    }
    let db = TestDb::new().await;
    let count = |sql: &'static str, name: String| {
        let url = std::env::var(sandbox_db::testing::ENV).unwrap();
        async move {
            let mut c = PgConnection::connect(&url).await.unwrap();
            let n: i64 = sqlx::query_scalar(sql)
                .bind(name)
                .fetch_one(&mut c)
                .await
                .unwrap();
            c.close().await.ok();
            n
        }
    };
    let db_sql = "SELECT count(*) FROM pg_database WHERE datname = $1";
    let role_sql = "SELECT count(*) FROM pg_roles WHERE rolname = $1";
    let (stale, fresh, role) = (
        count(db_sql, stale_db.clone()).await,
        count(db_sql, fresh_db.clone()).await,
        count(role_sql, stale_role.clone()).await,
    );
    admin
        .execute(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {fresh_db}; DROP DATABASE IF EXISTS {stale_db}; DROP ROLE IF EXISTS {stale_role}"
        )))
        .await
        .ok();
    db.cleanup().await;
    assert_eq!(
        (stale, fresh, role),
        (0, 1, 0),
        "stale db, fresh db, stale role"
    );
}
