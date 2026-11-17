//! Review finding 1: the runtime role (what the server connects as) cannot tamper with the
//! ledger. Every attempt below must be refused by PostgreSQL itself, and the journal must be
//! intact afterwards.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::journal::{self, EntryRefs};
use sandbox_db::projects;
use sandbox_db::testing::TestDb;
use sandbox_domain::identity::entry_id;
use sandbox_domain::{Amount, Currency, Recipe};
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use uuid::Uuid;

async fn runtime(db: &TestDb) -> PgConnection {
    PgConnection::connect(&db.url())
        .await
        .expect("connecting as the server's role")
}

#[tokio::test]
async fn the_runtime_role_cannot_bypass_or_rewrite_the_journal() {
    let db = TestDb::new().await;
    let a = projects::create(&db.pool, Uuid::now_v7(), "store-a", "A")
        .await
        .unwrap();
    let b = projects::create(&db.pool, Uuid::now_v7(), "store-b", "B")
        .await
        .unwrap();
    let entry = Recipe::Authorize
        .post(Currency::Usd, Amount::new(500).unwrap())
        .unwrap();
    let id = entry_id(Uuid::now_v7(), Recipe::Authorize);
    let mut tx = db.pool.begin().await.unwrap();
    journal::post(&mut tx, a.scope(), id, &entry, EntryRefs::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut conn = runtime(&db).await;
    let (pa, pb) = (a.id, b.id);
    let e = Uuid::now_v7();
    let attempts = [
        "SET session_replication_role = replica".to_owned(),
        "ALTER TABLE journal_lines DISABLE TRIGGER USER".to_owned(),
        "TRUNCATE journal_lines".to_owned(),
        "TRUNCATE journal_entries CASCADE".to_owned(),
        "DELETE FROM journal_lines".to_owned(),
        "UPDATE journal_lines SET amount_minor = 999".to_owned(),
        "DROP TRIGGER journal_lines_balanced ON journal_lines".to_owned(),
        "CREATE TABLE intruder (a int)".to_owned(),
        "CREATE TEMP TABLE journal_lines (a int)".to_owned(),
        // Replica mode is the only way past foreign keys and the stamp trigger.
        format!(
            "BEGIN; SET LOCAL session_replication_role = replica;
             INSERT INTO journal_entries (project_id, id, dataset_epoch, currency, recipe, semantics_version, posted_at)
             VALUES ('{pb}', '{e}', 1, 'USD', 'authorize', 1, '2001-01-01');
             INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             VALUES ('{pa}', '{e}', 1, 'JPY', 1, 'payer_funding', 7), ('{pa}', '{e}', 2, 'JPY', 1, 'payer_held', -7);
             COMMIT;"
        ),
    ];
    for sql in attempts {
        let result = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
            .execute(&mut conn)
            .await;
        assert!(result.is_err(), "the server's role was allowed: {sql}");
        let _ = sqlx::raw_sql("ROLLBACK").execute(&mut conn).await;
    }
    let (lines, total): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(amount_minor), 0)::bigint FROM journal_lines",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((lines, total), (2, 0), "the journal is intact");
    let replica: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal_entries WHERE posted_at < '2020-01-01'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(replica, 0, "nothing was backdated");
    conn.close().await.ok();
    db.cleanup().await;
}

/// The server refuses to start as a superuser or as the owner of the journal.
#[tokio::test]
async fn only_the_runtime_role_passes_the_startup_check() {
    use sandbox_db::roles::{RoleCheckError, RoleProblem, check_runtime};
    let db = TestDb::new().await;
    let pool = |url: String| async move { sqlx::PgPool::connect(&url).await.unwrap() };
    let admin = pool(db.admin_url()).await;
    let owner = pool(db.owner_url()).await;
    let app = pool(db.url()).await;
    assert!(matches!(
        check_runtime(&admin).await,
        Err(RoleCheckError::Refused(RoleProblem::Superuser(_)))
    ));
    assert!(matches!(
        check_runtime(&owner).await,
        Err(RoleCheckError::Refused(RoleProblem::Owner(_)))
    ));
    assert!(check_runtime(&app).await.is_ok());
    for p in [admin, owner, app] {
        p.close().await;
    }
    db.cleanup().await;
}
