//! Routes. Probes are outside `/v1`, so they are never shed or authenticated.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;

use crate::error::ApiError;
use crate::handlers;
use crate::middleware;
use crate::state::AppState;

/// The whole application.
pub fn app(state: &AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/sandbox", get(sandbox_notice))
        .route("/v1/projects", post(handlers::projects::create))
        .route("/v1/project", get(handlers::projects::current))
        .route("/v1/project/reset", post(handlers::projects::reset))
        .route(
            "/v1/keys",
            get(handlers::keys::list).post(handlers::keys::create),
        )
        .route("/v1/keys/{key_id}/revoke", post(handlers::keys::revoke))
        .route(
            "/v1/payments",
            get(handlers::payments::list).post(handlers::payments::create),
        )
        .route("/v1/payments/{payment_id}", get(handlers::payments::get))
        .route(
            "/v1/payments/{payment_id}/journal",
            get(handlers::payments::journal),
        )
        .route(
            "/v1/payments/{payment_id}/captures",
            post(handlers::payments::capture),
        )
        .route(
            "/v1/payments/{payment_id}/void",
            post(handlers::payments::void),
        )
        .route(
            "/v1/payments/{payment_id}/refunds",
            post(handlers::payments::refund),
        )
        .route("/v1/balances", get(handlers::payments::balances))
        .route("/v1/checkout_sessions", post(handlers::checkout::create))
        .route("/v1/checkout_sessions/{id}", get(handlers::checkout::get))
        .route(
            "/v1/webhook_endpoints",
            get(handlers::webhooks::list).post(handlers::webhooks::create),
        )
        .route(
            "/v1/webhook_endpoints/{endpoint_id}/rotate",
            post(handlers::webhooks::rotate),
        )
        .route(
            "/v1/webhook_endpoints/{endpoint_id}/disable",
            post(handlers::webhooks::disable),
        )
        .route("/v1/events", get(handlers::webhooks::events))
        // One classification, applied to both budgets (D-101): `shed_load` admits by
        // method, `classify_pool` hands the request its pool by the same rule.
        .route_layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::classify_pool,
        ))
        .route_layer(axum_mw::from_fn_with_state(
            state.gate.clone(),
            middleware::shed_load,
        ));
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(v1)
        .fallback(|| async { ApiError::RouteNotFound })
        .with_state(state.clone());
    // Reports scan the whole epoch: their own, longer (still bounded) deadline.
    let reports = Router::new()
        .route(
            "/v1/reconciliation",
            get(handlers::payments::reconciliation),
        )
        .route(
            "/v1/balances/{currency}/{account}/explain",
            get(handlers::payments::explain),
        )
        .route_layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::classify_pool,
        ))
        .route_layer(axum_mw::from_fn_with_state(
            state.gate.clone(),
            middleware::shed_load,
        ))
        .with_state(state.clone());
    middleware::apply(router, &state.config).merge(middleware::apply_with_timeout(
        reports,
        &state.config,
        REPORT_TIMEOUT,
    ))
}

/// Deadline of report requests (`/v1/reconciliation`).
pub const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Liveness: the process answers. Touches nothing.
async fn healthz() -> Response {
    Json(json!({"status": "ok", "simulated": true})).into_response()
}

/// Readiness: the database answers within the readiness deadline, and every supervised
/// background task is running and beating (D-051).
async fn readyz(State(state): State<AppState>) -> Response {
    let ping = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.read_db);
    match tokio::time::timeout(state.config.readiness_timeout, ping).await {
        Ok(Ok(_)) => {
            let tasks = state.tasks.statuses();
            if let Some(down) = state.tasks.unhealthy() {
                tracing::warn!(
                    task = down.name,
                    running = down.running,
                    stale = down.stale,
                    "readiness: a background task is down or stuck"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"status": "not_ready", "reason": "background_task", "task": down.name, "tasks": tasks, "simulated": true})),
                )
                    .into_response();
            }
            Json(json!({"status": "ready", "tasks": tasks, "simulated": true})).into_response()
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "readiness: database ping failed");
            not_ready()
        }
        Err(_) => {
            tracing::warn!("readiness: database ping exceeded its deadline");
            not_ready()
        }
    }
}

fn not_ready() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"status": "not_ready", "reason": "database", "simulated": true})),
    )
        .into_response()
}

/// States what this server is, for humans and for SDKs that check before use.
async fn sandbox_notice() -> Response {
    Json(json!({
        "simulated": true,
        "notice": sandbox_domain::SANDBOX_NOTICE,
        "ledger_semantics_version": sandbox_domain::LEDGER_SEMANTICS_VERSION,
        "currencies": sandbox_domain::Currency::ALL.iter().map(|c| json!({"code": c.code(), "exponent": c.exponent()})).collect::<Vec<_>>(),
    }))
    .into_response()
}
