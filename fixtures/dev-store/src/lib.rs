//! The development store (D-024): the reference consumer of the sandbox's webhooks, and the
//! "development store" of the ship gate. **SIMULATED MONEY ONLY.**
//!
//! For each delivery it:
//! 1. verifies the signature and the 300 s replay window (forged or stale → `401`);
//! 2. answers a duplicate event id from its inbox at once (deliveries are at-least-once);
//! 3. re-reads the payment from the sandbox API (thin events, D-025), so duplicated or
//!    reordered deliveries converge on current state;
//! 4. in one transaction of its own store (a redb file), records the event in its inbox and,
//!    if the payment has captured money, records the fulfillment under its own business key
//!    — the payment id — at most once.
//!
//! A failure to re-read the payment answers `503`, so the sandbox retries later. A payment
//! a dataset reset archived (`410`, D-066) is recorded in the inbox and fulfills nothing.
//!
//! `PUT /config` replaces the API key and the signing secrets the store trusts, so it is a
//! test control (D-046): mounted only when the store was given an operator credential
//! ([`DevStore::with_control`]), and refused without it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use sandbox_domain::access::Credential;
use sandbox_domain::webhook::{self, EVENT_ID_HEADER, EventPayload, SIGNATURE_HEADER};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use uuid::Uuid;

const INBOX: TableDefinition<'_, u128, &[u8]> = TableDefinition::new("inbox");
const FULFILLMENTS: TableDefinition<'_, u128, &[u8]> = TableDefinition::new("fulfillments");

/// What the store needs to talk to the sandbox (set at start or with `PUT /config`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// The sandbox API, e.g. `http://127.0.0.1:56080`.
    pub sandbox_url: String,
    /// A `read` key of the store's project.
    pub api_key: String,
    /// Signing secrets accepted (two during a rotation).
    pub secrets: Vec<String>,
}

/// A recorded fulfillment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fulfillment {
    /// The order's business key: the payment it was paid by.
    pub payment_id: Uuid,
    /// Captured minor units when fulfilled.
    pub captured: i64,
    /// The event that triggered it.
    pub event_id: Uuid,
}

#[derive(Debug, Default)]
struct Counters {
    duplicates: AtomicU64,
    rejected: AtomicU64,
}

/// The store's state.
#[derive(Clone)]
pub struct DevStore {
    db: Arc<Database>,
    config: Arc<RwLock<StoreConfig>>,
    http: reqwest::Client,
    counters: Arc<Counters>,
    control: Option<Arc<Credential>>,
    transport: sandbox_http::Limits,
}

/// The store listener's default transport limits (chosen, not measured; D-054).
pub const STORE_LIMITS: sandbox_http::Limits = sandbox_http::Limits::with_connections(256);
/// Deadline of one request to the store (its re-read of the payment is 2 s at most).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

impl std::fmt::Debug for DevStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DevStore")
    }
}

/// Store failures.
#[derive(Debug, thiserror::Error)]
#[error("dev store: {0}")]
pub struct StoreError(String);

