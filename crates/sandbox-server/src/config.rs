//! Typed configuration, read once from the environment and validated with every problem
//! reported together.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use sandbox_db::PoolSettings;

/// A secret string whose `Debug` never prints the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line.
    Json,
    /// Human-readable.
    Pretty,
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Listen address; loopback only (D-023).
    pub listen: SocketAddr,
    /// PostgreSQL URL.
    pub database_url: Secret,
    /// Pool limits and statement deadlines.
    pub pool: PoolSettings,
    /// Deadline for producing response headers.
    pub request_timeout: Duration,
    /// Largest accepted request body.
    pub max_body_bytes: usize,
    /// Most requests in flight on `/v1` before shedding with `503`.
    pub max_in_flight: usize,
    /// Deadline of the readiness probe's database ping.
    pub readiness_timeout: Duration,
    /// Hard deadline for draining in-flight requests at shutdown.
    pub shutdown_grace: Duration,
    /// Apply migrations at startup.
    pub migrate_on_start: bool,
    /// Log format.
    pub log_format: LogFormat,
    /// `tracing` filter directives.
    pub log_filter: String,
    /// Operator token that may create projects (`POST /v1/projects`); unset disables it.
    pub bootstrap_token: Option<Secret>,
}

/// Every configuration problem found, reported together.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid configuration: {}", .0.join("; "))]
pub struct ConfigError(pub Vec<String>);

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads configuration through `lookup` (testable without touching the environment).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut errors = Vec::new();
        let mut parse = Parser {
            lookup: &lookup,
            errors: &mut errors,
        };
        let default_listen = SocketAddr::from(([127, 0, 0, 1], 56080));
        let listen = match lookup("PS_LISTEN") {
            None => default_listen,
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                parse
                    .errors
                    .push("PS_LISTEN is not a socket address".to_owned());
                default_listen
            }),
        };
        let database_url = parse.required("PS_DATABASE_URL");
        let pool = PoolSettings {
            max_connections: parse.value("PS_DB_MAX_CONNECTIONS", Some(32)),
            acquire_timeout: parse.millis("PS_DB_ACQUIRE_TIMEOUT_MS", 1_000),
            statement_timeout: parse.millis("PS_DB_STATEMENT_TIMEOUT_MS", 2_000),
            lock_timeout: parse.millis("PS_DB_LOCK_TIMEOUT_MS", 1_000),
        };
        let request_timeout = parse.millis("PS_REQUEST_TIMEOUT_MS", 5_000);
        let max_body_bytes: usize = parse.value("PS_MAX_BODY_BYTES", Some(16 * 1024));
        let max_in_flight: usize = parse.value("PS_MAX_IN_FLIGHT", Some(512));
        let readiness_timeout = parse.millis("PS_READINESS_TIMEOUT_MS", 500);
        let shutdown_grace = parse.millis("PS_SHUTDOWN_GRACE_MS", 10_000);
        let migrate_on_start: bool = parse.value("PS_MIGRATE", Some(true));
        let log_format = match lookup("PS_LOG_FORMAT").as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            Some(_) => {
                errors.push("PS_LOG_FORMAT must be json or pretty".to_owned());
                LogFormat::Json
            }
        };
        let log_filter = lookup("PS_LOG").unwrap_or_else(|| "info".to_owned());
        let bootstrap_token = lookup("PS_BOOTSTRAP_TOKEN");
        if let Some(token) = &bootstrap_token
            && (token.len() < 32 || token.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            errors.push("PS_BOOTSTRAP_TOKEN must be at least 32 visible characters".to_owned());
        }

        if !listen.ip().is_loopback() {
            errors.push(
                "PS_LISTEN must be a loopback address: this sandbox runs in the local/CI mode only (D-023)"
                    .to_owned(),
            );
        }
        if pool.max_connections == 0 {
            errors.push("PS_DB_MAX_CONNECTIONS must be at least 1".to_owned());
        }
        if !(1_024..=1_048_576).contains(&max_body_bytes) {
            errors.push("PS_MAX_BODY_BYTES must be between 1024 and 1048576".to_owned());
        }
        if max_in_flight == 0 {
            errors.push("PS_MAX_IN_FLIGHT must be at least 1".to_owned());
        }
        if request_timeout <= pool.statement_timeout {
            errors.push(
                "PS_REQUEST_TIMEOUT_MS must exceed PS_DB_STATEMENT_TIMEOUT_MS, so a slow statement fails before the request does"
                    .to_owned(),
            );
        }
        if !errors.is_empty() {
            return Err(ConfigError(errors));
        }
        Ok(Self {
            listen,
            database_url: Secret::new(database_url),
            pool,
            request_timeout,
            max_body_bytes,
            max_in_flight,
            readiness_timeout,
            shutdown_grace,
            migrate_on_start,
            log_format,
            log_filter,
            bootstrap_token: bootstrap_token.map(Secret::new),
        })
    }
}

struct Parser<'a, F: Fn(&str) -> Option<String>> {
    lookup: &'a F,
    errors: &'a mut Vec<String>,
}

impl<F: Fn(&str) -> Option<String>> Parser<'_, F> {
    fn required(&mut self, name: &str) -> String {
        (self.lookup)(name).unwrap_or_else(|| {
            self.errors.push(format!("{name} is required"));
            String::new()
        })
    }

    fn value<T: std::str::FromStr + Default>(&mut self, name: &str, default: Option<T>) -> T {
        match (self.lookup)(name) {
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                self.errors.push(format!("{name} is not valid"));
                T::default()
            }),
            None => default.unwrap_or_default(),
        }
    }

    fn millis(&mut self, name: &str, default: u64) -> Duration {
        let ms: u64 = self.value(name, Some(default));
        if ms == 0 {
            self.errors.push(format!("{name} must be at least 1"));
        }
        Duration::from_millis(ms)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_are_bounded_and_loopback() {
        let c = config(&[("PS_DATABASE_URL", "postgres://x@127.0.0.1/db")]).unwrap();
        assert_eq!(c.listen, "127.0.0.1:56080".parse().unwrap());
        assert_eq!(c.max_body_bytes, 16 * 1024);
        assert!(c.request_timeout > c.pool.statement_timeout);
        assert_eq!(format!("{:?}", c.database_url), "Secret(<redacted>)");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let err = config(&[
            ("PS_LISTEN", "0.0.0.0:56080"),
            ("PS_MAX_BODY_BYTES", "12"),
            ("PS_DB_MAX_CONNECTIONS", "many"),
            ("PS_REQUEST_TIMEOUT_MS", "100"),
        ])
        .unwrap_err();
        let text = err.to_string();
        for needle in [
            "PS_DATABASE_URL is required",
            "loopback",
            "PS_MAX_BODY_BYTES",
            "PS_DB_MAX_CONNECTIONS",
            "PS_REQUEST_TIMEOUT_MS must exceed",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
    }
}
