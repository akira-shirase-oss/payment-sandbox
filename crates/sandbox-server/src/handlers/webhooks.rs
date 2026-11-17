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
    webhook::parse_target(&body.url).map_err(|e| ApiError::InvalidBody(e.to_string()))?;
    let id = Uuid::now_v7();
    let secret = new_secret()?;
    let sealed = secret_box
        .seal(&secret, auth.scope().project_id, id)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    let endpoint =
        sandbox_db::webhooks::create_endpoint(&state.db, auth.scope(), id, &body.url, &sealed)
            .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"endpoint": endpoint, "secret": secret, "simulated": true})),
    )
        .into_response())
}

/// Lists endpoints (never their secrets).
pub async fn list(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let endpoints = sandbox_db::webhooks::list_endpoints(&state.db, auth.scope()).await?;
    Ok(Json(json!({"endpoints": endpoints, "simulated": true})).into_response())
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