fn err(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

impl DevStore {
    /// Opens (creating) the store's own database.
    pub fn open(path: &Path, config: StoreConfig) -> Result<Self, StoreError> {
        let db = Database::create(path).map_err(err)?;
        let txn = db.begin_write().map_err(err)?;
        txn.open_table(INBOX).map_err(err)?;
        txn.open_table(FULFILLMENTS).map_err(err)?;
        txn.commit().map_err(err)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .pool_idle_timeout(sandbox_http::CLIENT_POOL_IDLE)
            .no_proxy()
            .build()
            .map_err(err)?;
        Ok(Self {
            db: Arc::new(db),
            config: Arc::new(RwLock::new(config)),
            http,
            counters: Arc::new(Counters::default()),
            control: None,
            transport: STORE_LIMITS,
        })
    }

    /// Serves within `limits` instead of [`STORE_LIMITS`].
    #[must_use]
    pub const fn with_transport(mut self, limits: sandbox_http::Limits) -> Self {
        self.transport = limits;
        self
    }

    /// Mounts `PUT /config`, admitting only `credential` (D-046). Without it the route does
    /// not exist.
    #[must_use]
    pub fn with_control(mut self, credential: Option<Credential>) -> Self {
        self.control = credential.map(Arc::new);
        self
    }

    /// Every fulfillment.
    pub async fn fulfillments(&self) -> Result<Vec<Fulfillment>, StoreError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let txn = db.begin_read().map_err(err)?;
            let table = txn.open_table(FULFILLMENTS).map_err(err)?;
            let mut out = Vec::new();
            for entry in table.iter().map_err(err)? {
                let (_, value) = entry.map_err(err)?;
                out.push(serde_json::from_slice(value.value()).map_err(err)?);
            }
            Ok(out)
        })
        .await
        .map_err(err)?
    }

    /// At most `limit` fulfillments in payment-id order after `after` (review N11).
    pub async fn fulfillments_page(
        &self,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<Fulfillment>, StoreError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let txn = db.begin_read().map_err(err)?;
            let table = txn.open_table(FULFILLMENTS).map_err(err)?;
            let start = after.map_or(0, |a| a.as_u128().saturating_add(1));
            let mut out = Vec::with_capacity(limit);
            for entry in table.range(start..).map_err(err)? {
                if out.len() == limit {
                    break;
                }
                let (_, value) = entry.map_err(err)?;
                out.push(serde_json::from_slice(value.value()).map_err(err)?);
            }
            Ok(out)
        })
        .await
        .map_err(err)?
    }

    /// Number of fulfillments, without reading them.
    pub async fn fulfillments_len(&self) -> Result<u64, StoreError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let txn = db.begin_read().map_err(err)?;
            txn.open_table(FULFILLMENTS)
                .map_err(err)?
                .len()
                .map_err(err)
        })
        .await
        .map_err(err)?
    }

    /// Events in the inbox.
    pub async fn inbox_len(&self) -> Result<u64, StoreError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let txn = db.begin_read().map_err(err)?;
            txn.open_table(INBOX).map_err(err)?.len().map_err(err)
        })
        .await
        .map_err(err)?
    }

    /// Replaces the configuration.
    pub async fn configure(&self, config: StoreConfig) {
        *self.config.write().await = config;
    }
}

/// The store's HTTP API.
pub fn app(store: DevStore) -> Router {
    let mut router = Router::new()
        .route(
            "/healthz",
            get(|| async { Json(json!({"status": "ok", "simulated": true})) }),
        )
        .route("/webhooks", post(receive))
        .route("/fulfillments", get(list))
        .route("/stats", get(stats));
    if store.control.is_some() {
        router = router.route("/config", put(configure));
    }
    router
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .with_state(store)
}

/// Serves until `shutdown` within the store's transport limits (D-054), then drains for at
/// most 5 s.
pub async fn serve(
    listener: tokio::net::TcpListener,
    store: DevStore,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let limits = store.transport;
    sandbox_http::serve(
        listener,
        app(store),
        limits,
        Duration::from_secs(5),
        shutdown,
        std::sync::Arc::new(sandbox_http::ServeStats::default()),
    )
    .await
    .map(|_| ())
}

