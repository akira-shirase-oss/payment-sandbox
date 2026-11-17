//! The dashboard's origin (D-061, D-062, D-063): the inspection app and its API under
//! `/api`.
//!
//! A person signs in with a project's sandbox key, exchanged for a host-only session
//! cookie (`__Host-ps_dashboard`) that names a session held in memory, which keeps only the
//! key's hash and re-resolves it on every request, so a revoked key ends its sessions at
//! once. Cookies are not isolated by port, so a sibling origin on the same host would send
//! this cookie too, and could plant one: every API request is refused when the browser
//! says another origin started it, every state change needs this exact `Origin` and, once
//! signed in, the session's `X-CSRF-Token`, and a doubled cookie is trusted by neither copy.
//!
//! The reads are the API's own handlers, mounted read-only under `/api/v1`: the session
//! hands them its identity ([`crate::auth::Delegated`]), and the request's own
//! `Authorization` is dropped, so a key cannot be used on this origin. The simulator's
//! controls exist only in the `dev` profile and only with the operator's own credential,
//! forwarded to the network and never kept (D-063).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self as axum_mw, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sandbox_domain::access::Profile;
use sandbox_domain::keys::{self, KeyHash, Scope};
use serde_json::{Value, json};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;

use super::site::Site;
use crate::auth::{Auth, Delegated};
use crate::error::ApiError;
use crate::extract::SandboxJson;
use crate::handlers;
use crate::state::AppState;

/// The session cookie (D-062): `__Host-` makes browsers refuse it unless `Secure`, host-only
/// and for `/`, so no other host can set or scope it.
pub const COOKIE: &str = "__Host-ps_dashboard";
/// The header a state change must carry the session's CSRF token in.
pub const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");
/// The header the operator's credential travels in, to be forwarded (D-063).
pub const OPERATOR_HEADER: HeaderName = HeaderName::from_static("x-operator-token");
/// Most sessions held at once.
pub const MAX_SESSIONS: usize = 4_096;
/// Most sessions one key holds; a further sign-in ends that key's oldest (review M3,
/// D-065), so no key can fill the table and lock others out.
pub const MAX_SESSIONS_PER_KEY: usize = 8;
/// A session unused this long ends.
pub const IDLE: Duration = Duration::from_mins(30);
/// A session ends this long after sign-in, used or not.
pub const LIFETIME: Duration = Duration::from_hours(12);

/// Dashboard sessions, in memory and bounded.
#[derive(Debug, Default)]
pub struct Sessions {
    map: Mutex<HashMap<[u8; 32], Entry>>,
}

#[derive(Debug, Clone)]
struct Entry {
    key: KeyHash,
    csrf: String,
    created: Instant,
    seen: Instant,
}

impl Entry {
    fn live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.seen) < IDLE
            && now.saturating_duration_since(self.created) < LIFETIME
    }
}

/// Every session slot holds a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

fn random_letters() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OS randomness unavailable: {e}")))?;
    Ok(sandbox_domain::letters::encode(&bytes))
}

impl Sessions {
    /// Starts a session for `key`; returns its cookie token and CSRF token.
    pub fn create(
        &self,
        key: KeyHash,
        now: Instant,
    ) -> Result<Result<(String, String), Full>, ApiError> {
        let token = random_letters()?;
        let csrf = random_letters()?;
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() >= MAX_SESSIONS {
            map.retain(|_, entry| entry.live(now));
        }
        // This key's oldest sessions make room for its new one.
        let mut own: Vec<([u8; 32], Instant)> = map
            .iter()
            .filter(|(_, entry)| entry.key == key)
            .map(|(digest, entry)| (*digest, entry.created))
            .collect();
        if own.len() >= MAX_SESSIONS_PER_KEY {
            own.sort_by_key(|(_, created)| *created);
            let surplus = own.len() + 1 - MAX_SESSIONS_PER_KEY;
            for (digest, _) in own.into_iter().take(surplus) {
                map.remove(&digest);
            }
        }
        if map.len() >= MAX_SESSIONS {
            return Ok(Err(Full));
        }
        map.insert(
            keys::digest(&token),
            Entry {
                key,
                csrf: csrf.clone(),
                created: now,
                seen: now,
            },
        );
        Ok(Ok((token, csrf)))
    }

