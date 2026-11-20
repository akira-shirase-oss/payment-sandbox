//! Sandbox key management (admin scope). Rotation is: create a second key, move clients,
//! revoke the first (D-021).

use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sandbox_db::DbError;
use sandbox_db::keys::{NewKey, RevokeOutcome};
use sandbox_domain::keys::{self, Scope};
use serde::Deserialize;
use serde_json::json;
use sqlx::Acquire as _;
use uuid::Uuid;

use crate::auth::{self, Auth};
use crate::error::ApiError;
use crate::extract::{PathId, SandboxJson};
use crate::middleware::DbPool;
use crate::state::AppState;

/// `POST /v1/keys` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateKey {
    label: String,
    scopes: Vec<String>,
}

/// Creates a key; the secret is in this response only.
pub async fn create(
    State(state): State<AppState>,
    Extension(db): Extension<DbPool>,
    auth: Auth,
    SandboxJson(input): SandboxJson<CreateKey>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let label = sandbox_domain::project::validate_name(&input.label)
        .map_err(|e| ApiError::InvalidBody(e.replace("name", "label")))?;
    if label.chars().count() > 100 {
        return Err(ApiError::InvalidBody(
            "label must be at most 100 characters".to_owned(),
        ));
    }
    let mut scopes = Vec::new();
    for code in &input.scopes {
        let scope = Scope::from_code(code).ok_or_else(|| {
            ApiError::InvalidBody("scopes must be read, payments:write or admin".to_owned())
        })?;
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    if scopes.is_empty() {
        return Err(ApiError::InvalidBody(
            "a key needs at least one scope".to_owned(),
        ));
    }
    let secret = auth::generate_key()?;
    let hash = keys::hash_presented(&secret).map_err(|_| {
        ApiError::Internal(anyhow::anyhow!("generated key failed its own shape check"))
    })?;
    let prefix = keys::display_prefix(&secret);
    let mut conn = crate::poolstat::timed_acquire(&db.0, &state.pool_stats.write).await?;
    let mut tx = conn.begin().await.map_err(DbError::from)?;
    let record = sandbox_db::keys::insert(
        &mut tx,
        auth.scope().project_id,
        &NewKey {
            id: Uuid::now_v7(),
            hash,
            prefix: &prefix,
            scopes: &scopes,
            label,
        },
        &format!("key:{}", auth.key_id()),
    )
    .await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"api_key": record, "secret": secret, "simulated": true})),
    )
        .into_response())
}

/// Lists the project's keys (never their secrets).
pub async fn list(Extension(db): Extension<DbPool>, auth: Auth) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let keys = sandbox_db::keys::list(&db.0, auth.scope().project_id).await?;
    Ok(Json(json!({"api_keys": keys, "simulated": true})).into_response())
}

/// Revokes a key of the caller's project. Idempotent; refuses the last admin key.
pub async fn revoke(
    State(state): State<AppState>,
    Extension(db): Extension<DbPool>,
    auth: Auth,
    PathId(key_id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Admin)?;
    let actor = format!("key:{}", auth.key_id());
    match sandbox_db::keys::revoke(&db.0, auth.scope().project_id, key_id, &actor).await? {
        RevokeOutcome::Revoked => {
            // D-102: a revoked key fails on its very next request — the cache drops it now,
            // it never waits for the TTL.
            state.key_cache.invalidate_key(key_id);
            Ok(Json(json!({"revoked": key_id, "simulated": true})).into_response())
        }
        RevokeOutcome::NotFound => Err(ApiError::NotFound("key")),
        RevokeOutcome::LastAdmin => Err(ApiError::InvalidState(
            "this is the project's last active admin key; create another admin key first"
                .to_owned(),
        )),
    }
}
