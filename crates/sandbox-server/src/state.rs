//! Shared state: cheap to clone, built once at startup, no globals.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::locks::PaymentLocks;
use crate::network::Network;
use crate::secrets::SecretBox;

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
    /// Webhook secret encryption; `None` disables webhooks.
    pub secrets: Option<Arc<SecretBox>>,
}

impl AppState {
    /// Builds the state around an existing pool.
    ///
    /// # Panics
    ///
    /// Only if the HTTP client cannot be built, which needs a broken TLS-less reqwest build
    /// (the network credential was already validated by the configuration).
    pub fn new(config: Config, db: PgPool) -> Self {
        let gate = Arc::new(Semaphore::new(config.max_in_flight));
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
        Self {
            secrets,
            db,
            config: Arc::new(config),
            gate,
            network,
            payment_locks: Arc::new(PaymentLocks::default()),
        }
    }
}
