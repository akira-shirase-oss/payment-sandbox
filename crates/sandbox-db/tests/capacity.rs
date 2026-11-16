//! Capacity and finality in the schema itself: raw SQL cannot overrun a payment or change a
//! resolved operation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::projects;
use sandbox_db::testing::TestDb;
use sqlx::AssertSqlSafe;
use uuid::Uuid;

async fn sqlstate(db: &TestDb, sql: String) -> String {
    let error = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&db.pool)
        .await
        .expect_err("must be refused");
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
        .unwrap()
}

#[tokio::test]
async fn raw_sql_cannot_overrun_capacity_or_reopen_a_resolved_operation() {
    let db = TestDb::new().await;
    let p = projects::create(&db.pool, Uuid::now_v7(), "store", "Store")
        .await
        .unwrap()
        .id;
    let (pay, op) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::raw_sql(AssertSqlSafe(format!(
        "INSERT INTO payments (project_id, id, dataset_epoch, authorize_operation_id, currency, amount_minor, token, status, authorized_minor, captured_minor)
         VALUES ('{p}', '{pay}', 1, '{op}', 'USD', 100, 'tok_sim_approve', 'captured', 100, 100);
         INSERT INTO operations (project_id, id, dataset_epoch, payment_id, kind, idempotency_key, fingerprint, amount_minor, state, resolved_at)
         VALUES ('{p}', '{op}', 1, '{pay}', 'authorize', 'k', decode(repeat('00', 32), 'hex'), 100, 'succeeded', now());"
    )))
    .execute(&db.pool)
    .await
    .unwrap();
    for sql in [
        format!("UPDATE payments SET captured_minor = 101 WHERE id = '{pay}'"),
        format!("UPDATE payments SET capture_reserved_minor = 1 WHERE id = '{pay}'"),
        format!(
            "UPDATE payments SET refunded_minor = 60, refund_reserved_minor = 41 WHERE id = '{pay}'"
        ),
        format!("UPDATE payments SET authorized_minor = 50 WHERE id = '{pay}'"),
    ] {
        assert_eq!(sqlstate(&db, sql.clone()).await, "23514", "{sql}");
    }
    assert_eq!(
        sqlstate(
            &db,
            format!("UPDATE operations SET state = 'declined' WHERE id = '{op}'")
        )
        .await,
        "PSO01"
    );
    assert_eq!(
        sqlstate(&db, format!("DELETE FROM operations WHERE id = '{op}'")).await,
        "PSJ01"
    );
    assert_eq!(
        sqlstate(&db, format!("DELETE FROM payments WHERE id = '{pay}'")).await,
        "PSJ01"
    );
    db.cleanup().await;
}
