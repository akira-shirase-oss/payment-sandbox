//! D-100: the database layer is bulkheaded like the HTTP layer — reads and writes never
//! share a connection pool. A write flood that holds every write-side connection cannot
//! starve a read: `/readyz` answers from the read pool inside the read deadline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sandbox_db::testing::TestDb;
use sandbox_server::{AppState, Config, app};
use tower::ServiceExt;

mod common;
use common::{BOOTSTRAP, SIM_TOKEN};

fn config(url: &str, extra: &[(&str, &str)]) -> Config {
    let mut vars = HashMap::from([
        ("PS_DATABASE_URL".to_owned(), url.to_owned()),
        ("PS_BOOTSTRAP_TOKEN".to_owned(), BOOTSTRAP.to_owned()),
        ("PS_SIM_TOKEN".to_owned(), SIM_TOKEN.to_owned()),
        ("PS_PROFILE".to_owned(), "dev".to_owned()),
    ]);
    for (k, v) in extra {
        vars.insert((*k).to_owned(), (*v).to_owned());
    }
    Config::from_lookup(|name| vars.get(name).cloned()).unwrap()
}

/// Holds every connection `write` allows, so the next acquire waits out its deadline.
async fn hold_all(
    pool: &sqlx::PgPool,
    connections: u32,
) -> Vec<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let mut held = Vec::new();
    for _ in 0..connections {
        held.push(pool.acquire().await.unwrap());
    }
    held
}

/// The write pool is exhaustible without starving reads: with every write connection
/// held, `/readyz` still answers `200` from the read pool inside the read deadline —
/// the read never waits on a write's connection (D-100).
#[tokio::test]
async fn reads_answer_while_the_write_pool_is_held() {
    let db = TestDb::new().await;
    let cfg = config(
        &db.url(),
        &[
            ("PS_DB_MAX_CONNECTIONS", "2"),
            ("PS_DB_READ_CONNECTIONS", "1"),
            ("PS_DB_ACQUIRE_TIMEOUT_MS", "150"),
            ("PS_DB_READ_ACQUIRE_TIMEOUT_MS", "150"),
            ("PS_READINESS_TIMEOUT_MS", "2000"),
        ],
    );
    let pool = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.pool).unwrap();
    let read = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.read_pool()).unwrap();
    let connections = cfg.pool.max_connections;
    assert_eq!(
        connections, 1,
        "a budget of 2 with 1 read leaves exactly 1 write connection"
    );
    let state = AppState::new(cfg, pool, read);
    let router = app(&state);

    let _held = hold_all(&state.write_db, connections).await;

    let started = Instant::now();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a held write pool must not starve the readiness read"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the read waited {:?} on a write-held connection",
        started.elapsed()
    );
}

/// The pre-bulkhead behaviour, kept as the red half of the proof: give reads the same
/// pool writes exhaust (as before D-100) and the identical request cannot answer — it
/// is refused only after the whole acquire deadline passes.
#[tokio::test]
async fn a_shared_pool_starves_the_same_read() {
    let db = TestDb::new().await;
    let cfg = config(
        &db.url(),
        &[
            ("PS_DB_MAX_CONNECTIONS", "2"),
            ("PS_DB_READ_CONNECTIONS", "1"),
            ("PS_DB_ACQUIRE_TIMEOUT_MS", "150"),
            ("PS_READINESS_TIMEOUT_MS", "2000"),
        ],
    );
    let pool = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.pool).unwrap();
    // One pool serving both duties: the D-099 world, where a held write connection is
    // also a held read connection.
    let state = AppState::new(cfg, pool.clone(), pool);
    let router = app(&state);

    let _held = hold_all(&state.write_db, 1).await;

    let started = Instant::now();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "one shared pool cannot answer a read while its only connection is held"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the starved read should pay the whole acquire deadline, waited {:?}",
        started.elapsed()
    );
}
