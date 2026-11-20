//! `POST /v1/checkout_sessions` and `GET /v1/checkout_sessions/{id}`: the merchant's side of
//! the hosted checkout (D-060). The merchant fixes what is paid; the answer carries the
//! payer's URL, whose fragment holds the session's client secret (shown once, never stored).

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sandbox_domain::Currency;
use sandbox_domain::checkout::{self, CaptureMethod};
use sandbox_domain::keys::Scope;
use serde::Deserialize;

use crate::auth::Auth;
use crate::error::ApiError;
use crate::extract::{PathId, SandboxJson};
use crate::state::AppState;

/// `POST /v1/checkout_sessions` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    amount: i64,
    currency: String,
    description: Option<String>,
    capture_method: Option<String>,
    embed_origins: Option<Vec<String>>,
    return_url: Option<String>,
    expires_in: Option<i64>,
}

fn invalid(error: impl std::fmt::Display) -> ApiError {
    ApiError::InvalidBody(error.to_string())
}

/// Creates a session; `201` with the session, its client secret and the payer's URL.
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    SandboxJson(body): SandboxJson<CreateSession>,
) -> Result<Response, ApiError> {
    auth.require(Scope::PaymentsWrite)?;
    let amount =
        sandbox_domain::Amount::new(body.amount).map_err(|e| invalid(format!("amount: {e}")))?;
    let currency = Currency::from_code(&body.currency).map_err(invalid)?;
    if let Some(description) = &body.description {
        checkout::validate_description(description).map_err(invalid)?;
    }
    let capture_method = match body.capture_method.as_deref() {
        None => CaptureMethod::Automatic,
        Some(code) => CaptureMethod::from_code(code)
            .ok_or_else(|| invalid("capture_method must be automatic or manual"))?,
    };
    let embed_origins = checkout::embed_origins(body.embed_origins.as_deref().unwrap_or_default())
        .map_err(invalid)?;
    if let Some(url) = &body.return_url {
        checkout::validate_return_url(url).map_err(invalid)?;
    }
    let lifetime = checkout::validate_lifetime(
        body.expires_in
            .unwrap_or(checkout::DEFAULT_LIFETIME_SECONDS),
    )
    .map_err(invalid)?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OS randomness unavailable: {e}")))?;
    let secret = checkout::secret_from_bytes(random);
    let id = uuid::Uuid::now_v7();
    let session = sandbox_db::checkout::create(
        &state.write_db,
        auth.scope(),
        &sandbox_db::checkout::NewSession {
            id,
            secret_hash: sandbox_domain::keys::digest(&secret),
            amount,
            currency,
            description: body.description.as_deref(),
            capture_method,
            embed_origins: &embed_origins,
            return_url: body.return_url.as_deref(),
            lifetime_seconds: lifetime,
        },
    )
    .await?;
    let mut view = crate::checkout::merchant_view(&state.write_db, &session).await?;
    view["client_secret"] = serde_json::json!(secret);
    view["url"] = serde_json::json!(format!(
        "{}/c/{}#{}",
        state.config.checkout_origin, session.id, secret
    ));
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

/// A session with its attempts and their payments.
pub async fn get(
    State(state): State<AppState>,
    auth: Auth,
    PathId(id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let session = sandbox_db::checkout::get(&state.read_db, auth.scope(), id)
        .await?
        .ok_or(ApiError::NotFound("checkout session"))?;
    Ok(Json(crate::checkout::merchant_view(&state.read_db, &session).await?).into_response())
}