fn reply(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

async fn receive(State(store): State<DevStore>, headers: HeaderMap, body: Bytes) -> Response {
    let config = store.config.read().await.clone();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let event_id = header(EVENT_ID_HEADER).to_owned();
    let secrets: Vec<&str> = config.secrets.iter().map(String::as_str).collect();
    if let Err(e) = webhook::verify(&secrets, header(SIGNATURE_HEADER), &event_id, &body, now()) {
        store.counters.rejected.fetch_add(1, Ordering::Relaxed);
        return reply(StatusCode::UNAUTHORIZED, json!({"error": e.to_string()}));
    }
    let Ok(event) = serde_json::from_slice::<EventPayload>(&body) else {
        return reply(StatusCode::BAD_REQUEST, json!({"error": "not an event"}));
    };
    if event.id.to_string() != event_id || !event.simulated {
        return reply(
            StatusCode::BAD_REQUEST,
            json!({"error": "event id mismatch or not simulated"}),
        );
    }
    match seen(&store, event.id).await {
        Ok(true) => {
            store.counters.duplicates.fetch_add(1, Ordering::Relaxed);
            return reply(StatusCode::OK, json!({"duplicate": true}));
        }
        Ok(false) => {}
        Err(e) => {
            return reply(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": e.to_string()}),
            );
        }
    }
    // Thin event: the current state comes from the API, not from the delivery.
    let captured = match captured(&store, &config, event.payment_id).await {
        Ok(captured) => captured,
        Err(e) => {
            return reply(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": e.to_string()}),
            );
        }
    };
    let db = store.db.clone();
    let outcome = tokio::task::spawn_blocking(move || record(&db, &event, captured)).await;
    match outcome {
        Ok(Ok(Recorded::Duplicate)) => {
            store.counters.duplicates.fetch_add(1, Ordering::Relaxed);
            reply(StatusCode::OK, json!({"duplicate": true}))
        }
        Ok(Ok(Recorded::New { fulfilled })) => {
            reply(StatusCode::OK, json!({"fulfilled": fulfilled}))
        }
        Ok(Err(e)) => reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": e.to_string()}),
        ),
        Err(e) => reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": e.to_string()}),
        ),
    }
}

async fn seen(store: &DevStore, id: Uuid) -> Result<bool, StoreError> {
    let db = store.db.clone();
    tokio::task::spawn_blocking(move || {
        let txn = db.begin_read().map_err(err)?;
        Ok(txn
            .open_table(INBOX)
            .map_err(err)?
            .get(id.as_u128())
            .map_err(err)?
            .is_some())
    })
    .await
    .map_err(err)?
}

async fn captured(
    store: &DevStore,
    config: &StoreConfig,
    payment: Uuid,
) -> Result<i64, StoreError> {
    let response = store
        .http
        .get(format!("{}/v1/payments/{payment}", config.sandbox_url))
        .bearer_auth(&config.api_key)
        .send()
        .await
        .map_err(err)?;
    // A reset archived the payment's epoch (D-066): the event is real history, but the
    // store's orders start afresh with the new epoch, so it is recorded and fulfills nothing.
    if response.status() == reqwest::StatusCode::GONE {
        return Ok(0);
    }
    if !response.status().is_success() {
        return Err(StoreError(format!(
            "payment read answered {}",
            response.status()
        )));
    }
    let body: serde_json::Value = response.json().await.map_err(err)?;
    body["payment"]["captured"]
        .as_i64()
        .ok_or_else(|| StoreError("payment without captured".to_owned()))
}

enum Recorded {
    Duplicate,
    New { fulfilled: bool },
}

/// One transaction: inbox entry, and the fulfillment at most once per payment.
fn record(db: &Database, event: &EventPayload, captured: i64) -> Result<Recorded, StoreError> {
    let txn = db.begin_write().map_err(err)?;
    let fulfilled;
    {
        let mut inbox = txn.open_table(INBOX).map_err(err)?;
        if inbox.get(event.id.as_u128()).map_err(err)?.is_some() {
            return Ok(Recorded::Duplicate);
        }
        let entry = serde_json::to_vec(&json!({"type": event.kind, "payment_id": event.payment_id, "sequence": event.sequence}))
            .map_err(err)?;
        inbox
            .insert(event.id.as_u128(), entry.as_slice())
            .map_err(err)?;
        let mut fulfillments = txn.open_table(FULFILLMENTS).map_err(err)?;
        let key = event.payment_id.as_u128();
        fulfilled = captured > 0 && fulfillments.get(key).map_err(err)?.is_none();
        if fulfilled {
            let value = serde_json::to_vec(&Fulfillment {
                payment_id: event.payment_id,
                captured,
                event_id: event.id,
            })
            .map_err(err)?;
            fulfillments.insert(key, value.as_slice()).map_err(err)?;
        }
    }
    txn.commit().map_err(err)?;
    Ok(Recorded::New { fulfilled })
}

