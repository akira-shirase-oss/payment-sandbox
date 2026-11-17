//! A pooled connection never carries a transaction to its next user. sqlx's `begin` leaves
//! one open when its future is cancelled after `BEGIN` reached the server but before sqlx
//! counted it (its rollback guard sees depth 0 and does nothing); a client that disconnects
//! mid-request does exactly that. In the v1 round a cancelled `BEGIN ... READ ONLY` snapshot
//! read (D-076) handed its connection on, and the next request, creating a key, failed with
//! "cannot execute INSERT in a read-only transaction" (D-091). SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::PoolSettings;
use sandbox_db::testing::TestDb;
use sqlx::Executor;

#[tokio::test]
async fn a_transaction_left_open_on_a_pooled_connection_never_reaches_its_next_user() {
    let db = TestDb::new().await;
    let settings = PoolSettings {
        max_connections: 1,
        ..PoolSettings::default()
    };
    let pool = sandbox_db::connect_lazy(&db.url(), &settings).unwrap();
    {
        let mut conn = pool.acquire().await.unwrap();
        // What a cancelled `begin_with` leaves behind: the server is in a read-only
        // transaction, and sqlx does not know it.
        conn.execute("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .await
            .unwrap();
    }
    // The only connection, back from the pool: autocommit, writable.
    let mut conn = pool.acquire().await.unwrap();
    let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        read_only, "off",
        "the next user inherited a read-only transaction"
    );
    drop(conn);
    // The same for a plain `BEGIN`: what it set locally must be gone.
    {
        let mut conn = pool.acquire().await.unwrap();
        conn.execute("BEGIN").await.unwrap();
        conn.execute("SET LOCAL application_name = 'left-over'")
            .await
            .unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    let name: String = sqlx::query_scalar("SHOW application_name")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        name, "payment-sandbox",
        "the next user is inside someone else's transaction"
    );
    drop(conn);
    pool.close().await;
    db.cleanup().await;
}
