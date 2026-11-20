//! A whole sandbox for the CLI's tests, in this process but over real sockets: the simulated
//! network with its own store, the API server on a fresh database with its reconciler and
//! deliverer running, and the development store receiving signed webhooks. The CLI and the
//! runner talk to it over HTTP exactly as they talk to a real stack. SIMULATED MONEY ONLY.
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

use sandbox_db::testing::TestDb;
use sandbox_dev_store::{DevStore, StoreConfig};
use serde_json::{Value, json};
use tokio::sync::watch;

pub const BOOTSTRAP: &str = "cli-test-bootstrap-token-0123456789abcdef";
const SIM_TOKEN: &str = "cli-test-network-credential-0123456789abcdef";
const SECRET_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// A running sandbox.
pub struct Stack {
    pub api: String,
    pub store_url: String,
    pub store: DevStore,
    pub db: Option<TestDb>,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    files: Vec<PathBuf>,
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ps-cli-{name}-{}.redb",
        uuid::Uuid::now_v7().simple()
    ))
}

impl Stack {
    pub async fn start() -> Self {
        let (stop, stopping) = watch::channel(false);
        let mut tasks = Vec::new();
        let until = |mut rx: watch::Receiver<bool>| async move {
            let _ = rx.wait_for(|s| *s).await;
        };

        // The simulated network: its own store, its own port, lost replies held 4 s: longer
        // than the server's 2 s network deadline below, so a lost reply is always lost.
        let sim_path = temp("sim");
        let store = sandbox_sim::store::Store::open(&sim_path).await.unwrap();
        let sim = sandbox_sim::SimState {
            store,
            faults: Arc::new(sandbox_sim::faults::Faults::default()),
            lost_reply_hold: Duration::from_secs(4),
            access: Arc::new(sandbox_sim::Access {
                network: sandbox_domain::access::Credential::new(SIM_TOKEN).unwrap(),
                control: None,
            }),
            transport: sandbox_sim::SIM_LIMITS,
            http: Arc::new(sandbox_http::ServeStats::default()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sim_url = format!("http://{}", listener.local_addr().unwrap());
        let rx = stopping.clone();
        tasks.push(tokio::spawn(async move {
            let _ = sandbox_sim::serve(listener, sim, Duration::from_millis(200), until(rx)).await;
        }));

        // The API server on a fresh database, with the reconciler and the deliverer.
        let db = TestDb::new().await;
        let vars: HashMap<&str, String> = HashMap::from([
            ("PS_DATABASE_URL", db.url()),
            ("PS_BOOTSTRAP_TOKEN", BOOTSTRAP.to_owned()),
            ("PS_SIM_TOKEN", SIM_TOKEN.to_owned()),
            ("PS_SIM_URL", sim_url),
            ("PS_PROFILE", "dev".to_owned()),
            ("PS_SECRET_KEY", SECRET_KEY.to_owned()),
            // A network call that answers must never look lost: on a busy CI runner the
            // network's durable write took over 300 ms once (an approval came back
            // `uncertain` and the scenario trace differed), so the deadline is 2 s.
            ("PS_SIM_TIMEOUT_MS", "2000".to_owned()),
            // The grace stays at its default (the network deadline plus 1 s), so the reconciler
            // never resolves an operation whose own request is still waiting: a scenario's
            // uncertain answer is then the same on every run.
            ("PS_RECONCILE_INTERVAL_MS", "150".to_owned()),
            ("PS_DELIVERY_POLL_MS", "50".to_owned()),
            ("PS_DB_ACQUIRE_TIMEOUT_MS", "15000".to_owned()),
            ("PS_DB_STATEMENT_TIMEOUT_MS", "10000".to_owned()),
            ("PS_DB_LOCK_TIMEOUT_MS", "10000".to_owned()),
            ("PS_REQUEST_TIMEOUT_MS", "60000".to_owned()),
            ("PS_PAYMENT_QUEUE_WAIT_MS", "30000".to_owned()),
        ]);
        let config = sandbox_server::Config::from_lookup(|name| vars.get(name).cloned()).unwrap();
        let pool = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool).unwrap();
        let read =
            sandbox_db::connect_lazy(config.database_url.expose(), &config.read_pool()).unwrap();
        let state = sandbox_server::AppState::new(config, pool, read);
        let router = sandbox_server::app(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let rx = stopping.clone();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(until(rx))
                .await;
        }));
        let (reconciler_state, rx) = (state.clone(), stopping.clone());
        tasks.push(tokio::spawn(sandbox_server::reconciler::run(
            reconciler_state,
            rx,
        )));
        let deliverer = sandbox_server::deliverer::Deliverer::new(
            &state,
            Arc::new(sandbox_server::deliverer::SystemClock),
        )
        .unwrap();
        tasks.push(tokio::spawn(deliverer.run(stopping.clone())));

        // The development store.
        let store_path = temp("store");
        let dev_store = DevStore::open(&store_path, StoreConfig::default()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let store_url = format!("http://{}", listener.local_addr().unwrap());
        let (served, rx) = (dev_store.clone(), stopping.clone());
        tasks.push(tokio::spawn(async move {
            let _ = sandbox_dev_store::serve(listener, served, until(rx)).await;
        }));

        Self {
            api,
            store_url,
            store: dev_store,
            db: Some(db),
            stop,
            tasks,
            files: vec![sim_path, store_path],
        }
    }

    /// A project; returns (slug, admin key, read key). The store is configured to consume
    /// its webhooks.
    pub async fn project(&self, slug: &str) -> (String, String, String) {
        let http = reqwest::Client::new();
        let created: Value = http
            .post(format!("{}/v1/projects", self.api))
            .bearer_auth(BOOTSTRAP)
            .json(&json!({"slug": slug, "name": slug}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let admin = created["api_key"]["secret"].as_str().unwrap().to_owned();
        let reader: Value = http
            .post(format!("{}/v1/keys", self.api))
            .bearer_auth(&admin)
            .json(&json!({"label": "store", "scopes": ["read"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let read = reader["secret"].as_str().unwrap().to_owned();
        let endpoint: Value = http
            .post(format!("{}/v1/webhook_endpoints", self.api))
            .bearer_auth(&admin)
            .json(&json!({"url": format!("{}/webhooks", self.store_url)}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        self.store
            .configure(StoreConfig {
                sandbox_url: self.api.clone(),
                api_key: read.clone(),
                secrets: vec![endpoint["secret"].as_str().unwrap().to_owned()],
            })
            .await;
        (slug.to_owned(), admin, read)
    }

    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        for task in self.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
        if let Some(db) = self.db.take() {
            db.cleanup().await;
        }
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
    }
}
