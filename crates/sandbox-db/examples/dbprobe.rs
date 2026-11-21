//! Load-run anatomy probe (SIMULATED MONEY ONLY): what one authorize/capture/refund
//! costs on this cluster — round-trip floor, per-phase time inside each lifecycle
//! transaction, and the server settings that decide them (`synchronous_commit`, `fsync`).
//! Reads `PS_DATABASE_URL`; writes one uniquely-slugged probe project.
//!
//!   `PS_DATABASE_URL=... cargo run -p sandbox-db --example dbprobe`

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::items_after_statements,
    reason = "a measurement probe: a failed call is a failed measurement — panic \
              rather than recover like a library; and the run is one linear \
              script, not an API to decompose"
)]

use std::time::Instant;

use sandbox_db::{ProjectScope, payments, projects};
use sandbox_domain::{Amount, Currency, NetworkOutcome, OperationKind};
use sqlx::PgPool;
use uuid::Uuid;

/// Wall time of `f`, in microseconds.
async fn timed<Fut, T>(f: Fut) -> (u128, T)
where
    Fut: std::future::Future<Output = T>,
{
    let at = Instant::now();
    let out = f.await;
    (at.elapsed().as_micros(), out)
}

/// p50/p95/max of a sample, microseconds.
fn stats(samples: &[u128]) -> String {
    let mut v = samples.to_vec();
    v.sort_unstable();
    let at = |pct: usize| v[(v.len() * pct / 100).min(v.len() - 1)];
    format!(
        "n={} p50={}us p95={}us max={}us",
        v.len(),
        at(50),
        at(95),
        v[v.len() - 1]
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("PS_DATABASE_URL").expect("PS_DATABASE_URL must be set");
    let pool = PgPool::connect(&url).await.expect("connect");

    println!("== server settings");
    for setting in [
        "server_version",
        "synchronous_commit",
        "fsync",
        "max_connections",
        "shared_buffers",
    ] {
        let value: String = sqlx::query_scalar("SELECT current_setting($1)")
            .bind(setting)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| "<unavailable>".to_owned());
        println!("{setting}: {value}");
    }
    let has_pss: bool = sqlx::query_scalar(
        "SELECT count(*) > 0 FROM pg_extension WHERE extname = 'pg_stat_statements'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(false);
    println!("pg_stat_statements installed: {has_pss}");

    println!("\n== round-trip floor (SELECT 1, warm pool)");
    let mut rtt = Vec::new();
    for _ in 0..200 {
        let (us, row) = timed(sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool)).await;
        row.expect("SELECT 1");
        rtt.push(us);
    }
    println!("{}", stats(&rtt));

    println!("\n== pool acquire");
    let mut acquire = Vec::new();
    for _ in 0..200 {
        let (us, conn) = timed(pool.acquire()).await;
        drop(conn);
        acquire.push(us);
    }
    println!("{}", stats(&acquire));

    println!("\n== begin/commit overhead (empty tx)");
    let mut begin_us = Vec::new();
    let mut commit_us = Vec::new();
    for _ in 0..200 {
        let (b, tx) = timed(pool.begin()).await;
        let (c, _) = timed(tx.expect("begin").commit()).await;
        begin_us.push(b);
        commit_us.push(c);
    }
    println!("begin: {}", stats(&begin_us));
    println!("commit (fsync): {}", stats(&commit_us));

    let project = projects::create(
        &pool,
        Uuid::now_v7(),
        &format!("dbprobe-{}", Uuid::now_v7().simple()),
        "dbprobe",
    )
    .await
    .expect("create probe project");
    let scope = ProjectScope {
        project_id: project.id,
        epoch: project.dataset_epoch,
    };

    const N: usize = 300;
    println!("\n== lifecycle phases (n={N} authorizes)");
    let mut begin_auth = Vec::new();
    let mut apply_auth = Vec::new();
    let mut begin_cap = Vec::new();
    let mut apply_cap = Vec::new();
    let mut begin_ref = Vec::new();
    let mut apply_ref = Vec::new();
    for i in 0..N {
        let auth = payments::NewAuthorize {
            operation_id: Uuid::now_v7(),
            payment_id: Uuid::now_v7(),
            idempotency_key: &format!("dbprobe-a-{i}"),
            fingerprint: [7_u8; 32],
            amount: Amount::new(1_000).unwrap(),
            currency: Currency::Usd,
            token: "tok_sim_approve",
            description: None,
        };
        let (us, begun) = timed(payments::begin_authorize(&pool, scope, &auth)).await;
        begin_auth.push(us);
        let payments::Begin::Started(operation, payment) = begun.expect("begin_authorize") else {
            panic!("unexpected replay");
        };
        let approved = payments::NetworkResult {
            outcome: NetworkOutcome::Approved,
            decline_code: None,
            network_ref: Some(format!("ref-{i}")),
        };
        let (us, applied) = timed(payments::apply(
            &pool,
            scope,
            payment.id,
            operation.id,
            &approved,
        ))
        .await;
        apply_auth.push(us);
        let payments::Applied::Applied(_, payment) = applied.expect("apply authorize") else {
            panic!("unexpected already-resolved");
        };

        // Capture 600 of 1,000 — the guide's shape.
        let cap = payments::NewChild {
            operation_id: Uuid::now_v7(),
            payment_id: payment.id,
            kind: OperationKind::Capture,
            idempotency_key: &format!("dbprobe-c-{i}"),
            fingerprint: [7_u8; 32],
            amount: Some(Amount::new(600).unwrap()),
        };
        let (us, begun) = timed(payments::begin_child(&pool, scope, &cap)).await;
        begin_cap.push(us);
        let payments::Begin::Started(operation, _) = begun.expect("begin capture") else {
            panic!("unexpected replay");
        };
        let (us, applied) = timed(payments::apply(
            &pool,
            scope,
            payment.id,
            operation.id,
            &approved,
        ))
        .await;
        apply_cap.push(us);
        let payments::Applied::Applied(_, payment) = applied.expect("apply capture") else {
            panic!("unexpected already-resolved");
        };

        // Partial refund of 200.
        let refund = payments::NewChild {
            operation_id: Uuid::now_v7(),
            payment_id: payment.id,
            kind: OperationKind::Refund,
            idempotency_key: &format!("dbprobe-r-{i}"),
            fingerprint: [7_u8; 32],
            amount: Some(Amount::new(200).unwrap()),
        };
        let (us, begun) = timed(payments::begin_child(&pool, scope, &refund)).await;
        begin_ref.push(us);
        let payments::Begin::Started(operation, _) = begun.expect("begin refund") else {
            panic!("unexpected replay");
        };
        let (us, applied) = timed(payments::apply(
            &pool,
            scope,
            payment.id,
            operation.id,
            &approved,
        ))
        .await;
        apply_ref.push(us);
        applied.expect("apply refund");
    }
    println!("begin_authorize: {}", stats(&begin_auth));
    println!("apply authorize: {}", stats(&apply_auth));
    println!("begin_capture:   {}", stats(&begin_cap));
    println!("apply capture:   {}", stats(&apply_cap));
    println!("begin_refund:    {}", stats(&begin_ref));
    println!("apply refund:    {}", stats(&apply_ref));

    // Statement counts per phase, from pg_stat_statements when present.
    if has_pss {
        println!("\n== hottest statements by total time (this database)");
        let rows: Vec<(String, i64, f64)> = sqlx::query_as(
            "SELECT left(regexp_replace(query, '\\s+', ' ', 'g'), 90), calls, mean_exec_time
               FROM pg_stat_statements
              WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
              ORDER BY total_exec_time DESC LIMIT 12",
        )
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        for (query, calls, mean) in rows {
            println!("{calls:>8} calls  {mean:>8.3} ms mean  {query}");
        }
    }
}
