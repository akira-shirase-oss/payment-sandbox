//! The connection pool: bounded, with a deadline on acquiring a connection and on every
//! statement and lock wait, so a slow or stuck database becomes a fast `503`, never a pile
//! of stalled requests.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

use crate::DbError;

/// Pool limits and per-statement deadlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    /// Most open connections.
    pub max_connections: u32,
    /// Longest wait for a free connection.
    pub acquire_timeout: Duration,
    /// Longest any single statement may run (`statement_timeout`).
    pub statement_timeout: Duration,
    /// Longest any statement may wait for a row lock (`lock_timeout`).
    pub lock_timeout: Duration,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            max_connections: 32,
            acquire_timeout: Duration::from_millis(1_000),
            statement_timeout: Duration::from_millis(2_000),
            lock_timeout: Duration::from_millis(1_000),
        }
    }
}

/// Parses `url` and applies the deadlines as session settings.
pub fn connect_options(url: &str, settings: &PoolSettings) -> Result<PgConnectOptions, DbError> {
    let options = PgConnectOptions::from_str(url).map_err(DbError::Other)?;
    Ok(options.application_name("payment-sandbox").options([
        (
            "statement_timeout",
            format!("{}ms", settings.statement_timeout.as_millis()),
        ),
        (
            "lock_timeout",
            format!("{}ms", settings.lock_timeout.as_millis()),
        ),
        // A connection left idle inside a transaction (a crashed handler) is cut off
        // instead of holding row locks forever.
        ("idle_in_transaction_session_timeout", "10000ms".to_owned()),
    ]))
}

/// Builds the pool without connecting, so the server starts (and reports not-ready) while
/// the database is down.
pub fn connect_lazy(url: &str, settings: &PoolSettings) -> Result<PgPool, DbError> {
    let options = connect_options(url, settings)?;
    Ok(PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .idle_timeout(Some(Duration::from_secs(300)))
        .connect_lazy_with(options))
}
