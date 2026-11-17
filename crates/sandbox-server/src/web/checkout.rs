//! The hosted checkout's origin (D-060, D-061): the payer's page at `/c/{session}` and its
//! API under `/api/sessions/{session}`.
//!
//! The page holds no cookie and no sandbox key. It authenticates with the session's client
//! secret, which reached it in the URL's fragment (never sent to a server, never in a
//! `Referer`), as `Authorization: Bearer cs_sim_…`; the server keeps only its SHA-256. A
//! state change must also come from this origin (D-062). The page may be framed only by
//! the origins its session names (`frame-ancestors`), and by nobody when it names none.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self as axum_mw, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sandbox_db::checkout::{AttemptError, Begun, Session};
use sandbox_domain::checkout as rules;
use sandbox_domain::keys;
use sandbox_domain::scenario::Scenario;
use serde::Deserialize;
use uuid::Uuid;

use super::site::Site;
use crate::error::ApiError;
use crate::extract::{IdempotencyKeyHeader, PathId, SandboxJson};
use crate::state::AppState;

/// The checkout site's state.
#[derive(Debug, Clone)]
pub struct Checkout {
    state: AppState,
    site: Arc<Site>,
    origin: Arc<str>,
}

/// The checkout origin's whole application.
pub fn router(state: &AppState, site: Arc<Site>, origin: &str) -> Router {
    let ctx = Checkout {
        state: state.clone(),
        site,
        origin: Arc::from(origin),
    };
    let api = Router::new()
        .route("/api/sessions/{id}", get(view))
        .route("/api/sessions/{id}/attempts", post(attempt))
        .route("/api/sessions/{id}/continue", post(resume))
        .route_layer(axum_mw::from_fn_with_state(ctx.clone(), same_origin))
        .route_layer(axum_mw::from_fn_with_state(
            state.gate.clone(),
            crate::middleware::shed_load,
        ));
    let app = Router::new()
        .merge(api)
        .route("/c/{id}", get(page))
        .fallback(asset)
        .with_state(ctx);
    // An attempt may call the network twice (authorize, then capture): room for both.
    let timeout = state
        .config
        .request_timeout
        .saturating_add(state.config.sim_timeout);
    crate::middleware::apply_with_timeout(app, &state.config, timeout)
        .layer(axum_mw::map_response(harden))
}

async fn harden(mut response: Response) -> Response {
    super::harden(response.headers_mut());
    response
}

async fn same_origin(State(ctx): State<Checkout>, request: Request, next: Next) -> Response {
    match super::require_same_origin(request.method(), request.headers(), &ctx.origin) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

/// The session `id`, if the request presents its client secret. An unknown session and a
/// wrong secret are the same answer, so the API is no oracle for either.
async fn authenticate(ctx: &Checkout, id: Uuid, headers: &HeaderMap) -> Result<Session, ApiError> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|secret| rules::is_secret_shaped(secret))
        .ok_or(ApiError::Unauthorized)?;
    let session = sandbox_db::checkout::find(&ctx.state.db, id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if keys::constant_time_eq(&keys::digest(presented), &session.secret_hash) {
        Ok(session)
    } else {
        Err(ApiError::Unauthorized)
    }
}

async fn view(
    State(ctx): State<Checkout>,
    PathId(id): PathId,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session = authenticate(&ctx, id, &headers).await?;
    Ok(Json(crate::checkout::payer_view(&ctx.state, &session).await?).into_response())
}

/// `POST /api/sessions/{id}/attempts` body: the scenario the payer picked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptBody {
    token: String,
}

async fn attempt(
    State(ctx): State<Checkout>,
    PathId(id): PathId,
    headers: HeaderMap,
    IdempotencyKeyHeader(key): IdempotencyKeyHeader,
    SandboxJson(body): SandboxJson<AttemptBody>,
) -> Result<Response, ApiError> {
    let session = authenticate(&ctx, id, &headers).await?;
    Scenario::from_token(&body.token).map_err(|e| ApiError::InvalidPaymentToken(e.to_string()))?;
    let attempt = match sandbox_db::checkout::begin_attempt(
        &ctx.state.db,
        &session,
        key.as_str(),
        &body.token,
    )
    .await
    {
        Ok(Begun::Started(attempt) | Begun::Existing(attempt)) => attempt,
        Err(AttemptError::NotFound) => return Err(ApiError::Unauthorized),
        Err(AttemptError::KeyConflict) => return Err(ApiError::IdempotencyKeyConflict),
        Err(AttemptError::Open) => return Err(ApiError::CheckoutAttemptOpen),
        Err(AttemptError::Exhausted) => return Err(ApiError::CheckoutAttemptsExhausted),
        Err(AttemptError::Expired) => return Err(ApiError::CheckoutExpired),
        Err(AttemptError::Db(db)) => return Err(db.into()),
    };
    finish(&ctx, &session, &attempt).await
}

