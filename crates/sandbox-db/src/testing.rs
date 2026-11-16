//! Test support: one throwaway, migrated database per test.
//!
//! `PS_TEST_DATABASE_URL` names a PostgreSQL role allowed to `CREATE DATABASE` (the local
//! stack's: `scripts/stack.sh up` prints it). Tests that need a database fail loudly
//! without it rather than pass silently.
#![expect(
    clippy::expect_used,
    reason = "test support: a setup failure must abort the test with its cause"
)]

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{AssertSqlSafe, ConnectOptions, Connection, Executor};
use uuid::Uuid;

/// The variable naming the admin connection.
pub const ENV: &str = "PS_TEST_DATABASE_URL";

/// A freshly created and migrated database, dropped by [`TestDb::cleanup`].
#[derive(Debug)]
pub struct TestDb {
    /// A pool on the new database.
    pub pool: PgPool,
    admin_url: String,
    name: String,
}

impl TestDb {
    /// Creates and migrates `ps_test_<uuid>`.
    ///
    /// # Panics
    ///
    /// When `PS_TEST_DATABASE_URL` is unset or the server is unreachable: a database test
    /// that cannot run must fail, not pass.
    pub async fn new() -> Self {
        let admin_url = std::env::var(ENV).unwrap_or_else(|_| {
            panic!("{ENV} is not set; start the stack with `scripts/stack.sh up` and export it")
        });
        let name = format!("ps_test_{}", Uuid::now_v7().simple());
        let mut admin = PgConnectOptions::from_str(&admin_url)
            .expect("invariant: PS_TEST_DATABASE_URL parses")
            .connect()
            .await
            .expect("connecting to PS_TEST_DATABASE_URL");
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .await
            .expect("creating the test database");
        admin.close().await.ok();
        let url = Self::url_for(&admin_url, &name);
        let options = crate::pool::connect_options(&url, &crate::PoolSettings::default())
            .expect("invariant: test url parses");
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await
            .expect("connecting to the test database");
        crate::MIGRATOR
            .run(&pool)
            .await
            .expect("migrating the test database");
        Self {
            pool,
            admin_url,
            name,
        }
    }

    /// The new database's URL (for child processes).
    pub fn url(&self) -> String {
        Self::url_for(&self.admin_url, &self.name)
    }

    /// The database name.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn url_for(admin_url: &str, name: &str) -> String {
        let (base, query) = match admin_url.split_once('?') {
            Some((base, query)) => (base, Some(query)),
            None => (admin_url, None),
        };
        let prefix = base.rsplit_once('/').map_or(base, |(prefix, _)| prefix);
        match query {
            Some(query) => format!("{prefix}/{name}?{query}"),
            None => format!("{prefix}/{name}"),
        }
    }

    /// Drops the database (forcing out any connection a test left behind).
    pub async fn cleanup(self) {
        // close() waits for every checked-out connection; a test that still holds one
        // must not hang the suite, and the FORCE drop below cuts it off anyway.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.pool.close()).await;
        if let Ok(mut admin) = PgConnectOptions::from_str(&self.admin_url)
            .expect("invariant: PS_TEST_DATABASE_URL parses")
            .connect()
            .await
        {
            let _ = admin
                .execute(AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    self.name
                )))
                .await;
            admin.close().await.ok();
        }
    }
}
