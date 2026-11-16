//! The service skeleton, in-process: probes, the error envelope, body limits, deadlines,
//! load shedding, idempotency-key and card-data rules, and the first failure drill — the
//! database going away (refused connections and a black hole that never answers).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware as axum_mw,
    routing::{get, post},
};
use http_body_util::BodyExt;
use sandbox_db::testing::TestDb;
use sandbox_server::extract::{IdempotencyKeyHeader, SandboxJson};
use sandbox_server::{AppState, Config, app, middleware};
use serde_json::Value;
use tower::ServiceExt;

fn config(database_url: &str, extra: &[(&str, &str)]) -> Config {
    let mut vars: HashMap<String, String> = HashMap::from([
        ("PS_DATABASE_URL".to_owned(), database_url.to_owned()),
        ("PS_DB_ACQUIRE_TIMEOUT_MS".to_owned(), "300".to_owned()),
        ("PS_READINESS_TIMEOUT_MS".to_owned(), "500".to_owned()),
    ]);
    for (k, v) in extra {
        vars.insert((*k).to_owned(), (*v).to_owned());
    }
    Config::from_lookup(|name| vars.get(name).cloned()).unwrap()
}

fn state(config: Config) -> AppState {
    let db = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool).unwrap();
    AppState::new(config, db)
}

async fn call(
    router: &Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, json)
}

fn get_req(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn probes_answer_and_every_response_is_labelled_simulated() {
    let db = TestDb::new().await;
    let router = app(&state(config(&db.url(), &[])));
    let (status, headers, body) = call(&router, get_req("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["sandbox-simulated"], "true");
    assert!(headers.contains_key("x-request-id"));
    assert_eq!(body["simulated"], true);
    let (status, _, body) = call(&router, get_req("/readyz")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call(&router, get_req("/v1/sandbox")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["currencies"].as_array().map(Vec::len), Some(4));
    db.cleanup().await;
}

#[tokio::test]
async fn errors_from_routing_share_one_envelope_with_the_request_id() {
    let router = app(&state(config("postgres://x@127.0.0.1:1/none", &[])));
    let request = Request::get("/nope")
        .header("x-request-id", "trace-123")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = call(&router, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "route_not_found");
    assert_eq!(body["error"]["request_id"], "trace-123");
    assert_eq!(body["simulated"], true);
    assert_eq!(headers["x-request-id"], "trace-123");

    let (status, _, body) = call(
        &router,
        Request::post("/healthz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"]["code"], "method_not_allowed");

    // An unsafe client id is replaced, never echoed.
    let request = Request::get("/nope")
        .header("x-request-id", "bad id\twith tab")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = call(&router, request).await;
    assert_ne!(headers["x-request-id"], "bad id\twith tab");
}

/// Drill: the database refuses connections, or accepts them and never answers. Liveness
/// stays green, readiness turns red within its deadline, and nothing hangs.
#[tokio::test]
async fn readiness_fails_fast_when_the_database_is_down_or_a_black_hole() {
    let refused = app(&state(config("postgres://x:y@127.0.0.1:1/none", &[])));
    let black_hole = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = black_hole.local_addr().unwrap();
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = black_hole.accept().await {
            open.push(socket);
        }
    });
    let silent = app(&state(config(&format!("postgres://x:y@{addr}/none"), &[])));
    for router in [&refused, &silent] {
        let started = Instant::now();
        let (status, _, body) = call(router, get_req("/readyz")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "not_ready");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let (status, _, _) = call(router, get_req("/healthz")).await;
        assert_eq!(status, StatusCode::OK);
    }
    held.abort();
}

fn test_router(cfg: &Config, state: &AppState) -> Router {
    let routes = Router::new()
        .route(
            "/echo",
            post(
                |IdempotencyKeyHeader(key): IdempotencyKeyHeader,
                 SandboxJson(v): SandboxJson<Value>| async move {
                    axum::Json(serde_json::json!({"key": key.as_str(), "body": v}))
                },
            ),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "late"
            }),
        )
        .route(
            "/panic",
            get(|| async {
                let fail: Result<&str, &str> = std::hint::black_box(Err("secret detail"));
                fail.unwrap()
            }),
        )
        .route_layer(axum_mw::from_fn_with_state(
            state.gate.clone(),
            middleware::shed_load,
        ));
    middleware::apply(routes, cfg)
}