    /// The key and CSRF token of a live session, which counts as a use.
    pub fn check(&self, token: &str, now: Instant) -> Option<(KeyHash, String)> {
        let digest = keys::digest(token);
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.get_mut(&digest)?;
        if !entry.live(now) {
            map.remove(&digest);
            return None;
        }
        entry.seen = now;
        Some((entry.key, entry.csrf.clone()))
    }

    /// Ends a session.
    pub fn remove(&self, token: &str) {
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(&keys::digest(token));
    }

    /// Sessions held (live or not yet swept).
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// True when none are held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The client to the network's control plane: no credential of its own (D-063).
#[derive(Debug, Clone)]
struct Control {
    http: reqwest::Client,
    base: String,
}

/// The dashboard site's state.
#[derive(Debug, Clone)]
pub struct Dashboard {
    state: AppState,
    site: Arc<Site>,
    origin: Arc<str>,
    sessions: Arc<Sessions>,
    control: Option<Control>,
}

/// A signed-in request's session (set by the session layer).
#[derive(Debug, Clone)]
struct Signed {
    token: String,
    csrf: String,
}

/// The dashboard origin's whole application.
pub fn router(state: &AppState, site: Arc<Site>, origin: &str, sessions: Arc<Sessions>) -> Router {
    let control = (state.config.profile == Profile::Dev)
        .then(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .connect_timeout(Duration::from_millis(500))
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
                .map(|http| Control {
                    http,
                    base: state.config.sim_url.clone(),
                })
        })
        .flatten();
    let ctx = Dashboard {
        state: state.clone(),
        site,
        origin: Arc::from(origin),
        sessions,
        control,
    };
    // The API's own read handlers, read-only, behind the session.
    let reads = Router::new()
        .route("/api/v1/project", get(handlers::projects::current))
        .route("/api/v1/payments", get(handlers::payments::list))
        .route(
            "/api/v1/payments/{payment_id}",
            get(handlers::payments::get),
        )
        .route(
            "/api/v1/payments/{payment_id}/journal",
            get(handlers::payments::journal),
        )
        .route("/api/v1/balances", get(handlers::payments::balances))
        .route("/api/v1/webhook_endpoints", get(handlers::webhooks::list))
        .route("/api/v1/events", get(handlers::webhooks::events))
        .route(
            "/api/v1/checkout_sessions/{id}",
            get(handlers::checkout::get),
        )
        .route("/api/status", get(status))
        .with_state(state.clone());
    // The one state change the dashboard makes to a project (D-066): the API's own reset,
    // with the session's identity (so `admin` is still required), this origin and the
    // session's CSRF token.
    let writes = Router::new()
        .route("/api/v1/project/reset", post(handlers::projects::reset))
        .with_state(state.clone());
    let reports = Router::new()
        .route(
            "/api/v1/reconciliation",
            get(handlers::payments::reconciliation),
        )
        .route(
            "/api/v1/balances/{currency}/{account}/explain",
            get(handlers::payments::explain),
        )
        .with_state(state.clone());
    let mut signed = Router::new()
        .route("/api/session/sign-out", post(sign_out))
        .route("/api/session/current", get(current));
    if ctx.control.is_some() {
        signed = signed
            .route("/api/sim/clock", get(sim_clock).post(sim_advance))
            .route("/api/sim/faults", post(sim_fault))
            .route("/api/sim/faults/clear", post(sim_clear));
    }
    let signed = signed.with_state(ctx.clone());
    let session_layer = axum_mw::from_fn_with_state(ctx.clone(), require_session);
    let gate = axum_mw::from_fn_with_state(state.gate.clone(), crate::middleware::shed_load);
    let behind_session = |router: Router| {
        router
            .route_layer(session_layer.clone())
            .route_layer(gate.clone())
    };
    let open = Router::new()
        .route("/api/session", post(sign_in))
        .fallback(asset)
        .with_state(ctx.clone());
    let hot = Router::new()
        .merge(behind_session(reads))
        .merge(behind_session(writes))
        .merge(behind_session(signed))
        .merge(open);
    let app =
        crate::middleware::apply(hot, &state.config).merge(crate::middleware::apply_with_timeout(
            behind_session(reports),
            &state.config,
            crate::routes::REPORT_TIMEOUT,
        ));
    app.layer(axum_mw::from_fn_with_state(ctx, guard))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::COOKIE,
            CSRF_HEADER,
            OPERATOR_HEADER,
        ]))
        .layer(axum_mw::map_response(harden))
}

