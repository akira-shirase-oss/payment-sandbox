//! Typed configuration, read once from the environment and validated with every problem
//! reported together.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use sandbox_domain::access::Profile;

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
    /// The read edge (D-099): `GET`/`HEAD` admit separately at both layers — their own
    /// in-flight permits and, when `listen` names one, their own listener too.
    pub read_edge: ReadEdge,
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
    /// Report budgets and concurrency (D-047).
    pub reports: ReportSettings,
    /// The loopback ports webhooks may target (review N10, D-059).
    pub webhook_ports: WebhookPorts,
    /// Connection cap and transport deadlines of the listener (D-054): `PS_MAX_CONNECTIONS`,
    /// `PS_READ_TIMEOUT_MS`, `PS_WRITE_TIMEOUT_MS`, `PS_MAX_REQUESTS_PER_CONNECTION`,
    /// `PS_MAX_CONNECTION_AGE_MS`.
    pub transport: sandbox_http::Limits,
    /// The reconciler's fairness, concurrency, backoff and alert (D-048).
    pub reconcile: ReconcileTuning,
    /// Key that encrypts webhook signing secrets (64 hex); webhooks are off without it.
    pub secret_key: Option<Secret>,
    /// How often the deliverer looks for due deliveries when idle.
    pub delivery_poll: Duration,
    /// Deadline of one call to the simulated network. A call that misses it is `unknown`,
    /// and the operation stays `uncertain` until readback.
    pub sim_timeout: Duration,
    /// Where the process runs (`PS_PROFILE`): decides control-plane defaults only (D-046);
    /// the dashboard's simulator controls exist only in `dev` (D-063).
    pub profile: Profile,
    /// The checkout's own listener (D-061), on when `PS_CHECKOUT_DIR` names a built checkout.
    pub checkout: Option<UiSite>,
    /// The dashboard's own listener, on when `PS_DASHBOARD_DIR` names a built dashboard.
    pub dashboard: Option<UiSite>,
    /// The checkout's public origin, which session URLs name (`PS_CHECKOUT_ORIGIN`, by
    /// default `http://` and the checkout's listen address).
    pub checkout_origin: String,
}

/// The read edge's own admission (D-099): reads never queue behind writes.
#[derive(Debug, Clone)]
pub struct ReadEdge {
    /// A second API listener for reads, off unless `PS_READ_LISTEN` names one. Its
    /// connections never wait behind write connections for a serving place.
    pub listen: Option<SocketAddr>,
    /// In-flight permits reserved for reads (`PS_READ_IN_FLIGHT`, D-099): a write flood
    /// can hold every write permit without a read ever waiting on one.
    pub in_flight: usize,
    /// Longest a read waits for its own permit before a fast named `503`.
    pub queue_wait: Duration,
    /// The edge's bounded wait for a serving place (`PS_READ_ACCEPT_WAIT_MS`).
    pub accept_wait: Duration,
    /// The read pool's slice of the connection budget (`PS_DB_READ_CONNECTIONS`, D-100):
    /// `PS_DB_MAX_CONNECTIONS` is the whole budget; writes get the rest.
    pub db_connections: u32,
    /// The read pool's own acquire deadline (`PS_DB_READ_ACQUIRE_TIMEOUT_MS`, D-100): a
    /// saturated read pool refuses on its own budget, never on the write pool's.
    pub db_acquire_timeout: Duration,
    /// The read pool's own statement deadline (`PS_DB_READ_STATEMENT_TIMEOUT_MS`, D-101):
    /// reads are sub-millisecond lookups, so a read statement that runs long is a scan
    /// defect and is cut off independently of the write deadline.
    pub db_statement_timeout: Duration,
}

/// A browser-facing site on its own listener, hence its own origin (D-029, D-061).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSite {
    /// Listen address (loopback, D-023).
    pub listen: SocketAddr,
    /// The built static app (`index.html` and its assets).
    pub dir: PathBuf,
    /// The origin browsers reach it at; state-changing requests must carry exactly this
    /// `Origin` (D-062).
    pub origin: String,
}

