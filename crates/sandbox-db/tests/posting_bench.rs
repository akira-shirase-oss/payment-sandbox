//! A development-observation benchmark, not a gate: the cost of maintaining the derived
//! per-account balances inside the posting statement (D-096), measured as `N` concurrent
//! tasks post to ONE project. Every posting touches the same few account rows, so this is
//! the hot-row ceiling the coordinator asked to be measured before merge.
//!
//! Run: `cargo test --release -p sandbox-db --test posting_bench -- --nocapture`
//! with `PS_TEST_DATABASE_URL` set (the stack's `scripts/stack.sh env` prints it). Results
//! print as `BENCH {...}` JSON lines to commit under `perf/results/` with machine + commit.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a benchmark: a panic is the failure report"
)]

use std::time::{Duration, Instant};

use sandbox_db::journal::{self, EntryRefs};
use sandbox_db::testing::TestDb;
use sandbox_db::{ProjectScope, projects};
use sandbox_domain::{Amount, Currency, Recipe};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// One posting: a whole transaction (begin, post, commit) — what a caller waits for.
async fn post_one(pool: &PgPool, scope: ProjectScope) {
    let entry = Recipe::Authorize
        .post(Currency::Usd, Amount::new(100).unwrap())
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    journal::post(&mut tx, scope, Uuid::now_v7(), &entry, EntryRefs::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn level(pool: &PgPool, scope: ProjectScope, n: usize, seconds: u64) -> (u64, Vec<u64>) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            let mut latencies = Vec::new();
            while Instant::now() < deadline {
                let started = Instant::now();
                post_one(&pool, scope).await;
                latencies.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            }
            latencies
        }));
    }
    let mut latencies = Vec::new();
    for handle in handles {
        latencies.extend(handle.await.unwrap());
    }
    latencies.sort_unstable();
    (u64::try_from(latencies.len()).unwrap_or(0), latencies)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn posting_throughput_by_concurrency() {
    let db = TestDb::new().await;
    let scope = projects::create(&db.pool, Uuid::now_v7(), "bench", "Bench store")
        .await
        .unwrap()
        .scope();
    // The project's own runtime role, with enough connections that the pool itself is
    // never the measured limit.
    let pool = PgPoolOptions::new()
        .max_connections(80)
        .connect(&db.url())
        .await
        .unwrap();
    // Warm the connections and the statement plans before the first level.
    for _ in 0..4 {
        post_one(&pool, scope).await;
    }
    for n in [1usize, 8, 32, 64] {
        let (posts, latencies) = level(&pool, scope, n, 5).await;
        let p99 = latencies
            .get(latencies.len().saturating_mul(99) / 100)
            .copied()
            .unwrap_or(0);
        let median = latencies.get(latencies.len() / 2).copied().unwrap_or(0);
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned());
        println!(
            "BENCH {{\"commit\":\"{commit}\",\"concurrency\":{n},\"posts\":{posts},\
             \"seconds\":5,\"posts_per_second\":{:.1},\"p50_us\":{median},\"p99_us\":{p99}}}",
            f64::from(u32::try_from(posts).unwrap_or(u32::MAX)) / 5.0
        );
    }
    db.cleanup().await;
}
