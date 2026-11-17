//! Shared helpers for the server's integration tests: an in-process app on a fresh
//! database, JSON requests, and project bootstrap.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: not every test uses every helper, and a panic is the failure report"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use sandbox_db::testing::TestDb;
use sandbox_server::{AppState, Config, app};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A simulated network on a random loopback port with its own redb file: the same HTTP
/// boundary and store the separate process uses (the process-kill drills run the server as
/// a real child process; see `tests/drills.rs`).
pub struct TestSim {
    pub url: String,
    pub store: sandbox_sim::store::Store,
    pub faults: Arc<sandbox_sim::faults::Faults>,
    pub path: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TestSim {
    pub async fn start() -> Self {
        let path =
            std::env::temp_dir().join(format!("ps-sim-{}.redb", uuid::Uuid::now_v7().simple()));
        Self::start_at(path, Duration::from_secs(3)).await
    }

    /// Starts (or restarts) a network on an existing store file, on a fresh port.
    pub async fn start_at(path: PathBuf, lost_reply_hold: Duration) -> Self {
        Self::start_on("127.0.0.1:0", path, lost_reply_hold).await
    }

    /// The address it listens on (for a restart on the same address).
    pub fn addr(&self) -> String {
        self.url.trim_start_matches("http://").to_owned()
    }

    /// Starts (or restarts) a network on `addr` with the store at `path`.
    pub async fn start_on(addr: &str, path: PathBuf, lost_reply_hold: Duration) -> Self {
        let store = sandbox_sim::store::Store::open(&path).await.unwrap();
        let faults = Arc::new(sandbox_sim::faults::Faults::default());
        let access = sandbox_sim::Access {
            network: sandbox_domain::access::Credential::new(SIM_TOKEN).unwrap(),
            control: Some(sandbox_domain::access::Credential::new(SIM_CONTROL_TOKEN).unwrap()),
        };
        let state = sandbox_sim::SimState {
            store: store.clone(),
            faults: faults.clone(),
            lost_reply_hold,
            access: Arc::new(access),
        };
        let listener = bind_retrying(addr).await;
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = sandbox_sim::serve(listener, state, Duration::from_millis(200), async move {
                let _ = rx.await;
            })
            .await;
        });
        Self {
            url,
            store,
            faults,
            path,
            stop: Some(tx),
            task: Some(task),
        }
    }

    pub fn fault(&self, rule: serde_json::Value) {
        let rule: sandbox_sim::faults::FaultRule = serde_json::from_value(rule).unwrap();
        self.faults.add(rule).unwrap();
    }

    /// Stops serving but keeps the store file (a process restart reopens it).
    pub async fn stop(mut self) -> PathBuf {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        self.store.close().await;
        self.path.clone()
    }

    /// Stops and deletes the store.
    pub async fn remove(self) {
        let path = self.stop().await;
        let _ = std::fs::remove_file(path);
    }
}

/// Binds `addr`, retrying briefly: a just-closed listener's port can take a moment to free.
async fn bind_retrying(addr: &str) -> tokio::net::TcpListener {
    for _ in 0..100 {
        if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
            return listener;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("could not bind {addr}");
}

/// The settings an app talking to `sim` uses, plus `extra`.
pub fn sim_vars<'a>(sim: &'a TestSim, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut vars = vec![
        ("PS_SIM_URL", sim.url.as_str()),
        ("PS_DB_ACQUIRE_TIMEOUT_MS", "15000"),
        ("PS_DB_STATEMENT_TIMEOUT_MS", "10000"),
        ("PS_DB_LOCK_TIMEOUT_MS", "10000"),
        ("PS_REQUEST_TIMEOUT_MS", "60000"),
        ("PS_PAYMENT_QUEUE_WAIT_MS", "30000"),
        ("PS_RECONCILE_GRACE_MS", "1"),
    ];
    vars.extend_from_slice(extra);
    vars
}

/// The operator token the test apps are configured with.
pub const BOOTSTRAP: &str = "test-bootstrap-token-0123456789abcdef";
/// The network credential the test networks require and the test apps present (D-046).
pub const SIM_TOKEN: &str = "test-network-credential-0123456789abcdef";
/// The test networks' control-plane credential (faults, the virtual clock).
pub const SIM_CONTROL_TOKEN: &str = "test-network-control-credential-0123456789";