/// Report budgets and concurrency (D-047).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportSettings {
    /// Units in each project's budget (`PS_REPORT_BURST`).
    pub burst: u64,
    /// Units refilled per minute (`PS_REPORT_REFILL_PER_MINUTE`).
    pub refill_per_minute: u64,
    /// Most reports one project may run at once (`PS_REPORT_PER_PROJECT`).
    pub per_project: u32,
    /// Most reports the server runs at once (`PS_REPORT_CONCURRENCY`).
    pub concurrency: usize,
}

/// The reconciler's fairness, concurrency, backoff and alert (D-048).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileTuning {
    /// Readbacks in flight at once (`PS_RECONCILE_CONCURRENCY`).
    pub concurrency: usize,
    /// Most operations of one project per sweep (`PS_RECONCILE_PER_PROJECT`; default the
    /// batch). Projects are taken in turn either way.
    pub per_project: i64,
    /// Longest wait before re-checking an operation whose check failed
    /// (`PS_RECONCILE_BACKOFF_MAX_MS`); the first wait is one sweep interval.
    pub backoff_max: Duration,
    /// An open operation older than this is logged at WARN on every sweep
    /// (`PS_RECONCILE_ALERT_AGE_MS`).
    pub alert_age: Duration,
}

/// Which loopback ports webhooks may target (review N10, D-059): never the server's own,
/// the network's or PostgreSQL's; otherwise those `PS_WEBHOOK_PORTS` lists (`9,56100-56199`),
/// or any port when that is unset and `PS_PROFILE=dev` (the developer's opt-in); with
/// neither, none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookPorts {
    /// Always refused.
    pub denied: Vec<u16>,
    /// Inclusive ranges allowed; `None` allows every port not denied.
    pub allowed: Option<Vec<(u16, u16)>>,
}

impl WebhookPorts {
    /// True when a webhook may target `port`.
    pub fn permits(&self, port: u16) -> bool {
        !self.denied.contains(&port)
            && self
                .allowed
                .as_ref()
                .is_none_or(|ranges| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&port)))
    }
}

/// The port of a `scheme://[user[:password]@]host:port/...` URL, if it names one.
fn url_port(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = host_port.rsplit_once(':')?;
    (!host.ends_with('[') && !port.contains(']'))
        .then(|| port.parse().ok())
        .flatten()
}

