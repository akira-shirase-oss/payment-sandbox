//! M1: projects and sandbox keys. Bootstrap, scopes, revocation, isolation between
//! projects, and secrets kept out of the logs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{BOOTSTRAP, TestApp};
use serde_json::json;

#[tokio::test]
async fn creating_a_project_needs_the_operator_bootstrap_token() {
    let app = TestApp::new(&[]).await;
    let body = json!({"slug": "store", "name": "Store"});
    for bearer in [None, Some("wrong-token-wrong-token-wrong-token-x")] {
        let (status, reply) = app
            .send(
                Method::POST,
                "/v1/projects",
                bearer,
                None,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{reply}");
        // Its own refusal, naming the credential it needs and where it comes from (v1
        // review M5), not "missing or invalid sandbox key".
        assert_eq!(
            reply["error"]["code"], "bootstrap_token_required",
            "{reply}"
        );
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(message.contains("PS_BOOTSTRAP_TOKEN"), "{message}");
        assert!(!message.contains("sandbox key"), "{message}");
    }
    let (status, reply) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let secret = reply["api_key"]["secret"].as_str().unwrap();
    assert!(
        secret.starts_with("sk_sim_") && secret.len() == 71,
        "{secret}"
    );
    assert_eq!(reply["api_key"]["scopes"], json!(["admin"]));
    assert_eq!(reply["simulated"], true);
    // An admin key is not a bootstrap token.
    let (status, _) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(secret),
            None,
            Some(json!({"slug": "other", "name": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn without_a_configured_bootstrap_token_nobody_can_create_projects() {
    let app = TestApp::new(&[]).await;
    let mut state = app.state.clone();
    let mut config = (*state.config).clone();
    config.bootstrap_token = None;
    state.config = Arc::new(config);
    let router = sandbox_server::app(&state);
    let request = axum::http::Request::post("/v1/projects")
        .header("authorization", format!("Bearer {BOOTSTRAP}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"slug":"s","name":"n"}"#))
        .unwrap();
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        reply["error"]["code"], "bootstrap_token_required",
        "{reply}"
    );
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("started without PS_BOOTSTRAP_TOKEN"),
        "{message}"
    );
    app.cleanup().await;
}

#[tokio::test]
async fn slugs_are_unique_and_validated() {
    let app = TestApp::new(&[]).await;
    app.project("store").await;
    let (status, reply) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            Some(json!({"slug": "store", "name": "Again"})),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );
    for slug in ["Store", "-x", "a b", ""] {
        let (status, reply) = app
            .send(
                Method::POST,
                "/v1/projects",
                Some(BOOTSTRAP),
                None,
                Some(json!({"slug": slug, "name": "x"})),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{slug:?}: {reply}"
        );
    }
    app.cleanup().await;
}

#[tokio::test]
async fn scopes_decide_what_a_key_may_do() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("store").await;
    let (_, reader) = app.key(&admin, &["read"]).await;
    let (_, writer) = app.key(&admin, &["payments:write"]).await;

    assert_eq!(app.get("/v1/project", &reader).await.0, StatusCode::OK);
    assert_eq!(
        app.get("/v1/project", &writer).await.0,
        StatusCode::OK,
        "payments:write implies read"
    );
    for key in [&reader, &writer] {
        let (status, reply) = app.get("/v1/keys", key).await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
        let (status, _) = app
            .post(
                "/v1/keys",
                key,
                None,
                json!({"label": "x", "scopes": ["admin"]}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, reply) = app.get("/v1/keys", &admin).await;
    assert_eq!(status, StatusCode::OK);
    let listed = reply["api_keys"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    assert!(
        !reply.to_string().contains(&reader),
        "a listing never carries secrets"
    );

    let (status, _) = app
        .post(
            "/v1/keys",
            &admin,
            None,
            json!({"label": "x", "scopes": ["owner"]}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = app
        .post(
            "/v1/keys",
            &admin,
            None,
            json!({"label": "x", "scopes": []}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    app.cleanup().await;
}

#[tokio::test]
async fn malformed_and_live_looking_keys_are_refused() {
    let app = TestApp::new(&[]).await;
    app.project("store").await;
    let hex = "0".repeat(64);
    for key in [
        format!("sk_live_{hex}"),
        format!("sk_test_{hex}"),
        format!("sk_sim_{hex}"),
        "garbage".to_owned(),
    ] {
        let (status, reply) = app.get("/v1/project", &key).await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized")),
            "{key}"
        );
    }
    let (status, _) = app.send(Method::GET, "/v1/project", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn a_revoked_key_fails_on_its_very_next_request() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("store").await;
    let (reader_id, reader) = app.key(&admin, &["read"]).await;
    assert_eq!(app.get("/v1/project", &reader).await.0, StatusCode::OK);
    let (status, _) = app
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        app.get("/v1/project", &reader).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Revocation is idempotent.
    let (status, _) = app
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    app.cleanup().await;
}

// D-102's addendum: the key cache is exact inside the process that revoked; a SECOND
// instance on the same database is the cross-process case. B caches the key, A revokes
// it through the API, and B's `auth_epoch` listener must drop it — B refuses the key on
// its next request, not after the 30 s TTL. Without the listener this stays `200` for
// the TTL: the red.
/// Test-local: a second "process" on `a`'s database — pools, cache, router and the
/// listener's unique `application_name` — without the listener task, so tests choose
/// `run` (production beat) or `run_with` (short beat).
fn b_state(a: &TestApp) -> (sandbox_server::AppState, axum::Router, sqlx::PgPool) {
    use sandbox_server::{AppState, Config, app};
    use std::collections::HashMap;

    let vars: HashMap<String, String> = HashMap::from([
        ("PS_DATABASE_URL".to_owned(), a.db.url()),
        (
            "PS_BOOTSTRAP_TOKEN".to_owned(),
            common::BOOTSTRAP.to_owned(),
        ),
        ("PS_SIM_TOKEN".to_owned(), common::SIM_TOKEN.to_owned()),
        ("PS_PROFILE".to_owned(), "dev".to_owned()),
        ("PS_DB_ACQUIRE_TIMEOUT_MS".to_owned(), "15000".to_owned()),
        ("PS_DB_STATEMENT_TIMEOUT_MS".to_owned(), "10000".to_owned()),
        ("PS_DB_LOCK_TIMEOUT_MS".to_owned(), "10000".to_owned()),
        ("PS_REQUEST_TIMEOUT_MS".to_owned(), "60000".to_owned()),
    ]);
    let config = Config::from_lookup(|name| vars.get(name).cloned()).unwrap();
    let b_pool = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool).unwrap();
    let b_read =
        sandbox_db::connect_lazy(config.database_url.expose(), &config.read_pool()).unwrap();
    let b_state = AppState::new(config, b_pool, b_read.clone());
    let b_router = app(&b_state);
    (b_state, b_router, b_read)
}

fn second_instance(
    a: &TestApp,
) -> (
    axum::Router,
    sqlx::PgPool,
    String,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::sync::watch;

    let (b_state, b_router, b_read) = b_state(a);
    let listen_name = b_state.listen_name.clone();
    // B's listener, what `serve` spawns for a real process.
    let (stop_tx, stop_rx) = watch::channel(false);
    let listener = tokio::spawn(sandbox_server::authlisten::run(b_state, stop_rx));
    (b_router, b_read, listen_name, stop_tx, listener)
}

async fn get_b(router: &axum::Router, path: &str, key: &str) -> StatusCode {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(path.to_owned())
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

/// The real signal for "the listener is attached": the backend carrying this
/// instance's unique `application_name` in `pg_stat_activity` — never a query-text
/// match, which on a shared cluster could pick this poller, a parallel test's
/// listener or another worktree's server (N2). Waits for a backend OTHER than
/// `other_than` (None the first time, the killed pid after a gap) so a reattach is
/// proven, not merely observed once. `pg_terminate_backend` on the returned pid can
/// only hit THIS test's listener.
/// The attach signal `wait_attached`'s pid poll cannot give on its own: on this
/// shared dev cluster `application_name` in `pg_stat_activity` has surfaced
/// ~15 s behind the backend itself (the stats path, not the view), while the
/// listener's `Tasks` beat fires the moment `LISTEN` lands — in-process, no
/// stats visibility involved. Tests that only need "is it attached" wait on the
/// beat; tests that need the pid then poll the name, which is guaranteed to
/// appear but not on any deadline.
/// `Tasks::register` seeds `last_beat` at registration, so `since_beat_ms` alone
/// cannot prove the loop reached `beat(TASK)` — require a beat newer than
/// `after` (captured after registration, before the spawn).
async fn wait_beating(
    tasks: &sandbox_server::supervisor::Tasks,
    name: &'static str,
    after: std::time::Instant,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let since_ms = u64::try_from(after.elapsed().as_millis()).unwrap_or(u64::MAX);
        let beating = tasks
            .statuses()
            .into_iter()
            .any(|s| s.name == name && s.since_beat_ms < since_ms);
        if beating {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the auth listener task never beat after attach"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn wait_attached(pool: &sqlx::PgPool, name: &str, other_than: Option<i32>) -> i32 {
    // The unique `application_name` is unforgeable, but its visibility lags (see
    // `wait_beating`); this poll only runs where a beat already proved the
    // backend exists, so a generous deadline just waits out the stats path.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut last_error = None;
    loop {
        match sqlx::query_scalar::<_, i32>(
            "SELECT pid FROM pg_stat_activity \
             WHERE datname = current_database() \
               AND backend_type = 'client backend' \
               AND application_name = $1 \
               AND pid <> pg_backend_pid() \
               AND ($2::int IS NULL OR pid <> $2) \
             LIMIT 1",
        )
        .bind(name)
        .bind(other_than)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(pid)) => return pid,
            Ok(None) => (),
            Err(error) => last_error = Some(error.to_string()),
        }
        if std::time::Instant::now() >= deadline {
            type Row = (i32, Option<String>, Option<String>, Option<String>);
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT pid, datname, application_name, backend_type \
                     FROM pg_stat_activity WHERE datname = current_database()",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            panic!(
                "the auth listener {name:?} never attached; \
                 activity={rows:?}; last_error={last_error:?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Bounded settle for an answer in B to reach `want` — notification delivery and
/// re-resolution are async; the assertion stays a signal wait, not a guessed sleep.
async fn until_b(router: &axum::Router, key: &str, want: StatusCode) -> StatusCode {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut last = StatusCode::OK;
    while std::time::Instant::now() < deadline {
        last = get_b(router, "/v1/project", key).await;
        if last == want {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    last
}

#[tokio::test]
async fn a_revocation_in_another_process_reaches_this_cache_on_its_next_request() {
    let a = TestApp::new(&[]).await;
    let (b_router, b_read, listen_name, stop_tx, listener) = second_instance(&a);

    let (_, admin) = a.project("crossa").await;
    let (reader_id, reader) = a.key(&admin, &["read"]).await;
    // B resolves and caches the key before the revocation — the stale-hit the listener
    // must kill.
    assert_eq!(
        get_b(&b_router, "/v1/project", &reader).await,
        StatusCode::OK
    );
    assert_eq!(
        get_b(&b_router, "/v1/project", &reader).await,
        StatusCode::OK
    );
    // The listener must be attached before A's revoke commits, or its notification is
    // legitimately missed (the TTL's own backstop case, not what this test measures).
    wait_attached(&b_read, &listen_name, None).await;

    let (status, _) = a
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let last = until_b(&b_router, &reader, StatusCode::UNAUTHORIZED).await;
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), listener).await;
    assert_eq!(
        last,
        StatusCode::UNAUTHORIZED,
        "a key revoked in process A must fail in process B on its next request"
    );
    a.cleanup().await;
}

/// S2-1: a lost listener connection drops the gap's notifications — sqlx's `recv()`
/// reconnects silently and never replays them. The loop must flush the cache on the
/// gap AND again after the next LISTEN lands, or a revoke committed in the gap keeps
/// working for the cache's TTL. Proved by killing the backend mid-test: B caches, the
/// backend dies, A revokes during the gap, and B's very next request after the
/// reattach still answers 401.
#[tokio::test]
async fn a_revocation_committed_in_a_listener_gap_still_flushes() {
    let a = TestApp::new(&[]).await;
    let (b_router, b_read, listen_name, stop_tx, listener) = second_instance(&a);

    let (_, admin) = a.project("gap").await;
    let (reader_id, reader) = a.key(&admin, &["read"]).await;
    // Cached in B — the entry that must NOT survive the gap.
    assert_eq!(
        get_b(&b_router, "/v1/project", &reader).await,
        StatusCode::OK
    );
    let dead = wait_attached(&b_read, &listen_name, None).await;

    // Kill the listener's backend, then revoke through A while B cannot be listening.
    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(dead)
        .execute(&b_read)
        .await
        .unwrap();
    let (status, _) = a
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // The reattach is the proof's pivot: only after a NEW backend is listening has the
    // loop run its post-listen flush — before the fix the stale entry survived this.
    wait_attached(&b_read, &listen_name, Some(dead)).await;
    let last = until_b(&b_router, &reader, StatusCode::UNAUTHORIZED).await;
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), listener).await;
    assert_eq!(
        last,
        StatusCode::UNAUTHORIZED,
        "a key revoked while the listener was down must not outlive the reconnect"
    );
    a.cleanup().await;
}

/// B: quiet beats keep the same backend — the self-ping arrives on the listener's
/// own second connection, so a live wire is never rebuilt.
#[tokio::test]
async fn quiet_beats_keep_the_same_backend() {
    use std::time::Duration;

    let a = TestApp::new(&[]).await;
    let (b_state, _router, b_read) = b_state(&a);
    let listen_name = b_state.listen_name.clone();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let listener = tokio::spawn(sandbox_server::authlisten::run_with(
        b_state,
        stop_rx,
        Duration::from_millis(150),
    ));
    let pid = wait_attached(&b_read, &listen_name, None).await;
    // Three full beats: pings sent and delivered — the wire must not be rebuilt.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let still = wait_attached(&b_read, &listen_name, None).await;
    assert_eq!(still, pid, "a live listener was rebuilt without any load");
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), listener).await;
    a.cleanup().await;
}

/// B: the liveness ping has its own connection — a write pool saturated past its
/// acquire timeout cannot unsend it and tear down a healthy listener (which would
/// then flush the whole key cache onto that same saturated pool every two beats).
#[tokio::test]
async fn a_saturated_write_pool_never_tears_down_the_listener() {
    use std::time::Duration;

    let a = TestApp::new(&[]).await;
    let (b_state, _router, b_read) = b_state(&a);
    let listen_name = b_state.listen_name.clone();
    let write_db = b_state.write_db.clone();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let listener = tokio::spawn(sandbox_server::authlisten::run_with(
        b_state,
        stop_rx,
        Duration::from_millis(150),
    ));
    let pid = wait_attached(&b_read, &listen_name, None).await;

    // Every pooled connection checked out for longer than several beats: a
    // pool-riding ping could not even be sent. `try_acquire` cannot create
    // connections, so `acquire` runs until it would block — saturation, whatever
    // the pool's configured size.
    let mut held = Vec::new();
    while let Ok(Ok(conn)) =
        tokio::time::timeout(Duration::from_millis(300), write_db.acquire()).await
    {
        held.push(conn);
    }
    assert!(!held.is_empty(), "the pool was never saturated");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let still = wait_attached(&b_read, &listen_name, None).await;
    assert_eq!(
        still, pid,
        "a saturated write pool rebuilt a healthy listener — the ping rode it"
    );

    drop(held);
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), listener).await;
    a.cleanup().await;
}

/// A TCP hop that can go silent without closing either side — the half-open
/// failure only a self-ping detects. With the gate closed the proxy stops READING
/// in both directions: the sockets stay ESTABLISHED, no FIN ever leaves, and
/// buffered bytes simply wait. Returns the proxy's `host:port`, its gate, and a
/// receiver pushed each forwarded connection's upstream `client_port` — the
/// identity `pg_stat_activity` would show, without the stats visibility lag.
async fn silent_proxy(
    upstream: String,
) -> (
    String,
    tokio::sync::watch::Sender<bool>,
    tokio::sync::mpsc::UnboundedReceiver<u16>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (gate_tx, gate_rx) = watch::channel(true);
    let (ports_tx, ports_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let gate = gate_rx.clone();
            let upstream = upstream.clone();
            let ports_tx = ports_tx.clone();
            tokio::spawn(async move {
                let Ok(outbound) = tokio::net::TcpStream::connect(&upstream).await else {
                    return;
                };
                let _ = ports_tx.send(outbound.local_addr().map_or(0, |a| a.port()));
                let (in_read, in_write) = inbound.into_split();
                let (out_read, out_write) = outbound.into_split();
                // Each direction moves bytes only while the gate is open — never
                // reading is what makes the hop silent, not just unresponsive.
                let pipe = |mut read: tokio::net::tcp::OwnedReadHalf,
                            mut write: tokio::net::tcp::OwnedWriteHalf,
                            mut gate: watch::Receiver<bool>| async move {
                    let mut buf = [0_u8; 8_192];
                    loop {
                        tokio::select! {
                            changed = gate.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                            }
                            n = read.read(&mut buf), if *gate.borrow() => {
                                match n {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => {
                                        if write.write_all(&buf[..n]).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                tokio::join!(
                    pipe(in_read, out_write, gate.clone()),
                    pipe(out_read, in_write, gate)
                );
            });
        }
    });
    (addr, gate_tx, ports_rx)
}

/// The next upstream port the proxy accepted, or a labeled panic at 15 s — labels
/// keep the failure message honest about which expected connection never came.
async fn next_port(which: &str, ports: &mut tokio::sync::mpsc::UnboundedReceiver<u16>) -> u16 {
    tokio::time::timeout(std::time::Duration::from_secs(15), ports.recv())
        .await
        .unwrap_or_else(|_| panic!("the proxy saw no new connection for {which}"))
        .expect("the proxy stopped reporting connections")
}

/// B + N3: a half-open LISTEN socket (no error, no FIN, no bytes) is detected —
/// the self-ping's notification stops arriving, a whole silent beat declares the
/// wire dead, and the reconnect attaches a NEW backend.
#[tokio::test]
async fn a_black_holed_listener_socket_is_detected() {
    use std::collections::HashMap;
    use std::time::Duration;

    use sandbox_server::{AppState, Config};

    let a = TestApp::new(&[]).await;
    // The pools reach the database directly; only the listener pair rides the proxy.
    let direct = a.db.url();
    let (creds, rest) = direct
        .rsplit_once('@')
        .expect("the test database url carries credentials");
    let (upstream, path) = rest
        .split_once('/')
        .expect("the test database url carries a database name");
    let (proxy_addr, gate, mut ports) = silent_proxy(upstream.to_owned()).await;
    let proxied = format!("{creds}@{proxy_addr}/{path}");
    // Canary: the proxy must really forward before its silence can prove anything.
    let mut canary = <sqlx::PgConnection as sqlx::Connection>::connect(&proxied)
        .await
        .expect("the proxy does not forward");
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut canary)
        .await
        .expect("the proxy dropped a query");
    assert_eq!(one, 1);
    drop(canary);
    // The LISTEN path too — a plain query proves the pipe, LISTEN the session.
    let mut probe = sqlx::postgres::PgListener::connect(&proxied)
        .await
        .expect("PgListener could not connect through the proxy");
    probe
        .listen("auth_epoch")
        .await
        .expect("LISTEN through the proxy");
    next_port("canary", &mut ports).await;
    next_port("probe", &mut ports).await;

    let vars: HashMap<String, String> = HashMap::from([
        ("PS_DATABASE_URL".to_owned(), proxied),
        (
            "PS_BOOTSTRAP_TOKEN".to_owned(),
            common::BOOTSTRAP.to_owned(),
        ),
        ("PS_SIM_TOKEN".to_owned(), common::SIM_TOKEN.to_owned()),
        ("PS_PROFILE".to_owned(), "dev".to_owned()),
        ("PS_DB_ACQUIRE_TIMEOUT_MS".to_owned(), "15000".to_owned()),
        ("PS_DB_STATEMENT_TIMEOUT_MS".to_owned(), "10000".to_owned()),
        ("PS_DB_LOCK_TIMEOUT_MS".to_owned(), "10000".to_owned()),
        ("PS_REQUEST_TIMEOUT_MS".to_owned(), "60000".to_owned()),
    ]);
    let config = Config::from_lookup(|name| vars.get(name).cloned()).unwrap();
    let write = sandbox_db::connect_lazy(&direct, &config.pool).unwrap();
    let read = sandbox_db::connect_lazy(&direct, &config.read_pool()).unwrap();
    let b_state = AppState::new(config, write, read.clone());
    let tasks = b_state.tasks.clone();
    tasks.register(sandbox_server::authlisten::TASK, Duration::from_secs(30));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let spawned = std::time::Instant::now();
    let listener = tokio::spawn(sandbox_server::authlisten::run_with(
        b_state,
        stop_rx,
        Duration::from_millis(150),
    ));
    // Attached the moment the task beats — `application_name` in
    // `pg_stat_activity` lags the attach by seconds on this cluster, so
    // connection identity comes from the proxy's own forwarding ports instead.
    wait_beating(&tasks, sandbox_server::authlisten::TASK, spawned).await;
    let first_port = next_port("listen-1", &mut ports).await;
    next_port("ping-1", &mut ports).await;

    // Go silent for well over two beats: pings stop landing, the loop declares the
    // wire dead and drops both sockets…
    let _ = gate.send(false);
    tokio::time::sleep(Duration::from_millis(700)).await;
    // …then heal: the reconnect that silence detection caused makes a NEW
    // connection through the proxy — the only way another port appears.
    let _ = gate.send(true);
    let second_port = next_port("listen-2", &mut ports).await;
    assert_ne!(
        second_port, first_port,
        "the half-open socket was never declared dead — no reconnect happened"
    );
    next_port("ping-2", &mut ports).await;

    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), listener).await;
    a.cleanup().await;
}

/// N5 + N3: a tenant notifying faster than the heartbeat interval cannot starve
/// readiness — the old loop only beat on the timer branch, which a steady stream of
/// notifications starves — and the self-ping must keep a LIVE socket from being
/// rebuilt (the ping arrives, so the silence check never trips).
#[tokio::test]
async fn a_flood_of_notifications_never_starves_the_heartbeat() {
    use std::time::Duration;

    let a = TestApp::new(&[]).await;
    let (b_state, _router, b_read) = b_state(&a);
    let tasks = b_state.tasks.clone();
    // NOTIFY goes through the write pool — the read pool's default transaction is
    // read only.
    let b_write = b_state.write_db.clone();
    tasks.register(sandbox_server::authlisten::TASK, Duration::from_secs(30));
    let listen_name = b_state.listen_name.clone();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let listener = tokio::spawn(sandbox_server::authlisten::run_with(
        b_state,
        stop_rx,
        Duration::from_millis(200),
    ));
    let pid = wait_attached(&b_read, &listen_name, None).await;

    // ~0.9 s of a well-formed notification every 10 ms — several beats' worth, each
    // keeping `try_recv` ready so the timer branch never wins the select.
    let spam = {
        tokio::spawn(async move {
            for _ in 0..90 {
                let _ = sqlx::query("SELECT pg_notify('auth_epoch', $1)")
                    .bind(format!("p{}", uuid::Uuid::now_v7()))
                    .execute(&b_write)
                    .await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(700)).await;
    let since = tasks
        .statuses()
        .into_iter()
        .find(|s| s.name == sandbox_server::authlisten::TASK)
        .map_or(u64::MAX, |s| s.since_beat_ms);
    assert!(
        since < 400,
        "the heartbeat starved under notification load ({since} ms since the last beat)"
    );
    spam.await.unwrap();

    // The same backend is still attached: the self-ping arrived, so the silence
    // check never rebuilt a healthy connection (N3's detector, proven not to
    // false-positive on a live socket).
    let still = wait_attached(&b_read, &listen_name, None).await;
    assert_eq!(
        still, pid,
        "a live listener was rebuilt — the self-ping never landed"
    );

    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), listener).await;
    a.cleanup().await;
}

#[tokio::test]
async fn the_last_admin_key_cannot_be_revoked_even_by_two_concurrent_requests() {
    let app = TestApp::new(&[]).await;
    let (_, first) = app.project("store").await;
    let (_, listed) = app.get("/v1/keys", &first).await;
    let first_id = listed["api_keys"][0]["id"].as_str().unwrap().to_owned();
    let (status, reply) = app
        .post(
            &format!("/v1/keys/{first_id}/revoke"),
            &first,
            None,
            json!({}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_state"))
    );

    // Two admins revoke each other at the same moment: exactly one may succeed.
    let (second_id, second) = app.key(&first, &["admin"]).await;
    let (path_a, path_b) = (
        format!("/v1/keys/{second_id}/revoke"),
        format!("/v1/keys/{first_id}/revoke"),
    );
    let a = app.post(&path_a, &first, None, json!({}));
    let b = app.post(&path_b, &second, None, json!({}));
    let ((sa, _), (sb, _)) = tokio::join!(a, b);
    let successes = [sa, sb].iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(successes, 1, "{sa} {sb}");
    let mut alive = 0;
    for key in [&first, &second] {
        if app.get("/v1/keys", key).await.0 == StatusCode::OK {
            alive += 1;
        }
    }
    assert_eq!(alive, 1, "exactly one admin key survives");
    app.cleanup().await;
}

#[tokio::test]
async fn projects_cannot_see_or_touch_each_others_keys() {
    let app = TestApp::new(&[]).await;
    let (a_id, a_admin) = app.project("store-a").await;
    let (b_id, b_admin) = app.project("store-b").await;
    let (b_reader_id, b_reader) = app.key(&b_admin, &["read"]).await;

    let (_, a_keys) = app.get("/v1/keys", &a_admin).await;
    assert_eq!(a_keys["api_keys"].as_array().unwrap().len(), 1);
    assert!(!a_keys.to_string().contains(&b_reader_id));

    let (status, _) = app
        .post(
            &format!("/v1/keys/{b_reader_id}/revoke"),
            &a_admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another project's key is invisible"
    );
    let (status, reply) = app.get("/v1/project", &b_reader).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["project"]["id"], b_id.as_str());
    let (_, reply) = app.get("/v1/project", &a_admin).await;
    assert_eq!(reply["project"]["id"], a_id.as_str());
    app.cleanup().await;
}
