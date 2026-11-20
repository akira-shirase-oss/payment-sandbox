//! Sandbox-key authentication. Every `/v1` request (other than project bootstrap) carries
//! `Authorization: Bearer sk_sim_…`; the key's SHA-256 is looked up on every request, so a
//! revoked key fails on its next request — no cache can keep it alive.

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};
use sandbox_db::ProjectScope;
use sandbox_db::keys::KeyIdentity;
use sandbox_domain::keys::{self, Scope};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// An authenticated request: the key's project (and epoch) and scopes.
#[derive(Debug, Clone)]
pub struct Auth(pub KeyIdentity);

/// An identity a front door has already authenticated (the dashboard's session, D-062),
/// placed in the request's extensions by that front door's middleware. Nothing outside the
/// server can put one there; the API itself never does, so `/v1` always needs a key.
#[derive(Debug, Clone)]
pub struct Delegated(pub KeyIdentity);

impl Auth {
    /// The project scope every query of this request must use.
    pub const fn scope(&self) -> ProjectScope {
        self.0.scope
    }

    /// The key's id (the actor in audit events).
    pub const fn key_id(&self) -> Uuid {
        self.0.key_id
    }

    /// Refuses unless the key holds `required` (or a scope implying it).
    pub fn require(&self, required: Scope) -> Result<(), ApiError> {
        if keys::permits(&self.0.scopes, required) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(required.code()))
        }
    }
}

/// The bearer token of a request, if well formed.
pub fn bearer(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(Delegated(identity)) = parts.extensions.get::<Delegated>() {
            return Ok(Self(identity.clone()));
        }
        let token = bearer(parts).ok_or(ApiError::Unauthorized)?;
        let hash = keys::hash_presented(token).map_err(|_| ApiError::Unauthorized)?;
        // The classifier already chose this request's pool (D-101): a read's lookup
        // pays from the read pool, so a write flood that holds every write connection
        // cannot make a read fail at its credential check. `db_for` is the fallback
        // for a request that never passed `classify_pool`.
        let pool = parts
            .extensions
            .get::<crate::middleware::DbPool>()
            .map_or_else(|| state.db_for(&parts.method), |db| &db.0);
        let identity = sandbox_db::keys::resolve(pool, &hash)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        Ok(Self(identity))
    }
}

/// True when the request presents the configured bootstrap token (compared as SHA-256
/// digests in constant time).
pub fn is_bootstrap(parts: &Parts, state: &AppState) -> bool {
    let (Some(expected), Some(presented)) = (&state.config.bootstrap_token, bearer(parts)) else {
        return false;
    };
    keys::constant_time_eq(&keys::digest(expected.expose()), &keys::digest(presented))
}

/// Generates a new key from OS randomness.
pub fn generate_key() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OS randomness unavailable: {e}")))?;
    Ok(keys::key_from_bytes(bytes))
}