/// The server listener's default transport limits (chosen, not measured; D-054).
pub const SERVER_LIMITS: sandbox_http::Limits = sandbox_http::Limits::with_connections(512);

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
        let listen = parse.listen();
        let database_url = parse.required("PS_DATABASE_URL");
        let sim_token = parse.required("PS_SIM_TOKEN");
        let read_edge = parse.read_edge();
        let db_budget: u32 = parse.value("PS_DB_MAX_CONNECTIONS", Some(32));
        let pool = PoolSettings {
            // The whole budget is `PS_DB_MAX_CONNECTIONS`; the write pool keeps what the
            // read edge leaves (D-100). Validation rejects a budget the read pool empties.
            max_connections: db_budget.saturating_sub(read_edge.db_connections),
            acquire_timeout: parse.millis("PS_DB_ACQUIRE_TIMEOUT_MS", 1_000),
            statement_timeout: parse.millis("PS_DB_STATEMENT_TIMEOUT_MS", 1_000),
            lock_timeout: parse.millis("PS_DB_LOCK_TIMEOUT_MS", 500),
            read_only: false,
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
        let reports = parse.reports();
        let transport = parse.transport();
        let sim_url = lookup("PS_SIM_URL").unwrap_or_else(|| "http://127.0.0.1:56081".to_owned());
        let sites = parse.sites(listen, read_edge.listen, &sim_url, &database_url);
        let reconcile = parse.reconcile_tuning(reconcile_batch);
        let secret_key = lookup("PS_SECRET_KEY");
        if let Some(key) = &secret_key
            && crate::secrets::SecretBox::from_hex(key).is_err()
        {
            errors.push("PS_SECRET_KEY must be 64 hex characters (32 bytes)".to_owned());
        }
        let log_format = log_format(lookup("PS_LOG_FORMAT").as_deref(), &mut errors);
        let log_filter = lookup("PS_LOG").unwrap_or_else(|| "info".to_owned());
        if !is_loopback_http(&sim_url) {
            errors.push(
                "PS_SIM_URL must be http://127.0.0.1:<port> or http://localhost:<port>: the network is a local process (D-023)"
                    .to_owned(),
            );
        }
        // Set but empty is not a credential (review N6): only an unset variable was missing
        // before, and an empty one left every operation uncertain while readiness was green.
        if lookup("PS_SIM_TOKEN").is_some()
            && sandbox_domain::access::Credential::new(&sim_token).is_err()
        {
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
            read_edge,
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
            reports,
            transport,
            webhook_ports: sites.webhook_ports,
            reconcile,
            delivery_poll,
            profile: sites.profile,
            checkout: sites.checkout,
            dashboard: sites.dashboard,
            checkout_origin: sites.checkout_origin,
        };
        // Every problem at once: parse errors and cross-field rules together.
        errors.extend(config.problems());
        if errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError(errors))
        }
    }

    /// The read pool's settings (D-100, D-101): its own slice of the budget, its own
    /// acquire and statement deadlines, and read-only transactions — a read path that
    /// tries to write fails loudly (SQLSTATE 25006) instead of silently working.
    pub fn read_pool(&self) -> PoolSettings {
        PoolSettings {
            max_connections: self.read_edge.db_connections,
            acquire_timeout: self.read_edge.db_acquire_timeout,
            statement_timeout: self.read_edge.db_statement_timeout,
            read_only: true,
            ..self.pool
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
        let mut ports = vec![listen.port()];
        for (name, site) in [("CHECKOUT", &self.checkout), ("DASHBOARD", &self.dashboard)] {
            let Some(site) = site else { continue };
            if !site.listen.ip().is_loopback() {
                errors.push(format!(
                    "PS_{name}_LISTEN must be a loopback address (D-023)"
                ));
            }
            if ports.contains(&site.listen.port()) {
                errors.push(format!(
                    "PS_{name}_LISTEN must use its own port: every site is its own origin (D-029)"
                ));
            }
            ports.push(site.listen.port());
        }
        if pool.max_connections == 0 {
            errors.push(
                "PS_DB_READ_CONNECTIONS must leave at least one write connection under PS_DB_MAX_CONNECTIONS"
                    .to_owned(),
            );
        }
        if self.read_edge.db_connections == 0 {
            errors.push("PS_DB_READ_CONNECTIONS must be at least 1".to_owned());
        }
        if !(1_024..=1_048_576).contains(&max_body_bytes) {
            errors.push("PS_MAX_BODY_BYTES must be between 1024 and 1048576".to_owned());
        }
        if self.reports.burst < crate::reports::NETWORK_AGREEMENT {
            errors.push(format!(
                "PS_REPORT_BURST must be at least {}, the cost of the largest report",
                crate::reports::NETWORK_AGREEMENT
            ));
        }
        if self.reports.refill_per_minute == 0 {
            errors.push("PS_REPORT_REFILL_PER_MINUTE must be at least 1".to_owned());
        }
        if self.reports.per_project == 0 || self.reports.concurrency == 0 {
            errors.push(
                "PS_REPORT_PER_PROJECT and PS_REPORT_CONCURRENCY must be at least 1".to_owned(),
            );
        }
        if max_in_flight == 0 {
            errors.push("PS_MAX_IN_FLIGHT must be at least 1".to_owned());
        }
        if self.read_edge.in_flight == 0 {
            errors.push("PS_READ_IN_FLIGHT must be at least 1".to_owned());
        }
        if let Some(read) = self.read_edge.listen {
            if !read.ip().is_loopback() {
                errors.push("PS_READ_LISTEN must be a loopback address (D-023)".to_owned());
            }
            if ports.contains(&read.port()) {
                errors.push("PS_READ_LISTEN must use its own port".to_owned());
            }
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

fn log_format(raw: Option<&str>, errors: &mut Vec<String>) -> LogFormat {
    match raw {
        None | Some("json") => LogFormat::Json,
        Some("pretty") => LogFormat::Pretty,
        Some(_) => {
            errors.push("PS_LOG_FORMAT must be json or pretty".to_owned());
            LogFormat::Json
        }
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

struct Sites {
    checkout: Option<UiSite>,
    checkout_origin: String,
    dashboard: Option<UiSite>,
    profile: Profile,
    webhook_ports: WebhookPorts,
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

    fn reconcile_tuning(&mut self, batch: i64) -> ReconcileTuning {
        let concurrency: usize = self.value("PS_RECONCILE_CONCURRENCY", Some(8));
        let per_project: i64 = self.value("PS_RECONCILE_PER_PROJECT", Some(batch));
        ReconcileTuning {
            concurrency: concurrency.clamp(1, 256),
            per_project: per_project.clamp(1, 10_000),
            backoff_max: self.millis("PS_RECONCILE_BACKOFF_MAX_MS", 300_000),
            alert_age: self.millis("PS_RECONCILE_ALERT_AGE_MS", 600_000),
        }
    }

    fn listen(&mut self) -> SocketAddr {
        let default_listen = SocketAddr::from(([127, 0, 0, 1], 56080));
        match (self.lookup)("PS_LISTEN") {
            None => default_listen,
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                self.errors
                    .push("PS_LISTEN is not a socket address".to_owned());
                default_listen
            }),
        }
    }

    /// The read edge (D-099): `PS_READ_LISTEN` (the edge is off without it), the reads'
    /// own in-flight budget and both bounded waits.
    fn read_edge(&mut self) -> ReadEdge {
        ReadEdge {
            listen: self.optional_listen("PS_READ_LISTEN"),
            in_flight: self.value("PS_READ_IN_FLIGHT", Some(128)),
            queue_wait: self.millis("PS_READ_QUEUE_WAIT_MS", 100),
            accept_wait: self.millis("PS_READ_ACCEPT_WAIT_MS", 100),
            db_connections: self.value("PS_DB_READ_CONNECTIONS", Some(8)),
            // The D-225 rule the bulkhead shares with Colophon: one second, then a
            // named 503 + Retry-After.
            db_acquire_timeout: self.millis("PS_DB_READ_ACQUIRE_TIMEOUT_MS", 1_000),
            db_statement_timeout: self.millis("PS_DB_READ_STATEMENT_TIMEOUT_MS", 1_000),
        }
    }

    /// `Option`al listen address: absent is off, present-but-unparseable is an error.
    fn optional_listen(&mut self, name: &str) -> Option<SocketAddr> {
        let raw = (self.lookup)(name)?;
        if let Ok(address) = raw.parse() {
            Some(address)
        } else {
            self.errors.push(format!("{name} is not a socket address"));
            None
        }
    }

    /// The two sites, the profile and the webhook ports (which may target neither site).
    fn sites(
        &mut self,
        listen: SocketAddr,
        read_listen: Option<SocketAddr>,
        sim_url: &str,
        database_url: &str,
    ) -> Sites {
        let profile = self.profile();
        let (checkout, checkout_origin) = self.ui_site("CHECKOUT", 56083, profile);
        let (dashboard, dashboard_origin) = self.ui_site("DASHBOARD", 56084, profile);
        let mut own_ports: Vec<u16> = [&checkout, &dashboard]
            .into_iter()
            .flatten()
            .map(|site| site.listen.port())
            .collect();
        own_ports.extend(read_listen.map(|read| read.port()));
        let webhook_ports = self.webhook_ports(listen, sim_url, database_url, &own_ports, profile);
        if checkout_origin == dashboard_origin {
            self.errors.push(
                "PS_CHECKOUT_ORIGIN and PS_DASHBOARD_ORIGIN must differ: checkout and dashboard are separate origins (D-029)"
                    .to_owned(),
            );
        }
        Sites {
            checkout,
            checkout_origin,
            dashboard,
            profile,
            webhook_ports,
        }
    }

    fn profile(&mut self) -> Profile {
        Profile::parse((self.lookup)("PS_PROFILE").as_deref()).unwrap_or_else(|_| {
            self.errors
                .push("PS_PROFILE must be dev, ci or shared".to_owned());
            Profile::Shared
        })
    }

    /// `PS_<name>_DIR` (the site is off without it), `PS_<name>_LISTEN` and
    /// `PS_<name>_ORIGIN`; the origin is returned even when the site is off (session URLs
    /// name the checkout's). The origin is a loopback one, or, in the `shared` profile only,
    /// an `https://` name the reverse proxy serves (D-071); the listener is loopback either way.
    fn ui_site(
        &mut self,
        name: &str,
        default_port: u16,
        profile: Profile,
    ) -> (Option<UiSite>, String) {
        let default_listen = SocketAddr::from(([127, 0, 0, 1], default_port));
        let listen = match (self.lookup)(&format!("PS_{name}_LISTEN")) {
            None => default_listen,
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                self.errors
                    .push(format!("PS_{name}_LISTEN is not a socket address"));
                default_listen
            }),
        };
        // The dashboard defaults to a host name of its own (D-065): a cookie set there is
        // never sent to the checkout or any other port of 127.0.0.1.
        let default_origin = if name == "DASHBOARD" {
            format!("http://dashboard.localhost:{}", listen.port())
        } else {
            format!("http://{listen}")
        };
        let raw_origin = (self.lookup)(&format!("PS_{name}_ORIGIN")).unwrap_or(default_origin);
        let origin = sandbox_domain::checkout::canonical_origin(&raw_origin)
            .or_else(|e| match profile {
                Profile::Shared => sandbox_domain::checkout::canonical_public_origin(&raw_origin),
                Profile::Dev | Profile::Ci => Err(e),
            })
            .unwrap_or_else(|_| {
                self.errors.push(format!(
                    "PS_{name}_ORIGIN must be an origin like http://127.0.0.1:{default_port} (loopback, D-023), or, with PS_PROFILE=shared, the https:// name the reverse proxy serves, like https://{} (D-071)",
                    if name == "DASHBOARD" { "dashboard.sandbox.example.com" } else { "pay.sandbox.example.com" }
                ));
                raw_origin.clone()
            });
        let site = (self.lookup)(&format!("PS_{name}_DIR")).map(|dir| UiSite {
            listen,
            dir: PathBuf::from(dir),
            origin: origin.clone(),
        });
        (site, origin)
    }

    fn webhook_ports(
        &mut self,
        listen: SocketAddr,
        sim_url: &str,
        database_url: &str,
        own_ports: &[u16],
        profile: Profile,
    ) -> WebhookPorts {
        let mut denied = vec![listen.port()];
        denied.extend(url_port(sim_url));
        denied.push(url_port(database_url).unwrap_or(5432));
        denied.extend_from_slice(own_ports);
        let allowed = match (self.lookup)("PS_WEBHOOK_PORTS") {
            Some(raw) => Some(
                raw.split(',')
                    .filter(|part| !part.trim().is_empty())
                    .filter_map(|part| {
                        let part = part.trim();
                        let range = match part.split_once('-') {
                            Some((lo, hi)) => lo.parse().ok().zip(hi.parse().ok()),
                            None => part.parse().ok().map(|p: u16| (p, p)),
                        };
                        if range.is_none_or(|(lo, hi): (u16, u16)| lo == 0 || lo > hi) {
                            self.errors.push(format!(
                                "PS_WEBHOOK_PORTS: {part:?} is not a port or a range like 56100-56199"
                            ));
                            return None;
                        }
                        range
                    })
                    .collect(),
            ),
            None if profile == Profile::Dev => None,
            None => Some(Vec::new()),
        };
        WebhookPorts { denied, allowed }
    }

    fn transport(&mut self) -> sandbox_http::Limits {
        let lookup = self.lookup;
        sandbox_http::Limits::from_lookup("PS_", lookup, SERVER_LIMITS).unwrap_or_else(|problems| {
            self.errors.extend(problems);
            SERVER_LIMITS
        })
    }

    fn reports(&mut self) -> ReportSettings {
        ReportSettings {
            burst: self.value("PS_REPORT_BURST", Some(600)),
            refill_per_minute: self.value("PS_REPORT_REFILL_PER_MINUTE", Some(600)),
            per_project: self.value("PS_REPORT_PER_PROJECT", Some(2)),
            concurrency: self.value("PS_REPORT_CONCURRENCY", Some(8)),
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
        // Review N6: set but empty is not a credential either.
        let empty = config(&[("PS_DATABASE_URL", "x"), ("PS_SIM_TOKEN", "")]).unwrap_err();
        assert!(empty.to_string().contains("PS_SIM_TOKEN"), "{empty}");
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

    #[test]
    fn the_sites_may_have_public_https_origins_only_in_the_shared_profile() {
        let base = [
            ("PS_DATABASE_URL", "postgres://x@127.0.0.1:56432/db"),
            ("PS_SIM_TOKEN", SIM_TOKEN),
            ("PS_CHECKOUT_DIR", "apps/checkout/build"),
            ("PS_DASHBOARD_DIR", "apps/dashboard/build"),
            ("PS_CHECKOUT_ORIGIN", "https://Pay.Sandbox.Example.com"),
            (
                "PS_DASHBOARD_ORIGIN",
                "https://dashboard.sandbox.example.com:443",
            ),
        ];
        let mut shared = base.to_vec();
        shared.push(("PS_PROFILE", "shared"));
        let c = config(&shared).unwrap();
        assert_eq!(c.checkout_origin, "https://pay.sandbox.example.com");
        assert_eq!(
            c.dashboard.unwrap().origin,
            "https://dashboard.sandbox.example.com"
        );
        // The listeners stay on loopback whatever the origins say (D-023).
        assert!(c.checkout.unwrap().listen.ip().is_loopback());
        for profile in ["dev", "ci"] {
            let mut local = base.to_vec();
            local.push(("PS_PROFILE", profile));
            let err = config(&local).unwrap_err().to_string();
            assert!(
                err.contains("PS_CHECKOUT_ORIGIN must be an origin"),
                "{profile}: {err}"
            );
            assert!(err.contains("PS_PROFILE=shared"), "{err}");
        }
        for refused in [
            "http://pay.sandbox.example.com",
            "https://10.0.0.5",
            "https://pay.example.com/c",
        ] {
            let mut bad = shared.clone();
            bad.retain(|(k, _)| *k != "PS_CHECKOUT_ORIGIN");
            bad.push(("PS_CHECKOUT_ORIGIN", refused));
            assert!(config(&bad).is_err(), "{refused}");
        }
    }

    #[test]
    fn the_sites_are_off_until_built_and_each_is_its_own_loopback_origin() {
        let base = [
            ("PS_DATABASE_URL", "postgres://x@127.0.0.1:56432/db"),
            ("PS_SIM_TOKEN", SIM_TOKEN),
        ];
        let c = config(&base).unwrap();
        assert_eq!((c.checkout.is_none(), c.dashboard.is_none()), (true, true));
        assert_eq!(c.checkout_origin, "http://127.0.0.1:56083");
        let mut vars = base.to_vec();
        vars.extend([
            ("PS_CHECKOUT_DIR", "apps/checkout/build"),
            ("PS_DASHBOARD_DIR", "apps/dashboard/build"),
            ("PS_PROFILE", "dev"),
        ]);
        let c = config(&vars).unwrap();
        let checkout = c.checkout.clone().unwrap();
        assert_eq!(checkout.listen, "127.0.0.1:56083".parse().unwrap());
        assert_eq!(
            c.dashboard.unwrap().origin,
            "http://dashboard.localhost:56084"
        );
        assert_eq!(c.profile, Profile::Dev);
        // Webhooks may never target the sites' ports (D-059 extended).
        assert!(!c.webhook_ports.permits(56083) && !c.webhook_ports.permits(56084));
        assert!(c.webhook_ports.permits(56464));
        for (name, value, needle) in [
            (
                "PS_CHECKOUT_LISTEN",
                "0.0.0.0:56083",
                "PS_CHECKOUT_LISTEN must be a loopback",
            ),
            (
                "PS_DASHBOARD_LISTEN",
                "127.0.0.1:56080",
                "PS_DASHBOARD_LISTEN must use its own port",
            ),
            (
                "PS_CHECKOUT_ORIGIN",
                "https://example.com:443",
                "PS_CHECKOUT_ORIGIN must be an origin",
            ),
            (
                "PS_DASHBOARD_ORIGIN",
                "http://127.0.0.1:56083",
                "must differ",
            ),
        ] {
            let mut bad = vars.clone();
            bad.retain(|(k, _)| *k != name);
            bad.push((name, value));
            let err = config(&bad).unwrap_err().to_string();
            assert!(err.contains(needle), "{name}={value}: {err}");
        }
    }
}
