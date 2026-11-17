//! Test support: one throwaway, migrated database per test.
//!
//! `PS_TEST_DATABASE_URL` names an administrator able to create roles and databases (the
//! local stack's superuser: `scripts/stack.sh env` prints it). Each test database is
//! created owned by `ps_owner` and migrated as it; [`TestDb::pool`] connects as the owner
//! (for inspection and tamper attempts), and [`TestDb::url`] is the runtime role `ps_app`
//! that servers connect as (D-038). Tests that need a database fail loudly without it.
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

/// A freshly created and migrated database, dropped by [`TestDb::cleanup`], or when the
/// handle is dropped without it (a failing test, review N12).
#[derive(Debug)]
pub struct TestDb {
    /// A pool on the new database.
    pub pool: PgPool,
    admin_url: String,
    name: String,
    cleaned: bool,
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        // Possibly inside an async runtime, possibly while a panic unwinds: drop the
        // database from a thread of its own, on a runtime of its own.
        let (admin_url, name) = (self.admin_url.clone(), self.name.clone());
        let _ = std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(drop_database(&admin_url, &name));
            }
        })
        .join();
    }
}

async fn drop_database(admin_url: &str, name: &str) {
    if let Ok(options) = PgConnectOptions::from_str(admin_url)
        && let Ok(mut admin) = options.connect().await
    {
        let _ = admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
            )))
            .await;
        admin.close().await.ok();
    }
}

/// Milliseconds since the Unix epoch in the UUIDv7 that `name` carries after `prefix`.
fn v7_millis(name: &str, prefix: &str) -> Option<u64> {
    let hex = name.strip_prefix(prefix)?.get(..12)?;
    u64::from_str_radix(hex, 16).ok()
}

