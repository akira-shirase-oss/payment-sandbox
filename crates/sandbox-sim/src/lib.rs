//! The simulated card network. **SIMULATED MONEY ONLY.** A separate process with its own
//! store, a redb file (D-002, D-018, D-034): it answers approvals and declines by scenario token, records
//! every result under the operation identity the server sends, replays that result for a
//! repeated identity, and supports fenced readback (D-017), listing for reverse
//! reconciliation, a virtual test clock, and test-only fault rules.
//!
//! No real network exists behind it, and it listens on loopback only. Loopback is not a
//! credential (D-046): executions, readbacks and listings need the server's network
//! credential, and the test control plane (faults, the clock) is mounted only when the
//! startup flag turns it on, behind the operator's credential.

pub mod faults;
pub mod store;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sandbox_domain::access::Credential;
use sandbox_domain::network::ExecuteRequest;
use sandbox_domain::scenario::Scenario;
use serde::Deserialize;
use serde_json::json;
use tower_http::{
    limit::RequestBodyLimitLayer, set_header::SetResponseHeaderLayer, timeout::TimeoutLayer,
};
use uuid::Uuid;

use crate::faults::{FaultAction, FaultRule, Faults};
use crate::store::{Store, StoreError};

/// Who may call which routes (D-046).
#[derive(Debug, Clone)]
pub struct Access {
    /// The server's credential: executions, readbacks (and so fences) and listings.
    pub network: Credential,
    /// The operator's credential for faults and the virtual clock. `None` (the default
    /// outside the `dev` profile) leaves those routes unmounted.
    pub control: Option<Credential>,
}

/// Shared state.
#[derive(Debug, Clone)]
pub struct SimState {
    /// The durable store.
    pub store: Store,
    /// Test fault rules.
    pub faults: Arc<Faults>,
    /// How long a lost reply is held.
    pub lost_reply_hold: Duration,
    /// Credentials.
    pub access: Arc<Access>,
    /// Connection cap and transport deadlines (D-054).
    pub transport: sandbox_http::Limits,
    /// The listener's telemetry (D-093), reported by `/v1/stats`.
    pub http: Arc<sandbox_http::ServeStats>,
}

/// The network listener's default transport limits (chosen, not measured; D-054): more
/// connections than the server's, since the server's pool keeps many open to it.
pub const SIM_LIMITS: sandbox_http::Limits = sandbox_http::Limits::with_connections(1_024);

/// Refuses a request without the route group's credential, before its body is read.
async fn require(
    State(credential): State<Arc<Credential>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if credential.admits(presented) {
        next.run(request).await
    } else {
        error(StatusCode::UNAUTHORIZED, "credential_required")
    }
}

/// The network's HTTP API.
pub fn app(state: SimState) -> Router {
    let network = Router::new()
        .route("/v1/operations", post(execute).get(list))
        .route("/v1/operations/{id}", get(readback))
        .route("/v1/stats", get(stats))
        .route_layer(middleware::from_fn_with_state(
            Arc::new(state.access.network.clone()),
            require,
        ));
    let mut router = Router::new()
        .route(
            "/healthz",
            get(|| async { Json(json!({"status": "ok", "simulated": true})) }),
        )
        .merge(network);
    if let Some(control) = state.access.control.clone() {
        router = router.merge(
            Router::new()
                .route("/v1/clock", get(clock).post(advance))
                .route("/v1/faults", post(add_fault).delete(clear_faults))
                .route_layer(middleware::from_fn_with_state(Arc::new(control), require)),
        );
    }
    router
        .with_state(state)
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::HeaderName::from_static("sandbox-simulated"),
            HeaderValue::from_static("true"),
        ))
        // Longer than any lost-reply hold, so a held reply is ended by the hold, not here.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            Duration::from_secs(120),
        ))
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

fn error(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code}, "simulated": true})),
    )
        .into_response()
}

