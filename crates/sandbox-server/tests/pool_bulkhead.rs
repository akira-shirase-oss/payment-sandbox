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
        started.elapsed() < Duration::from_secs(2),
        "the read waited {:?} on a write-held connection — the bound is the readiness \
         deadline, not a perf measure: a read queued behind the write pool would fail \
         its status first",
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

/// D-101: the read pool's transactions are read-only — a read path that tries to write
/// is refused by PostgreSQL itself (SQLSTATE 25006), so the defect fails loudly in the
/// test that introduces it instead of silently borrowing write authority.
#[tokio::test]
async fn a_write_on_the_read_pool_is_refused_loudly() {
    // The real write path: insert a project.
    async fn write(
        pool: &sqlx::PgPool,
    ) -> Result<sandbox_db::projects::Project, sandbox_db::DbError> {
        let mut tx = pool.begin().await.unwrap();
        let outcome = sandbox_db::projects::create_in(
            &mut tx,
            uuid::Uuid::now_v7(),
            "read-only-probe",
            "probe",
        )
        .await;
        tx.rollback().await.unwrap();
        outcome
    }

    let db = TestDb::new().await;
    let cfg = config(
        &db.url(),
        &[
            ("PS_DB_MAX_CONNECTIONS", "2"),
            ("PS_DB_READ_CONNECTIONS", "1"),
        ],
    );
    let pool = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.pool).unwrap();
    let read = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.read_pool()).unwrap();
    let state = AppState::new(cfg, pool, read);

    let error = write(&state.read_db)
        .await
        .expect_err("a write on the read pool must fail");
    assert!(
        error.sqlstate().as_deref() == Some("25006"),
        "expected SQLSTATE 25006 (read-only transaction), got {error:?}"
    );

    // The write pool still writes: the identical call answers there.
    write(&state.write_db).await.unwrap();
}

/// D-101: classification happens once in the middleware, not per handler — a `POST`
/// draws the write pool and only a route registered through `post_read` reaches the
/// read pool (the `ReadOnly` marker is the compile-checked opt-in).
#[tokio::test]
async fn a_post_reaches_the_read_pool_only_through_post_read() {
    use axum::{Extension, Router, routing::post};
    use sandbox_server::middleware::{self, DbPool};

    // A handler that reports which pool it was handed.
    async fn which_pool(Extension(db): Extension<DbPool>) -> String {
        db.0.options().get_max_connections().to_string()
    }

    let db = TestDb::new().await;
    let cfg = config(
        &db.url(),
        &[
            ("PS_DB_MAX_CONNECTIONS", "4"),
            ("PS_DB_READ_CONNECTIONS", "1"),
        ],
    );
    let pool = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.pool).unwrap();
    let read = sandbox_db::connect_lazy(cfg.database_url.expose(), &cfg.read_pool()).unwrap();
    let state = AppState::new(cfg, pool, read);
    let (read_size, write_size) = (
        state.read_db.options().get_max_connections(),
        state.write_db.options().get_max_connections(),
    );
    assert_ne!(read_size, write_size, "the pools must be distinguishable");

    let router = Router::new()
        .route("/plain", post(which_pool))
        .route("/read", middleware::post_read(&state, which_pool))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::classify_pool,
        ))
        .with_state(state.clone());

    for (path, expected) in [("/plain", write_size), ("/read", read_size)] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            expected.to_string(),
            "{path} drew the wrong pool"
        );
    }
}
