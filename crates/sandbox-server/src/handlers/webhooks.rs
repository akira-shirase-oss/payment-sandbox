//! Webhook endpoints (admin) and events (read).

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sandbox_domain::keys::Scope;
use sandbox_domain::webhook;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::ApiError;
use crate::extract::{PathId, SandboxJson};
use crate::state::AppState;

/// Why a target port is refused (review N10, D-059).
pub const PORT_REFUSED: &str = "webhooks may not target the sandbox's own ports or PostgreSQL's, and other ports need PS_WEBHOOK_PORTS (or PS_PROFILE=dev)";

/// How long a rotated-out secret keeps signing.
pub const ROTATION_OVERLAP: std::time::Duration = std::time::Duration::from_hours(24);

/// `POST /v1/webhook_endpoints` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEndpoint {
    url: String,
}

fn secrets(state: &AppState) -> Result<&crate::secrets::SecretBox, ApiError> {
    state.secrets.as_deref().ok_or_else(|| {
        ApiError::InvalidState("webhooks are off: the server has no PS_SECRET_KEY".to_owned())
    })
}

fn new_secret() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OS randomness unavailable: {e}")))?;
    Ok(webhook::secret_from_bytes(bytes))
}

/// Registers an endpoint; its signing secret is in this response only.
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    SandboxJson(body): SandboxJson<CreateEndpoint>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let secret_box = secrets(&state)?;
    let target =
        webhook::parse_target(&body.url).map_err(|e| ApiError::InvalidBody(e.to_string()))?;
    if !state.config.webhook_ports.permits(target.port) {
        return Err(ApiError::InvalidBody(PORT_REFUSED.to_owned()));
    }
    let id = Uuid::now_v7();
    let secret = new_secret()?;
    let sealed = secret_box
        .seal(&secret, auth.scope().project_id, id)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    let endpoint =
        sandbox_db::webhooks::create_endpoint(&state.db, auth.scope(), id, &body.url, &sealed)
            .await?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "a project has at most {} active webhook endpoints; disable one first",
                    sandbox_db::webhooks::MAX_ENDPOINTS_PER_PROJECT
                ))
            })?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"endpoint": endpoint, "secret": secret, "simulated": true})),
    )
        .into_response())
}

/// `GET /v1/webhook_endpoints` query: a page of at most `limit` (1–100, default 100) after
/// the endpoint `after`, as `next_after` returns it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListEndpoints {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// Lists endpoints (never their secrets), a page at a time (review N4).
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListEndpoints>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 100);
    let endpoints =
        sandbox_db::webhooks::list_endpoints(&state.db, auth.scope(), q.after, limit).await?;
    let next_after = (endpoints.len() == usize::try_from(limit).unwrap_or(0))
        .then(|| endpoints.last().map(|e| e.id))
        .flatten();
    Ok(
        Json(json!({"endpoints": endpoints, "next_after": next_after, "simulated": true}))
            .into_response(),
    )
}

/// Rotates the signing secret; the old one keeps signing for 24 hours.
pub async fn rotate(
    State(state): State<AppState>,
    auth: Auth,
    PathId(id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let secret_box = secrets(&state)?;
    let secret = new_secret()?;
    let sealed = secret_box
        .seal(&secret, auth.scope().project_id, id)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    if !sandbox_db::webhooks::rotate(&state.db, auth.scope(), id, &sealed, ROTATION_OVERLAP).await?
    {
        return Err(ApiError::NotFound("endpoint"));
    }
    Ok(Json(json!({"secret": secret, "previous_valid_for_seconds": ROTATION_OVERLAP.as_secs(), "simulated": true})).into_response())
}

/// Disables an endpoint.
pub async fn disable(
    State(state): State<AppState>,
    auth: Auth,
    PathId(id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    if !sandbox_db::webhooks::disable(&state.db, auth.scope(), id).await? {
        return Err(ApiError::NotFound("endpoint"));
    }
    Ok(Json(json!({"disabled": id, "simulated": true})).into_response())
}

/// `GET /v1/events` query.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    payment_id: Option<Uuid>,
    limit: Option<i64>,
}

/// Recent events with their delivery states.
pub async fn events(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let events = sandbox_db::webhooks::events(
        &state.db,
        auth.scope(),
        q.payment_id,
        q.limit.unwrap_or(100),
    )
    .await?;
    Ok(Json(json!({"events": events, "simulated": true})).into_response())
}
