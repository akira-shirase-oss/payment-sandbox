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
    /// Per-pool saturation metrics (D-101): sampled occupancy plus acquire waits and
    /// timeouts recorded by [`crate::poolstat::timed_acquire`].
    pub pool_stats: Arc<crate::poolstat::PoolStats>,
    /// Bounded key-identity cache (D-102): in-process revokes and resets invalidate
    /// exactly; a 30-second TTL is the out-of-band backstop.
    pub key_cache: Arc<crate::auth::KeyCache>,
    /// The `application_name` the `auth_epoch` listener connects under — unique per
    /// state, so tests and operators can name exactly this process's listener backend
    /// in `pg_stat_activity` instead of a cluster-wide query-text guess (N2).
    pub listen_name: String,
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
            config.cursor_ttl,
        ));
        // The request deadline is the longest a resolve can legitimately stay open —
        // the key cache's invalidation markers live at least that long (round-3 A).
        let key_cache = Arc::new(crate::auth::KeyCache::new(config.request_timeout));
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
            pool_stats: Arc::new(crate::poolstat::PoolStats::default()),
            key_cache,
            listen_name: format!("ps-listen-{}", uuid::Uuid::now_v7().simple()),
        }
    }

    /// The pool a request's database work must use — the same read/write split the load
    /// gate and `classify_pool` apply (D-100, D-101). `/v1` handlers draw
    /// `Extension<DbPool>` instead (the classification already ran); this is for
    /// request-scoped code outside the `/v1` routers — the sites' session checks — and
    /// for background tasks, which always name `write_db`.
    pub fn db_for(&self, method: &axum::http::Method) -> &PgPool {
        if crate::middleware::is_read_method(method) {
            &self.read_db
        } else {
            &self.write_db
        }
    }
}
