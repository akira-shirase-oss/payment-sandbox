//! EXPLAIN (ANALYZE, BUFFERS) evidence for the m5 read path (D-098): a project with a
//! realistically sized journal (~300k lines on one account across 16 balance stripes),
//! then the actual plans for `/v1/balances`, the explain horizon and the explain page.
//! Run with -- --nocapture; the printed plans are the committed evidence.
//!
//! Needs `PS_TEST_DATABASE_URL` (`scripts/stack.sh up`, then `eval "$(scripts/stack.sh env)"`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use sandbox_db::testing::TestDb;
use sandbox_db::{ProjectScope, projects};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

#[tokio::test]
async fn explain_plans_at_300k_lines() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    let scope = projects::create(pool, Uuid::now_v7(), "plans", "Plans")
        .await
        .unwrap()
        .scope();
    seed(pool, scope).await;

    let pid = scope.project_id.to_string();
    let epoch = scope.epoch.to_string();
    // /v1/balances: the stripe sum.
    run_plan(
        pool,
        "balances",
        &format!(
            "SELECT currency, account_kind, sum(balance_minor)::bigint \
               FROM account_balances \
              WHERE project_id = '{pid}' AND dataset_epoch = {epoch} \
              GROUP BY currency, account_kind ORDER BY 1,2"
        ),
    )
    .await;
    // The explain horizon: derived balance minus the snapshot-invisible tail.
    run_plan(
        pool,
        "horizon",
        &format!(
            "SELECT coalesce(b.s,0) - coalesce(inv.a,0) FROM (SELECT 1) one \
               LEFT JOIN LATERAL (SELECT sum(balance_minor) s FROM account_balances \
                 WHERE project_id = '{pid}' AND dataset_epoch = {epoch} \
                   AND currency='EUR' AND account_kind='payer_funding') b ON true \
               LEFT JOIN LATERAL (SELECT sum(l.amount_minor) a FROM journal_lines l \
                 JOIN journal_entries e ON e.project_id=l.project_id AND e.id=l.entry_id \
                 WHERE l.project_id='{pid}' AND l.dataset_epoch={epoch} \
                   AND l.currency='EUR' AND l.account_kind='payer_funding' \
                   AND e.created_xact >= pg_snapshot_xmin(pg_current_snapshot()) \
                   AND NOT (e.xmin <> e.created_xact::xid \
                            OR pg_visible_in_snapshot(e.created_xact, \
                                pg_current_snapshot()))) inv ON true"
        ),
    )
    .await;
    // The explain page: 100 lines at the cursor.
    run_plan(
        pool,
        "page",
        &format!(
            "SELECT e.seq, l.amount_minor FROM journal_lines l \
               JOIN journal_entries e ON e.project_id=l.project_id AND e.id=l.entry_id \
              WHERE l.project_id='{pid}' AND l.dataset_epoch={epoch} \
                AND l.currency='EUR' AND l.account_kind='payer_funding' \
                AND e.seq > 0 ORDER BY e.seq, l.line_no LIMIT 100"
        ),
    )
    .await;
}

async fn run_plan(pool: &PgPool, label: &str, sql: &str) {
    // Audited: the SQL is built in this test from a uuid, an integer and literal text —
    // no user input reaches it.
    let rows: Vec<(String,)> =
        sqlx::query_as(AssertSqlSafe(format!("EXPLAIN (ANALYZE, BUFFERS) {sql}")))
            .fetch_all(pool)
            .await
            .unwrap();
    println!("=== {label} ===");
    for (line,) in rows {
        println!("{line}");
    }
}

/// 100k entries x 2 lines on `payer_funding` plus the matching 16-stripe derived rows,
/// fabricated directly: this measures the READ plan, not the posting path.
async fn seed(pool: &PgPool, scope: ProjectScope) {
    let pid = scope.project_id;
    let epoch = scope.epoch;
    // Entries and their balanced lines in one statement — the journal's own check fires
    // per statement, so a two-step seed would commit unbalanced entries. A raised local
    // timeout: the bulk fabricate is bigger than any request's statement.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL statement_timeout = '120s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "WITH ent AS (
           INSERT INTO journal_entries
               (project_id, id, dataset_epoch, currency, recipe, semantics_version)
             SELECT $1, gen_random_uuid(), $2, 'EUR',
                    CASE g % 3 WHEN 0 THEN 'authorize' WHEN 1 THEN 'capture' ELSE 'refund' END, 1
               FROM generate_series(1, 100000) g
             RETURNING project_id, id, seq, currency, dataset_epoch
         )
         INSERT INTO journal_lines
             (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
           SELECT e.project_id, e.id, leg.line_no, e.currency, e.dataset_epoch,
                  leg.account_kind, leg.amount * ((e.seq % 97) + 1)
             FROM ent e
             JOIN (VALUES (1::smallint, 'payer_funding'::text, 1::bigint),
                          (2::smallint, 'merchant_pending'::text, -1::bigint))
                  AS leg(line_no, account_kind, amount) ON true",
    )
    .bind(pid)
    .bind(epoch)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO account_balances
            (project_id, dataset_epoch, currency, account_kind, stripe, balance_minor, line_count)
          SELECT $1, $2, 'EUR', 'payer_funding', s, 1000, 100
            FROM generate_series(0, 15) s",
    )
    .bind(pid)
    .bind(epoch)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE journal_entries")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE journal_lines")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE account_balances")
        .execute(pool)
        .await
        .unwrap();
}