async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    super::harden(headers);
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

/// Every request: the dashboard's own host name only (D-065) — a page on another is
/// redirected there, anything else is misdirected — so its cookie is only ever set for that
/// host. Every `/api` request: reads need fetch metadata saying same-origin or this exact
/// Origin, state changes this exact Origin.
async fn guard(State(ctx): State<Dashboard>, request: Request, next: Next) -> Response {
    let authority = ctx
        .origin
        .split_once("://")
        .map_or(&*ctx.origin, |(_, a)| a);
    let host_ok = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| host.eq_ignore_ascii_case(authority));
    let api = request.uri().path().starts_with("/api");
    let read = request.method() == Method::GET || request.method() == Method::HEAD;
    if !host_ok {
        let target = request
            .uri()
            .path_and_query()
            .map_or("/", axum::http::uri::PathAndQuery::as_str);
        return match HeaderValue::from_str(&format!("{}{target}", ctx.origin)) {
            Ok(location) if read && !api => (
                StatusCode::PERMANENT_REDIRECT,
                [(header::LOCATION, location)],
            )
                .into_response(),
            _ => ApiError::Misdirected.into_response(),
        };
    }
    if api {
        let checked = if read {
            super::require_same_origin_read(request.headers(), &ctx.origin)
        } else {
            super::require_same_origin(request.method(), request.headers(), &ctx.origin)
        };
        if let Err(error) = checked {
            return error.into_response();
        }
    }
    next.run(request).await
}

/// Resolves the session cookie to the key's identity (re-resolved every time), checks the
/// CSRF token of a state change, and hands the identity on; the request's own
/// `Authorization` is dropped.
async fn require_session(
    State(ctx): State<Dashboard>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = super::cookie(request.headers(), COOKIE).map(str::to_owned) else {
        return ApiError::Unauthorized.into_response();
    };
    let Some((key, csrf)) = ctx.sessions.check(&token, Instant::now()) else {
        return ApiError::Unauthorized.into_response();
    };
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let presented = request
            .headers()
            .get(&CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !keys::constant_time_eq(&keys::digest(presented), &keys::digest(&csrf)) {
            return ApiError::CrossOrigin.into_response();
        }
    }
    let identity = match sandbox_db::keys::resolve(&ctx.state.db, &key).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            // Revoked: the session ends with its key.
            ctx.sessions.remove(&token);
            return ApiError::Unauthorized.into_response();
        }
        Err(error) => return ApiError::from(error).into_response(),
    };
    request.headers_mut().remove(header::AUTHORIZATION);
    request.extensions_mut().insert(Delegated(identity));
    request.extensions_mut().insert(Signed { token, csrf });
    next.run(request).await
}

fn set_cookie(value: &str, max_age: Option<u64>) -> Result<HeaderValue, ApiError> {
    let mut cookie = format!("{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict");
    if let Some(seconds) = max_age {
        cookie = format!("{cookie}; Max-Age={seconds}");
    }
    HeaderValue::from_str(&cookie)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("session cookie is not a header value")))
}

async fn session_json(
    ctx: &Dashboard,
    identity: &sandbox_db::keys::KeyIdentity,
    csrf: &str,
) -> Result<Value, ApiError> {
    let project = sandbox_db::projects::get(&ctx.state.db, identity.scope.project_id).await?;
    Ok(json!({
        "project": {"id": project.id, "slug": project.slug, "name": project.name,
                    "dataset_epoch": project.dataset_epoch},
        "scopes": identity.scopes.iter().map(|s| s.code()).collect::<Vec<_>>(),
        "csrf_token": csrf,
        "profile": match ctx.state.config.profile {
            Profile::Dev => "dev",
            Profile::Ci => "ci",
            Profile::Shared => "shared",
        },
        "sim_controls": ctx.control.is_some(),
        "idle_timeout_seconds": IDLE.as_secs(),
        "simulated": true,
    }))
}

