//! Routes. Probes are outside `/v1`, so they are never shed or authenticated.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;

use crate::error::ApiError;
use crate::middleware;
use crate::state::AppState;

/// The whole application.
pub fn app(state: &AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/sandbox", get(sandbox_notice))
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
    middleware::apply(router, &state.config)
}

/// Liveness: the process answers. Touches nothing.
async fn healthz() -> Response {
    Json(json!({"status": "ok", "simulated": true})).into_response()
}

/// Readiness: the database answers within the readiness deadline.
async fn readyz(State(state): State<AppState>) -> Response {
    let ping = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.db);
    match tokio::time::timeout(state.config.readiness_timeout, ping).await {
        Ok(Ok(_)) => Json(json!({"status": "ready", "simulated": true})).into_response(),
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