/// `GET /fulfillments` query: at most `limit` (1–1,000, default 1,000) after `after`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<Uuid>,
    limit: Option<usize>,
}

async fn list(State(store): State<DevStore>, Query(page): Query<Page>) -> Response {
    let limit = page.limit.unwrap_or(1_000).clamp(1, 1_000);
    match store.fulfillments_page(page.after, limit).await {
        Ok(f) => {
            let next_after = (f.len() == limit)
                .then(|| f.last().map(|x: &Fulfillment| x.payment_id))
                .flatten();
            reply(
                StatusCode::OK,
                json!({"fulfillments": f, "next_after": next_after, "simulated": true}),
            )
        }
        Err(e) => reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": e.to_string()}),
        ),
    }
}

async fn stats(State(store): State<DevStore>) -> Response {
    let fulfillments = store.fulfillments_len().await.unwrap_or(0);
    let inbox = store.inbox_len().await.unwrap_or(0);
    reply(
        StatusCode::OK,
        json!({
            "inbox": inbox,
            "fulfillments": fulfillments,
            "duplicates": store.counters.duplicates.load(Ordering::Relaxed),
            "rejected": store.counters.rejected.load(Ordering::Relaxed),
            "simulated": true,
        }),
    )
}

async fn configure(State(store): State<DevStore>, headers: HeaderMap, body: Bytes) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    // Checked before the body is parsed: nothing an unauthenticated caller sends is read.
    if !store.control.as_ref().is_some_and(|c| c.admits(presented)) {
        return reply(
            StatusCode::UNAUTHORIZED,
            json!({"error": "credential_required"}),
        );
    }
    let Ok(config) = serde_json::from_slice::<StoreConfig>(&body) else {
        return reply(
            StatusCode::BAD_REQUEST,
            json!({"error": "not a store config"}),
        );
    };
    store.configure(config).await;
    reply(StatusCode::OK, json!({"configured": true}))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests: a panic is the failure report")]

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;

    async fn get(store: &DevStore, path: &str) -> serde_json::Value {
        let response = app(store.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Review N11: `/fulfillments` read the whole table into one answer (about 100 MB
    /// after a million-delivery run) and `/stats` read it all to count it. It now pages.
    #[tokio::test]
    async fn fulfillments_are_listed_in_pages() {
        let path =
            std::env::temp_dir().join(format!("ps-store-page-{}.redb", Uuid::now_v7().simple()));
        let store = DevStore::open(&path, StoreConfig::default()).unwrap();
        for n in 0..5 {
            let event = EventPayload {
                id: Uuid::now_v7(),
                kind: "capture.succeeded".to_owned(),
                project_id: Uuid::from_u128(1),
                payment_id: Uuid::from_u128(100 + n),
                operation_id: Uuid::now_v7(),
                sequence: 1,
                created_at: 0,
                simulated: true,
            };
            record(&store.db, &event, 10).unwrap();
        }
        let first = get(&store, "/fulfillments?limit=2").await;
        assert_eq!(
            first["fulfillments"].as_array().unwrap().len(),
            2,
            "{first}"
        );
        let after = first["next_after"].as_str().unwrap().to_owned();
        let second = get(&store, &format!("/fulfillments?limit=2&after={after}")).await;
        let third = get(
            &store,
            &format!(
                "/fulfillments?limit=2&after={}",
                second["next_after"].as_str().unwrap()
            ),
        )
        .await;
        assert_eq!(
            third["fulfillments"].as_array().unwrap().len(),
            1,
            "{third}"
        );
        assert!(third["next_after"].is_null(), "{third}");
        assert_eq!(get(&store, "/stats").await["fulfillments"], 5);
        let _ = std::fs::remove_file(path);
    }
}