/// `POST /api/session` with `Authorization: Bearer sk_sim_…`: signs in. Any session the
/// browser already presented ends first (no fixation by a planted cookie).
async fn sign_in(State(ctx): State<Dashboard>, headers: HeaderMap) -> Result<Response, ApiError> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    let hash = keys::hash_presented(presented).map_err(|_| ApiError::Unauthorized)?;
    let identity = sandbox_db::keys::resolve(&ctx.state.db, &hash)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if let Some(old) = super::cookie(&headers, COOKIE) {
        ctx.sessions.remove(old);
    }
    let (token, csrf) = ctx
        .sessions
        .create(hash, Instant::now())?
        .map_err(|Full| ApiError::Overloaded)?;
    let body = session_json(&ctx, &identity, &csrf).await?;
    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&token, None)?);
    Ok(response)
}

/// `GET /api/session/current`: who is signed in, and the CSRF token for state changes.
async fn current(State(ctx): State<Dashboard>, request: Request) -> Result<Response, ApiError> {
    let (Some(Delegated(identity)), Some(signed)) = (
        request.extensions().get::<Delegated>(),
        request.extensions().get::<Signed>(),
    ) else {
        return Err(ApiError::Unauthorized);
    };
    Ok(Json(session_json(&ctx, identity, &signed.csrf).await?).into_response())
}

/// `POST /api/session/sign-out`: ends the session here and in the browser.
async fn sign_out(State(ctx): State<Dashboard>, request: Request) -> Result<Response, ApiError> {
    if let Some(signed) = request.extensions().get::<Signed>() {
        ctx.sessions.remove(&signed.token);
    }
    let mut response = Json(json!({"signed_out": true, "simulated": true})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie("", Some(0))?);
    Ok(response)
}

/// `GET /api/status`: the background tasks (reconciler, deliverer) as readiness sees them.
async fn status(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    Ok(Json(json!({
        "tasks": state.tasks.statuses(),
        "healthy": state.tasks.unhealthy().is_none(),
        "simulated": true,
    }))
    .into_response())
}

