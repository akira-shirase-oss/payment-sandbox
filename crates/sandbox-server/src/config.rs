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
    /// Log format.
    pub log_format: LogFormat,
    /// `tracing` filter directives.
    pub log_filter: String,
    /// Operator token that may create projects (`POST /v1/projects`); unset disables it.
    pub bootstrap_token: Option<Secret>,
    /// Base URL of the simulated network; loopback only (D-023).
    pub sim_url: String,
    /// The credential the network requires on every execution, readback and listing
    /// (`PS_SIM_TOKEN`, D-046).
    pub sim_token: Secret,
    /// Longest a request waits for its payment's turn before it is shed (D-033).
    pub payment_queue_wait: Duration,
    /// Time between reconciliation sweeps.
    pub reconcile_interval: Duration,
    /// Age an open operation must reach before the reconciler fences it.
    pub reconcile_grace: Duration,
    /// Most operations one sweep examines.
    pub reconcile_batch: i64,
    /// Most pages of 1,000 a network-agreement report reads before it reports itself
    /// incomplete.
    pub reconcile_network_max_pages: usize,
    /// Key that encrypts webhook signing secrets (64 hex); webhooks are off without it.
    pub secret_key: Option<Secret>,
    /// How often the deliverer looks for due deliveries when idle.
    pub delivery_poll: Duration,
    /// Deadline of one call to the simulated network. A call that misses it is `unknown`,
    /// and the operation stays `uncertain` until readback.
    pub sim_timeout: Duration,
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
        let sim_token = parse.required("PS_SIM_TOKEN");
        let pool = PoolSettings {
            max_connections: parse.value("PS_DB_MAX_CONNECTIONS", Some(32)),
            acquire_timeout: parse.millis("PS_DB_ACQUIRE_TIMEOUT_MS", 1_000),
            statement_timeout: parse.millis("PS_DB_STATEMENT_TIMEOUT_MS", 1_000),
            lock_timeout: parse.millis("PS_DB_LOCK_TIMEOUT_MS", 500),
        };
        let request_timeout = parse.millis("PS_REQUEST_TIMEOUT_MS", 5_000);
        let max_body_bytes: usize = parse.value("PS_MAX_BODY_BYTES", Some(16 * 1024));
        let max_in_flight: usize = parse.value("PS_MAX_IN_FLIGHT", Some(512));
        let readiness_timeout = parse.millis("PS_READINESS_TIMEOUT_MS", 500);
        let shutdown_grace = parse.millis("PS_SHUTDOWN_GRACE_MS", 10_000);
        let sim_timeout = parse.millis("PS_SIM_TIMEOUT_MS", 2_000);
        let payment_queue_wait = parse.millis("PS_PAYMENT_QUEUE_WAIT_MS", 1_000);
        let reconcile_interval = parse.millis("PS_RECONCILE_INTERVAL_MS", 5_000);
        let grace_default =
            u64::try_from(sim_timeout.as_millis().saturating_add(1_000)).unwrap_or(u64::MAX);
        let reconcile_grace = parse.millis("PS_RECONCILE_GRACE_MS", grace_default);
        let reconcile_batch: i64 = parse.value("PS_RECONCILE_BATCH", Some(200));
        let reconcile_network_max_pages: usize =
            parse.value("PS_RECONCILE_NETWORK_MAX_PAGES", Some(1_000));
        let delivery_poll = parse.millis("PS_DELIVERY_POLL_MS", 250);
        let secret_key = lookup("PS_SECRET_KEY");
        if let Some(key) = &secret_key
            && crate::secrets::SecretBox::from_hex(key).is_err()
        {
            errors.push("PS_SECRET_KEY must be 64 hex characters (32 bytes)".to_owned());
        }
        let log_format = match lookup("PS_LOG_FORMAT").as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            Some(_) => {
                errors.push("PS_LOG_FORMAT must be json or pretty".to_owned());
                LogFormat::Json
            }
        };
        let log_filter = lookup("PS_LOG").unwrap_or_else(|| "info".to_owned());
        let sim_url = lookup("PS_SIM_URL").unwrap_or_else(|| "http://127.0.0.1:56081".to_owned());
        if !is_loopback_http(&sim_url) {
            errors.push(
                "PS_SIM_URL must be http://127.0.0.1:<port> or http://localhost:<port>: the network is a local process (D-023)"
                    .to_owned(),
            );
        }
        if !sim_token.is_empty() && sandbox_domain::access::Credential::new(&sim_token).is_err() {
            errors.push("PS_SIM_TOKEN must be at least 32 visible characters".to_owned());
        }
        let bootstrap_token = lookup("PS_BOOTSTRAP_TOKEN");
        if let Some(token) = &bootstrap_token
            && (token.len() < 32 || token.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            errors.push("PS_BOOTSTRAP_TOKEN must be at least 32 visible characters".to_owned());
        }

        let config = Self {
            listen,
            database_url: Secret::new(database_url),
            pool,
            request_timeout,
            max_body_bytes,
            max_in_flight,
            readiness_timeout,
            shutdown_grace,
            log_format,
            log_filter,
            bootstrap_token: bootstrap_token.map(Secret::new),
            sim_url: sim_url.trim_end_matches('/').to_owned(),
            sim_token: Secret::new(sim_token),
            sim_timeout,
            payment_queue_wait,
            reconcile_interval,
            reconcile_grace,
            reconcile_batch: reconcile_batch.clamp(1, 10_000),
            reconcile_network_max_pages: reconcile_network_max_pages.clamp(1, 100_000),
            secret_key: secret_key.map(Secret::new),
            delivery_poll,
        };
        // Every problem at once: parse errors and cross-field rules together.
        errors.extend(config.problems());
        if errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError(errors))
        }
    }

    /// Cross-field rules: every deadline must leave room for the work it bounds.
    fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let (listen, pool, max_body_bytes, max_in_flight, request_timeout, sim_timeout) = (
            self.listen,
            self.pool,
            self.max_body_bytes,
            self.max_in_flight,
            self.request_timeout,
            self.sim_timeout,
        );
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
        if sim_timeout.saturating_add(pool.statement_timeout.saturating_mul(2)) >= request_timeout {
            errors.push(
                "PS_REQUEST_TIMEOUT_MS must exceed PS_SIM_TIMEOUT_MS plus two statement timeouts, so a request can record what the network said"
                    .to_owned(),
            );
        }
        errors
    }
}