/// Drops what earlier test processes leaked (a killed process runs no destructor): test
/// databases older than two hours, shadow roles older than one (review N12). Once per
/// process, before its first test database.
async fn sweep_stale(admin: &mut sqlx::PgConnection) {
    static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let older = |born: Option<u64>, hours: u64| {
        born.is_some_and(|b| now.saturating_sub(b) > hours * 3_600_000)
    };
    let databases: Vec<String> = sqlx::query_scalar(
        "SELECT datname::text FROM pg_database WHERE datname ~ '^ps_test_[0-9a-f]{32}'",
    )
    .fetch_all(&mut *admin)
    .await
    .unwrap_or_default();
    for name in databases {
        if older(v7_millis(&name, "ps_test_"), 2) {
            let _ = admin
                .execute(AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
                )))
                .await;
        }
    }
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles WHERE rolname ~ '^ps_shadow_[0-9a-f]{32}$'",
    )
    .fetch_all(&mut *admin)
    .await
    .unwrap_or_default();
    for name in roles {
        if older(v7_millis(&name, "ps_shadow_"), 1) {
            let _ = admin
                .execute(AssertSqlSafe(format!("DROP ROLE IF EXISTS {name}")))
                .await;
        }
    }
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
        crate::roles::bootstrap(&mut admin)
            .await
            .expect("creating the database roles");
        sweep_stale(&mut admin).await;
        crate::roles::create_database(&mut admin, &name)
            .await
            .expect("creating the test database");
        admin.close().await.ok();
        let url = Self::url_as(
            &admin_url,
            &name,
            crate::roles::OWNER,
            crate::roles::OWNER_PASSWORD,
        );
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
            cleaned: false,
        }
    }

    /// The runtime role's URL: what a server under test connects as.
    pub fn url(&self) -> String {
        Self::url_as(
            &self.admin_url,
            &self.name,
            crate::roles::APP,
            crate::roles::APP_PASSWORD,
        )
    }

    /// The owner's URL (migrations, inspection).
    pub fn owner_url(&self) -> String {
        Self::url_as(
            &self.admin_url,
            &self.name,
            crate::roles::OWNER,
            crate::roles::OWNER_PASSWORD,
        )
    }

    /// The administrator's URL for this database (superuser).
    pub fn admin_url(&self) -> String {
        Self::url_for(&self.admin_url, &self.name)
    }

    fn url_as(admin_url: &str, name: &str, user: &str, password: &str) -> String {
        let base = Self::url_for(admin_url, name);
        let Some((scheme, rest)) = base.split_once("://") else {
            return base;
        };
        let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
        format!("{scheme}://{user}:{password}@{host}")
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

    /// A backup: a new database copied from this one (`CREATE DATABASE … TEMPLATE`), a
    /// consistent snapshot of every committed row. Every other pool on this database must
    /// be closed first (PostgreSQL copies a template only while nobody is connected); this
    /// one is closed and reopened around the copy.
    pub async fn snapshot(&mut self) -> Self {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.pool.close()).await;
        let name = format!("ps_test_{}", Uuid::now_v7().simple());
        let mut admin = PgConnectOptions::from_str(&self.admin_url)
            .expect("invariant: PS_TEST_DATABASE_URL parses")
            .connect()
            .await
            .expect("connecting to PS_TEST_DATABASE_URL");
        let mut copied = false;
        for _ in 0..50 {
            let result = admin
                .execute(AssertSqlSafe(format!(
                    "CREATE DATABASE {name} OWNER {} TEMPLATE {}",
                    crate::roles::OWNER,
                    self.name
                )))
                .await;
            if result.is_ok() {
                copied = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            copied,
            "could not copy {} (is a pool still open on it?)",
            self.name
        );
        admin
            .execute(AssertSqlSafe(format!(
                "REVOKE ALL ON DATABASE {name} FROM PUBLIC; GRANT CONNECT ON DATABASE {name} TO {};",
                crate::roles::RUNTIME
            )))
            .await
            .expect("closing the copy");
        admin.close().await.ok();
        self.pool = Self::pool_for(&self.owner_url()).await;
        let owner = Self::url_as(
            &self.admin_url,
            &name,
            crate::roles::OWNER,
            crate::roles::OWNER_PASSWORD,
        );
        let pool = Self::pool_for(&owner).await;
        Self {
            pool,
            admin_url: self.admin_url.clone(),
            name,
            cleaned: false,
        }
    }

    /// Adopts `name` on the server `admin_url` administers (another cluster, D-055), owned
    /// by `ps_owner`; [`TestDb::cleanup`] drops it there.
    pub async fn existing(admin_url: &str, name: &str) -> Self {
        let owner = Self::url_as(
            admin_url,
            name,
            crate::roles::OWNER,
            crate::roles::OWNER_PASSWORD,
        );
        Self {
            pool: Self::pool_for(&owner).await,
            admin_url: admin_url.to_owned(),
            name: name.to_owned(),
            cleaned: false,
        }
    }

    /// Adopts a database created next to this one by something else (a restore, D-053),
    /// owned by `ps_owner`; [`TestDb::cleanup`] drops it like any other.
    pub async fn attach(&self, name: &str) -> Self {
        let owner = Self::url_as(
            &self.admin_url,
            name,
            crate::roles::OWNER,
            crate::roles::OWNER_PASSWORD,
        );
        Self {
            pool: Self::pool_for(&owner).await,
            admin_url: self.admin_url.clone(),
            name: name.to_owned(),
            cleaned: false,
        }
    }

    async fn pool_for(url: &str) -> PgPool {
        let options = crate::pool::connect_options(url, &crate::PoolSettings::default())
            .expect("invariant: test url parses");
        PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await
            .expect("connecting to the test database")
    }

    /// Drops the database (forcing out any connection a test left behind).
    pub async fn cleanup(mut self) {
        // close() waits for every checked-out connection; a test that still holds one
        // must not hang the suite, and the FORCE drop below cuts it off anyway.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.pool.close()).await;
        drop_database(&self.admin_url, &self.name).await;
        self.cleaned = true;
    }
}