/// The operator's credential from `X-Operator-Token`, as it is to be forwarded.
fn operator(headers: &HeaderMap) -> Result<HeaderValue, ApiError> {
    let raw = headers
        .get(&OPERATOR_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty() && v.len() <= 256 && v.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or(ApiError::OperatorCredentialRequired)?;
    let mut value = HeaderValue::from_str(&format!("Bearer {raw}"))
        .map_err(|_| ApiError::OperatorCredentialRequired)?;
    value.set_sensitive(true);
    Ok(value)
}

/// Largest answer read from the control plane.
const MAX_CONTROL_REPLY: usize = 64 * 1024;

/// Forwards one control request; the network decides, with the operator's credential.
async fn forward(
    ctx: &Dashboard,
    method: reqwest::Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<&Value>,
) -> Result<Response, ApiError> {
    let control = ctx.control.as_ref().ok_or(ApiError::RouteNotFound)?;
    let mut request = control
        .http
        .request(method, format!("{}{path}", control.base))
        .header(reqwest::header::AUTHORIZATION, operator(headers)?);
    if let Some(body) = body {
        request = request.json(body);
    }
    let unreachable = |e: reqwest::Error| {
        ApiError::Unavailable(anyhow::anyhow!("the network's control plane: {e}"))
    };
    let mut response = request.send().await.map_err(unreachable)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_CONTROL_REPLY {
            return Err(ApiError::Unavailable(anyhow::anyhow!(
                "the network's control reply is too large"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match status.as_u16() {
        200..=299 => Ok(Json(json!({"network": answer, "simulated": true})).into_response()),
        401 => Err(ApiError::OperatorCredentialRequired),
        400..=499 => Err(ApiError::InvalidBody(format!(
            "the network refused the control ({})",
            answer["error"]["code"]
                .as_str()
                .filter(|c| c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                .unwrap_or("refused")
        ))),
        _ => Err(ApiError::Unavailable(anyhow::anyhow!(
            "the network's control plane answered {status}"
        ))),
    }
}

async fn sim_clock(State(ctx): State<Dashboard>, headers: HeaderMap) -> Result<Response, ApiError> {
    forward(&ctx, reqwest::Method::GET, "/v1/clock", &headers, None).await
}

/// `POST /api/sim/clock` body: seconds to move the network's virtual clock.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Advance {
    seconds: i64,
}

async fn sim_advance(
    State(ctx): State<Dashboard>,
    headers: HeaderMap,
    SandboxJson(body): SandboxJson<Advance>,
) -> Result<Response, ApiError> {
    forward(
        &ctx,
        reqwest::Method::POST,
        "/v1/clock",
        &headers,
        Some(&json!(body)),
    )
    .await
}

async fn sim_fault(
    State(ctx): State<Dashboard>,
    headers: HeaderMap,
    SandboxJson(rule): SandboxJson<Value>,
) -> Result<Response, ApiError> {
    if !rule.is_object() {
        return Err(ApiError::InvalidBody(
            "a fault rule is a JSON object".to_owned(),
        ));
    }
    forward(
        &ctx,
        reqwest::Method::POST,
        "/v1/faults",
        &headers,
        Some(&rule),
    )
    .await
}

async fn sim_clear(State(ctx): State<Dashboard>, headers: HeaderMap) -> Result<Response, ApiError> {
    forward(&ctx, reqwest::Method::DELETE, "/v1/faults", &headers, None).await
}

/// The app's files, and its page for every other route (`frame-ancestors 'none'`).
async fn asset(State(ctx): State<Dashboard>, method: Method, uri: Uri) -> Response {
    super::checkout::serve_asset(&ctx.site, &method, &uri, "'none'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_end_when_idle_when_old_and_when_removed() {
        let sessions = Sessions::default();
        let key = KeyHash([7; 32]);
        let t0 = Instant::now();
        let (token, csrf) = sessions.create(key, t0).unwrap().unwrap();
        assert!(!token.bytes().any(|b| b.is_ascii_digit()));
        assert_eq!(
            sessions.check(&token, t0 + Duration::from_secs(60)),
            Some((key, csrf.clone()))
        );
        // Used every 29 minutes it stays alive, until twelve hours after sign-in.
        let mut now = t0;
        while now.saturating_duration_since(t0) + Duration::from_mins(29) < LIFETIME {
            now += Duration::from_mins(29);
            assert!(sessions.check(&token, now).is_some());
        }
        assert!(sessions.check(&token, t0 + LIFETIME).is_none());
        let (idle, _) = sessions.create(key, t0).unwrap().unwrap();
        assert!(sessions.check(&idle, t0 + IDLE).is_none());
        let (gone, _) = sessions.create(key, t0).unwrap().unwrap();
        sessions.remove(&gone);
        assert!(sessions.check(&gone, t0).is_none());
        assert!(sessions.check("not-a-session", t0).is_none());
    }

    #[test]
    fn a_full_table_evicts_expired_sessions_and_refuses_when_all_are_live() {
        let sessions = Sessions::default();
        let t0 = Instant::now();
        // Many keys (one key alone cannot fill the table, review M3).
        let key = |i: usize| {
            let mut bytes = [0_u8; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            KeyHash(bytes)
        };
        for i in 0..MAX_SESSIONS {
            sessions
                .create(key(i / MAX_SESSIONS_PER_KEY), t0)
                .unwrap()
                .unwrap();
        }
        assert_eq!(sessions.create(key(usize::MAX), t0).unwrap(), Err(Full));
        // Once they are idle, a new sign-in sweeps them.
        assert!(sessions.create(key(usize::MAX), t0 + IDLE).unwrap().is_ok());
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn one_key_holds_a_bounded_number_of_sessions_and_its_oldest_go_first() {
        // Review M3: one key used to be able to fill the whole table and lock everyone out.
        let sessions = Sessions::default();
        let t0 = Instant::now();
        let mut tokens = Vec::new();
        for i in 0..MAX_SESSIONS_PER_KEY + 3 {
            let at = t0 + Duration::from_secs(u64::try_from(i).unwrap());
            tokens.push(sessions.create(KeyHash([9; 32]), at).unwrap().unwrap().0);
        }
        assert_eq!(sessions.len(), MAX_SESSIONS_PER_KEY);
        let now = t0 + Duration::from_secs(60);
        for (i, token) in tokens.iter().enumerate() {
            assert_eq!(sessions.check(token, now).is_some(), i >= 3, "session {i}");
        }
        // Another key still signs in.
        assert!(sessions.create(KeyHash([1; 32]), now).unwrap().is_ok());
    }

    #[test]
    fn the_cookie_is_host_only_secure_http_only_and_strict() {
        let cookie = set_cookie("abc", None).unwrap();
        assert_eq!(
            cookie.to_str().unwrap(),
            "__Host-ps_dashboard=abc; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
        assert!(
            set_cookie("", Some(0))
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("Max-Age=0")
        );
    }
}