/// True for `http://127.0.0.1:<port>`, `http://[::1]:<port>` or `http://localhost:<port>`.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host_port = rest.split('/').next().unwrap_or("");
    let Some((host, port)) = host_port.rsplit_once(':') else {
        return false;
    };
    port.parse::<u16>().is_ok() && matches!(host, "127.0.0.1" | "[::1]" | "localhost")
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

    const SIM_TOKEN: &str = "sim-credential-for-config-tests-0123456789";

    #[test]
    fn defaults_are_bounded_and_loopback() {
        let c = config(&[
            ("PS_DATABASE_URL", "postgres://x@127.0.0.1/db"),
            ("PS_SIM_TOKEN", SIM_TOKEN),
        ])
        .unwrap();
        assert_eq!(c.listen, "127.0.0.1:56080".parse().unwrap());
        assert_eq!(c.max_body_bytes, 16 * 1024);
        assert!(c.request_timeout > c.pool.statement_timeout);
        assert_eq!(format!("{:?}", c.database_url), "Secret(<redacted>)");
        assert_eq!(format!("{:?}", c.sim_token), "Secret(<redacted>)");
    }

    #[test]
    fn the_network_credential_is_required_and_strong() {
        let missing = config(&[("PS_DATABASE_URL", "x")]).unwrap_err();
        assert!(
            missing.to_string().contains("PS_SIM_TOKEN is required"),
            "{missing}"
        );
        let weak = config(&[("PS_DATABASE_URL", "x"), ("PS_SIM_TOKEN", "short")]).unwrap_err();
        assert!(weak.to_string().contains("PS_SIM_TOKEN must be"), "{weak}");
    }

    #[test]
    fn the_network_must_be_a_loopback_process() {
        for url in [
            "http://127.0.0.1:56081",
            "http://localhost:9",
            "http://[::1]:56081/",
        ] {
            assert!(is_loopback_http(url), "{url}");
        }
        for url in [
            "https://127.0.0.1:56081",
            "http://10.0.0.1:56081",
            "http://example.com:80",
            "http://127.0.0.1",
        ] {
            assert!(!is_loopback_http(url), "{url}");
        }
        let err = config(&[
            ("PS_DATABASE_URL", "x"),
            ("PS_SIM_URL", "http://api.example.com:443"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("PS_SIM_URL"), "{err}");
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
