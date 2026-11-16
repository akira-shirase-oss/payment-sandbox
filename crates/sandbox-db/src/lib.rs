//! PostgreSQL storage for the Payment Sandbox. **SIMULATED MONEY ONLY.**
//!
//! PostgreSQL is the only authority (ARCHITECTURE "Sources of truth"). Every query here is
//! scoped by a [`ProjectScope`], every table is keyed by `(project_id, …)` (D-013), and the
//! journal's rules are enforced by the schema itself (`migrations/0001_spine.sql`).
//!
//! Queries use runtime-checked `sqlx::query` with explicit binds, so building needs no
//! database; the integration tests in `tests/` prove them against a real PostgreSQL.

pub mod error;
pub mod journal;
pub mod keys;
pub mod pool;
pub mod projects;
#[cfg(feature = "test-support")]
pub mod testing;

pub use error::DbError;
pub use pool::{PoolSettings, connect_lazy};

use uuid::Uuid;

/// The embedded migrations, applied in order at startup and by the tests.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// The project (and its current dataset epoch) a query runs for. Repositories take one of
/// these instead of a bare id, so an unscoped query does not type-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectScope {
    /// The project.
    pub project_id: Uuid,
    /// Its current dataset epoch (D-022).
    pub epoch: i32,
}