fn store_error(e: &StoreError) -> Response {
    match e {
        StoreError::Conflict => error(StatusCode::CONFLICT, "operation_id_conflict"),
        StoreError::Fenced => error(StatusCode::GONE, "operation_fenced"),
        StoreError::Invalid(what) => error(StatusCode::UNPROCESSABLE_ENTITY, what),
        StoreError::Overloaded => error(StatusCode::SERVICE_UNAVAILABLE, "network_overloaded"),
        StoreError::Closed => error(StatusCode::SERVICE_UNAVAILABLE, "network_stopping"),
        StoreError::Storage(cause) => {
            tracing::error!(%cause, "network store failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "store_failed")
        }
    }
}

async fn execute(State(state): State<SimState>, Json(request): Json<ExecuteRequest>) -> Response {
    let target = request.parent_id.unwrap_or(request.id);
    let fault = state.faults.take(request.kind, target);
    // The authorization's own scenario applies to the authorization only.
    let scenario = if request.kind == sandbox_domain::OperationKind::Authorize {
        request
            .token
            .as_deref()
            .and_then(|t| Scenario::from_token(t).ok())
    } else {
        None
    };
    if fault == Some(FaultAction::Unavailable) || scenario == Some(Scenario::Unavailable) {
        tracing::info!(id = %request.id, kind = %request.kind, "network unavailable (not executed)");
        return error(StatusCode::SERVICE_UNAVAILABLE, "network_unavailable");
    }
    let executed = match state.store.execute(&request).await {
        Ok(executed) => executed,
        Err(e) => return store_error(&e),
    };
    let first_time = !executed.reply.replayed;
    let lose = first_time
        && (fault == Some(FaultAction::LostReply) || scenario == Some(Scenario::LostReply));
    if lose {
        tracing::info!(id = %request.id, kind = %request.kind, "executed; holding the reply (lost reply)");
        tokio::time::sleep(state.lost_reply_hold).await;
    } else if let Some(FaultAction::Delay { delay_ms }) = fault {
        tokio::time::sleep(Duration::from_millis(delay_ms.min(60_000))).await;
    }
    Json(executed.reply).into_response()
}

#[derive(Debug, Deserialize)]
struct ReadbackQuery {
    #[serde(default)]
    fence: bool,
}

async fn readback(
    State(state): State<SimState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ReadbackQuery>,
) -> Response {
    match state.store.readback(id, q.fence).await {
        Ok(readback) => Json(readback).into_response(),
        Err(e) => store_error(&e),
    }
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    project_id: Option<Uuid>,
    #[serde(default)]
    after: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}

const fn default_limit() -> i64 {
    500
}

/// The network's own telemetry (D-093): its write path (batch sizes, durable-commit times,
/// queue waits, sheds) and its listener's (cap waits, refusals). Counters since startup.
async fn stats(State(state): State<SimState>) -> Response {
    Json(json!({
        "store": state.store.stats(),
        "transport": state.http.snapshot(),
        "simulated": true,
    }))
    .into_response()
}

async fn list(State(state): State<SimState>, Query(q): Query<ListQuery>) -> Response {
    let page = match q.project_id {
        Some(project) => state.store.list_project(project, q.after, q.limit).await,
        None => state.store.list(q.after, q.limit).await,
    };
    match page {
        Ok(page) => Json(page).into_response(),
        Err(e) => store_error(&e),
    }
}

async fn clock(State(state): State<SimState>) -> Response {
    match state.store.now().await {
        Ok(now) => Json(json!({"now_seconds": now, "simulated": true})).into_response(),
        Err(e) => store_error(&e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Advance {
    seconds: i64,
}

async fn advance(State(state): State<SimState>, Json(body): Json<Advance>) -> Response {
    if body.seconds.unsigned_abs() > 10 * 365 * 24 * 3600 {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "jump_too_large");
    }
    match state.store.advance(body.seconds).await {
        Ok(now) => Json(json!({"now_seconds": now, "simulated": true})).into_response(),
        Err(e) => store_error(&e),
    }
}

async fn add_fault(State(state): State<SimState>, Json(rule): Json<FaultRule>) -> Response {
    match state.faults.add(rule) {
        Ok(()) => (StatusCode::CREATED, Json(json!({"added": true}))).into_response(),
        Err(_) => error(StatusCode::CONFLICT, "too_many_rules"),
    }
}

async fn clear_faults(State(state): State<SimState>) -> Response {
    state.faults.clear();
    Json(json!({"cleared": true})).into_response()
}

/// Serves until `shutdown` resolves within the transport limits (D-054), then drains for at
/// most `grace` (held lost replies would otherwise keep it alive for their whole hold).
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: SimState,
    grace: Duration,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let limits = state.transport;
    let telemetry = state.http.clone();
    sandbox_http::serve(listener, app(state), limits, grace, shutdown, telemetry)
        .await
        .map(|_| ())
}
