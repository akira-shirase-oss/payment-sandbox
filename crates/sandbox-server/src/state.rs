//! Shared state: cheap to clone, built once at startup, no globals.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::config::Config;

/// Everything a handler may need.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The only authority.
    pub db: PgPool,
    /// Validated configuration.
    pub config: Arc<Config>,
    /// Load-shedding gate for `/v1`.
    pub gate: Arc<Semaphore>,
}

impl AppState {
    /// Builds the state around an existing pool.
    pub fn new(config: Config, db: PgPool) -> Self {
        let gate = Arc::new(Semaphore::new(config.max_in_flight));
        Self {
            db,
            config: Arc::new(config),
            gate,
        }
    }
}
