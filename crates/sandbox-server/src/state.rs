//! Shared state: cheap to clone, built once at startup, no globals.

use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;
use crate::locks::PaymentLocks;
use crate::network::Network;
use crate::reports::ReportGate;
use crate::secrets::SecretBox;
use crate::supervisor::Tasks;

/// Everything a handler may need.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The only authority — the write pool: mutations, background tasks, anything that
    /// can hold a lock. Reads never draw from it (D-100).
    pub write_db: PgPool,
    /// The read pool: `GET`/`HEAD` work only. Its connections and acquire deadline are
    /// separate, so a write flood can never starve a read (D-100).
    pub read_db: PgPool,
    /// Validated configuration.
    pub config: Arc<Config>,
    /// Load-shedding gate for `/v1` (D-099: writes and reads admit separately).
    pub gate: Arc<crate::middleware::LoadGate>,
    /// The simulated network.
    pub network: Network,
    /// Per-payment queues ahead of the database (D-033).
    pub payment_locks: Arc<PaymentLocks>,
    /// Webhook secret encryption; `None` disables webhooks.
    pub secrets: Option<Arc<SecretBox>>,
    /// Report budgets and concurrency (D-047).
    pub reports: Arc<ReportGate>,
    /// Supervised background tasks, for readiness (D-051).
    pub tasks: Arc<Tasks>,
    /// Seals explanation cursors (D-056).
    pub cursors: Arc<crate::cursor::CursorKey>,
}

impl AppState {
    /// Builds the state around an existing pool.
    ///
    /// # Panics
    ///
    /// Only if the HTTP client cannot be built, which needs a broken TLS-less reqwest build
    /// (the network credential was already validated by the configuration).
    pub fn new(config: Config, write_db: PgPool, read_db: PgPool) -> Self {
        let gate = Arc::new(crate::middleware::LoadGate::new(
            config.max_in_flight,
            config.read_edge.in_flight,
            config.read_edge.queue_wait,
        ));
        #[expect(
            clippy::expect_used,
            reason = "invariant: a plain-HTTP reqwest client always builds, and the credential was validated as visible ASCII by the config"
        )]
        let network = Network::new(
            &config.sim_url,
            config.sim_token.expose(),
            config.sim_timeout,
        )
        .expect("invariant: HTTP client builds");
        let secrets = config
            .secret_key
            .as_ref()
            .and_then(|key| SecretBox::from_hex(key.expose()).ok())
            .map(Arc::new);
        let reports = Arc::new(ReportGate::new(
            config.reports.burst,
            config.reports.refill_per_minute,
            config.reports.per_project,
            config.reports.concurrency,
        ));
        let cursors = Arc::new(crate::cursor::CursorKey::new(
            config
                .secret_key
                .as_ref()
                .map(crate::config::Secret::expose),
        ));
        Self {
            cursors,
            tasks: Arc::new(Tasks::default()),
            reports,
            secrets,
            write_db,
            read_db,
            config: Arc::new(config),
            gate,
            network,
            payment_locks: Arc::new(PaymentLocks::default()),
        }
    }

    /// The pool a request's database work must use — the same read/write split the load
    /// gate applies to admission (D-100). Anything request-scoped goes through here; a
    /// handler that knows it writes may take `write_db` directly.
    pub fn db_for(&self, method: &axum::http::Method) -> &PgPool {
        if crate::middleware::is_read_method(method) {
            &self.read_db
        } else {
            &self.write_db
        }
    }
}