/// An app on its own database.
pub struct TestApp {
    pub db: TestDb,
    pub router: Router,
    pub state: AppState,
}

impl TestApp {
    /// An app without a simulated network. Correctness tests get generous database
    /// deadlines: on a busy machine (load average 18–99 has been seen) the production 1 s
    /// pool-acquire deadline made them fail for timing, not behaviour (review 14).
    pub async fn new(extra: &[(&str, &str)]) -> Self {
        let mut vars = vec![
            ("PS_DB_ACQUIRE_TIMEOUT_MS", "15000"),
            ("PS_DB_STATEMENT_TIMEOUT_MS", "10000"),
            ("PS_DB_LOCK_TIMEOUT_MS", "10000"),
            ("PS_REQUEST_TIMEOUT_MS", "60000"),
        ];
        vars.extend_from_slice(extra);
        Self::on(TestDb::new().await, &vars)
    }

    /// An app talking to `sim`, with deadlines generous enough for a busy test machine:
    /// the races below test correctness, and a lock timeout would turn a would-be success
    /// into a 503 (the load runs measure the production deadlines).
    pub async fn with_sim(sim: &TestSim, extra: &[(&str, &str)]) -> Self {
        Self::on(TestDb::new().await, &sim_vars(sim, extra))
    }

    pub fn on(db: TestDb, extra: &[(&str, &str)]) -> Self {
        let mut vars: HashMap<String, String> = HashMap::from([
            ("PS_DATABASE_URL".to_owned(), db.url()),
            ("PS_BOOTSTRAP_TOKEN".to_owned(), BOOTSTRAP.to_owned()),
            ("PS_SIM_TOKEN".to_owned(), SIM_TOKEN.to_owned()),
        ]);
        for (k, v) in extra {
            vars.insert((*k).to_owned(), (*v).to_owned());
        }
        let config = Config::from_lookup(|name| vars.get(name).cloned()).unwrap();
        let pool = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool).unwrap();
        let state = AppState::new(config, pool);
        let router = app(&state);
        Self { db, router, state }
    }

    /// Sends a request; `bearer` becomes `Authorization: Bearer …`.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub async fn get(&self, path: &str, key: &str) -> (StatusCode, Value) {
        self.send(Method::GET, path, Some(key), None, None).await
    }

    pub async fn post(
        &self,
        path: &str,
        key: &str,
        idempotency_key: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        self.send(Method::POST, path, Some(key), idempotency_key, Some(body))
            .await
    }

    /// Creates a project; returns (project id, its admin key secret).
    pub async fn project(&self, slug: &str) -> (String, String) {
        let (status, body) = self
            .send(
                Method::POST,
                "/v1/projects",
                Some(BOOTSTRAP),
                None,
                Some(json!({"slug": slug, "name": slug})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["project"]["id"].as_str().unwrap().to_owned(),
            body["api_key"]["secret"].as_str().unwrap().to_owned(),
        )
    }

    /// Creates a key with `scopes` using `admin`; returns (key id, secret).
    pub async fn key(&self, admin: &str, scopes: &[&str]) -> (String, String) {
        let (status, body) = self
            .post(
                "/v1/keys",
                admin,
                None,
                json!({"label": "test key", "scopes": scopes}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["api_key"]["id"].as_str().unwrap().to_owned(),
            body["secret"].as_str().unwrap().to_owned(),
        )
    }

    /// Authorizes `amount` and returns the payment id.
    pub async fn authorize(
        &self,
        key: &str,
        idem: &str,
        amount: i64,
        currency: &str,
        token: &str,
    ) -> (StatusCode, Value) {
        self.post(
            "/v1/payments",
            key,
            Some(idem),
            json!({"amount": amount, "currency": currency, "token": token}),
        )
        .await
    }

    /// Runs the reconciliation report and asserts it is clean.
    pub async fn assert_reconciled(&self, key: &str) -> Value {
        let (status, report) = self.get("/v1/reconciliation", key).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(
            report["clean"], true,
            "reconciliation found a mismatch: {report}"
        );
        report
    }

    pub async fn cleanup(self) {
        self.db.cleanup().await;
    }
}
