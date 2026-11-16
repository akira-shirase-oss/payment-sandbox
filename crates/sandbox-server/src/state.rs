//! Shared state: cheap to clone, built once at startup, no globals.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::locks::PaymentLocks;
use crate::network::Network;

/// Everything a handler may need.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The only authority.
    pub db: PgPool,
    /// Validated configuration.
    pub config: Arc<Config>,
    /// Load-shedding gate for `/v1`.
    pub gate: Arc<Semaphore>,
    /// The simulated network.
    pub network: Network,
    /// Per-payment queues ahead of the database (D-033).
    pub payment_locks: Arc<PaymentLocks>,
}

impl AppState {
    /// Builds the state around an existing pool.
    ///
    /// # Panics
    ///
    /// Only if the HTTP client cannot be built, which needs a broken TLS-less reqwest build.
    pub fn new(config: Config, db: PgPool) -> Self {
        let gate = Arc::new(Semaphore::new(config.max_in_flight));
        #[expect(
            clippy::expect_used,
            reason = "invariant: a plain-HTTP reqwest client always builds"
        )]
        let network = Network::new(&config.sim_url, config.sim_timeout)
            .expect("invariant: HTTP client builds");
        Self {
            db,
            config: Arc::new(config),
            gate,
            network,
            payment_locks: Arc::new(PaymentLocks::default()),
        }
    }
}