fn echo(key: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::post("/echo").header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn bodies_keys_and_card_data_follow_the_rules() {
    let cfg = config(
        "postgres://x@127.0.0.1:1/none",
        &[("PS_MAX_BODY_BYTES", "1024")],
    );
    let st = state(cfg.clone());
    let router = test_router(&cfg, &st);

    let (status, _, body) = call(&router, echo(Some("order-1"), r#"{"a": 1}"#)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["key"], "order-1");

    let cases = [
        (
            echo(None, "{}"),
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
        ),
        (
            echo(Some("has space"), "{}"),
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
        ),
        (
            echo(Some("4242424242424242"), "{}"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "raw_card_data_rejected",
        ),
        (
            echo(Some("k"), r#"{"number": "x"}"#),
            StatusCode::UNPROCESSABLE_ENTITY,
            "raw_card_data_rejected",
        ),
        (
            echo(Some("k"), r#"{"note": "5555 5555 5555 4444"}"#),
            StatusCode::UNPROCESSABLE_ENTITY,
            "raw_card_data_rejected",
        ),
        (
            echo(Some("k"), "{\"a\": "),
            StatusCode::BAD_REQUEST,
            "malformed_json",
        ),
        (
            echo(Some("k"), &format!("{{\"a\": \"{}\"}}", "x".repeat(2048))),
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
        ),
        (
            Request::post("/echo")
                .header("idempotency-key", "k")
                .body(Body::from("{}"))
                .unwrap(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        ),
    ];
    for (request, want_status, want_code) in cases {
        let (status, _, body) = call(&router, request).await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (want_status, Some(want_code)),
            "{body}"
        );
        let text = body.to_string();
        assert!(
            !text.contains("4242") && !text.contains("5555"),
            "card digits echoed: {text}"
        );
    }
}

#[tokio::test]
async fn slow_handlers_hit_the_deadline_and_panics_become_opaque_500s() {
    let cfg = config(
        "postgres://x@127.0.0.1:1/none",
        &[
            ("PS_REQUEST_TIMEOUT_MS", "300"),
            ("PS_DB_STATEMENT_TIMEOUT_MS", "100"),
        ],
    );
    let st = state(cfg.clone());
    let router = test_router(&cfg, &st);
    let started = Instant::now();
    let (status, _, body) = call(&router, get_req("/slow")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "unavailable");
    assert!(started.elapsed() < Duration::from_secs(2));

    let (status, _, body) = call(&router, get_req("/panic")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], "internal");
    assert!(!body.to_string().contains("secret detail"));
}

#[tokio::test]
async fn excess_in_flight_requests_are_shed_immediately() {
    let cfg = config(
        "postgres://x@127.0.0.1:1/none",
        &[("PS_MAX_IN_FLIGHT", "1")],
    );
    let st = state(cfg.clone());
    let router = test_router(&cfg, &st);
    let first = tokio::spawn({
        let router = router.clone();
        async move { call(&router, get_req("/slow")).await }
    });
    // Wait until the first request holds the only permit.
    let deadline = Instant::now() + Duration::from_secs(5);
    while st.gate.available_permits() > 0 {
        assert!(Instant::now() < deadline, "the first request never started");
        tokio::task::yield_now().await;
    }
    let started = Instant::now();
    let (status, headers, body) = call(&router, get_req("/slow")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "overloaded");
    assert_eq!(headers["retry-after"], "1");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "shed, not queued"
    );
    first.abort();
}