/// `POST /api/sessions/{id}/continue` body (empty object).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// Re-drives the latest attempt: after the reconciler resolved an uncertain answer, or a
/// server restart, the page finishes without the key its tab may have lost.
async fn resume(
    State(ctx): State<Checkout>,
    PathId(id): PathId,
    headers: HeaderMap,
    SandboxJson(Empty {}): SandboxJson<Empty>,
) -> Result<Response, ApiError> {
    let session = authenticate(&ctx, id, &headers).await?;
    let Some(attempt) = sandbox_db::checkout::attempts(&ctx.state.db, &session)
        .await?
        .pop()
    else {
        return Err(ApiError::InvalidState(
            "nothing to continue: no attempt was made".to_owned(),
        ));
    };
    finish(&ctx, &session, &attempt).await
}

/// Drives the attempt, then answers with what is recorded. Only failures worth a retry
/// (busy, unavailable) are errors; anything the network decided is in the view.
async fn finish(
    ctx: &Checkout,
    session: &Session,
    attempt: &sandbox_db::checkout::Attempt,
) -> Result<Response, ApiError> {
    if let Err(error) = crate::checkout::drive(&ctx.state, session, attempt).await {
        if error.status().is_server_error() {
            return Err(error);
        }
        tracing::info!(code = ?error.code(), "checkout attempt refused");
    }
    let session = sandbox_db::checkout::find(&ctx.state.db, session.id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(crate::checkout::payer_view(&ctx.state, &session).await?).into_response())
}

/// The page of session `raw`: framed only by the origins the session names.
async fn page(State(ctx): State<Checkout>, Path(raw): Path<String>) -> Response {
    let ancestors = match Uuid::try_parse(&raw) {
        Ok(id) => match sandbox_db::checkout::find(&ctx.state.db, id).await {
            Ok(Some(session)) if !session.embed_origins.is_empty() => {
                session.embed_origins.join(" ")
            }
            Ok(_) => "'none'".to_owned(),
            Err(error) => return ApiError::from(error).into_response(),
        },
        Err(_) => "'none'".to_owned(),
    };
    page_response(&ctx.site, &ancestors)
}

fn page_response(site: &Site, ancestors: &str) -> Response {
    let mut response = (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        site.index(),
    )
        .into_response();
    let headers = response.headers_mut();
    let policy = super::page_policy(site.script_hashes(), site.style_hashes(), ancestors);
    match HeaderValue::from_str(&policy) {
        Ok(value) => {
            headers.insert(header::CONTENT_SECURITY_POLICY, value);
        }
        Err(_) => {
            return ApiError::Internal(anyhow::anyhow!("page policy is not a header value"))
                .into_response();
        }
    }
    if ancestors == "'none'" {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    response
}

/// Assets by exact path; any other page path is the app (which says there is no session).
async fn asset(State(ctx): State<Checkout>, method: Method, uri: Uri) -> Response {
    serve_asset(&ctx.site, &method, &uri, "'none'")
}

/// Serves a site's file at `uri`, or its page for a route without a file extension.
pub fn serve_asset(site: &Site, method: &Method, uri: &Uri, ancestors: &str) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return ApiError::RouteNotFound.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    // No API answers here but the site's own; `/v1` lives on the API's origin only.
    if ["api", "v1"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        return ApiError::RouteNotFound.into_response();
    }
    if path != "index.html"
        && let Some(asset) = site.get(path)
    {
        let cache = if asset.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (header::CACHE_CONTROL, cache),
            ],
            asset.body.clone(),
        )
            .into_response();
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        return (StatusCode::NOT_FOUND, ApiError::RouteNotFound).into_response();
    }
    page_response(site, ancestors)
}
