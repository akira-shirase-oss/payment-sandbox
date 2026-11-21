//! `sandbox-server`: load config, connect, migrate, serve, drain.

use std::time::Duration;

use anyhow::Context as _;
use sandbox_server::supervisor::supervise;
use sandbox_server::{AppState, Config, shutdown, telemetry};
use tokio::net::TcpListener;

const HELP: &str = "sandbox-server: the Payment Sandbox API, checkout and dashboard.
SIMULATED MONEY ONLY: Payment Sandbox moves no real money and accepts no real cards.

Usage:
  sandbox-server [serve]   serve (settings from PS_* variables; docs/OPERATING.md lists them)
  sandbox-server setup     create the roles and the database, as the administrator
                           (PS_ADMIN_DATABASE_URL; PS_DATABASE_NAME, PS_OWNER_PASSWORD and
                           PS_APP_PASSWORD optional; existing passwords are never reset)
  sandbox-server migrate   apply migrations as the owner (PS_MIGRATE_DATABASE_URL)
  sandbox-server --version";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve().await,
        Some("setup") => setup().await,
        Some("migrate") => migrate_command().await,
        Some("--version" | "-V" | "version") => {
            println!(
                "sandbox-server {}\n{}",
                env!("CARGO_PKG_VERSION"),
                sandbox_domain::SANDBOX_NOTICE
            );
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}; run sandbox-server --help"),
    }
}

/// `sandbox-server setup`: as the stack's administrator (`PS_ADMIN_DATABASE_URL`), creates
/// the roles (D-038) and the database `PS_DATABASE_NAME` (default `payment_sandbox`) if it
/// does not exist.
async fn setup() -> anyhow::Result<()> {
    use sqlx::Connection as _;
    let admin_url =
        std::env::var("PS_ADMIN_DATABASE_URL").context("PS_ADMIN_DATABASE_URL is required")?;
    let name = std::env::var("PS_DATABASE_NAME").unwrap_or_else(|_| "payment_sandbox".to_owned());
    let passwords = sandbox_db::roles::RolePasswords::new(
        std::env::var("PS_OWNER_PASSWORD").ok(),
        std::env::var("PS_APP_PASSWORD").ok(),
    )
    .context("PS_OWNER_PASSWORD / PS_APP_PASSWORD")?;
    let mut admin = sqlx::PgConnection::connect(&admin_url)
        .await
        .context("connecting as the administrator")?;
    sandbox_db::roles::bootstrap_with(&mut admin, &passwords)
        .await
        .context("creating the roles")?;
    if passwords == sandbox_db::roles::RolePasswords::default() {
        eprintln!(
            "note: roles created now get the public development passwords; on a shared host set PS_OWNER_PASSWORD and PS_APP_PASSWORD (existing passwords were left as they are)"
        );
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&name)
            .fetch_one(&mut admin)
            .await?;
    if !exists {
        sandbox_db::roles::create_database(&mut admin, &name)
            .await
            .context("creating the database")?;
    }
    println!("setup ok: roles ps_owner, ps_runtime, ps_app; database {name}");
    Ok(())
}

/// `sandbox-server migrate`: applies migrations as the owner (`PS_MIGRATE_DATABASE_URL`).
/// The server itself never migrates: it does not own the schema.
async fn migrate_command() -> anyhow::Result<()> {
    let url =
        std::env::var("PS_MIGRATE_DATABASE_URL").context("PS_MIGRATE_DATABASE_URL is required")?;
    let db = sandbox_db::connect_lazy(&url, &sandbox_db::PoolSettings::default())
        .context("configuring the pool")?;
    migrate(&db).await?;
    db.close().await;
    println!("migrate ok");
    Ok(())
}

/// The write and read pools (D-100): separate budgets and separate acquire deadlines,
/// so a write flood can never starve a read at the database.
fn pools(config: &Config) -> anyhow::Result<(sqlx::PgPool, sqlx::PgPool)> {
    let url = config.database_url.expose();
    let write =
        sandbox_db::connect_lazy(url, &config.pool).context("configuring the database pool")?;
    let read = sandbox_db::connect_lazy(url, &config.read_pool())
        .context("configuring the read database pool")?;
    Ok((write, read))
}

/// A site's listener cap (S2-4): the sites serve static files — their share of the
/// descriptor budget is deliberately smaller than an API edge's.
const SITE_MAX_CONNECTIONS: usize = 128;

/// Sockets the process needs beyond its listeners: the pools, the auth listener, the
/// simulator and webhook clients, files, logging — plus slack for runtimes' internals.
const RESERVED_FDS: usize = 320;

/// The whole descriptor budget, pure so the arithmetic is testable: `edges` API-shaped
/// listeners at `edge` sockets each (cap + pending — a pending connection holds a real
/// socket), `sites` static listeners at the site cap, plus the pools and the reserve.
fn descriptor_need(edges: usize, edge: usize, sites: usize, pools: usize) -> u64 {
    let listeners = edges
        .saturating_mul(edge)
        .saturating_add(sites.saturating_mul(SITE_MAX_CONNECTIONS * 2));
    listeners.saturating_add(pools).saturating_add(RESERVED_FDS) as u64
}

/// Refuses to boot when Σ(cap+pending) over every listener plus the pools and the
/// reserve cannot fit the process's descriptor limit (S2-4): the API edge, the read
/// edge and each site hold up to cap+pending sockets, and the systemd unit's
/// LimitNOFILE=4096 is reachable at the defaults once sites are on — EMFILE would break
/// database connects, the listener's reconnect and webhook deliveries mid-run. When
/// only the soft limit is low (a dev shell's default), it is raised to the need up to
/// the hard bound; beyond the hard bound, startup fails with the budget spelled out.
fn descriptor_budget(config: &Config) -> anyhow::Result<()> {
    let transport = config.transport;
    let edge = transport.max_connections + transport.max_pending;
    let sites = usize::from(config.checkout.is_some()) + usize::from(config.dashboard.is_some());
    let pools = usize::try_from(config.pool.max_connections).unwrap_or(usize::MAX)
        + usize::try_from(config.read_edge.db_connections).unwrap_or(usize::MAX)
        + 16;
    let listeners = edge * (1 + usize::from(config.read_edge.listen.is_some()))
        + sites * SITE_MAX_CONNECTIONS * 2;
    let need = descriptor_need(
        1 + usize::from(config.read_edge.listen.is_some()),
        edge,
        sites,
        pools,
    );
    let rlim = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    if need <= rlim.current.unwrap_or(u64::MAX) {
        return Ok(());
    }
    if need <= rlim.maximum.unwrap_or(u64::MAX) {
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(need),
                maximum: rlim.maximum,
            },
        )
        .map_err(|e| anyhow::anyhow!("setrlimit(RLIMIT_NOFILE={need}): {e}"))?;
        tracing::warn!(
            need,
            "raised the soft descriptor limit to the configured budget"
        );
        return Ok(());
    }
    anyhow::bail!(
        "the descriptor budget ({need}) exceeds RLIMIT_NOFILE ({}): {listeners} listener \
         sockets + {pools} pool and client sockets + {RESERVED_FDS} reserved; lower \
         PS_MAX_CONNECTIONS/PS_MAX_PENDING or raise the service's LimitNOFILE",
        rlim.maximum.unwrap_or(u64::MAX),
    );
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    telemetry::init(config.log_format, &config.log_filter).context("initialising telemetry")?;
    descriptor_budget(&config)?;
    tracing::warn!(
        notice = sandbox_domain::SANDBOX_NOTICE,
        "starting payment sandbox server"
    );

    let signal = shutdown::os_signal().context("installing signal handlers")?;
    let (db, read_db) = pools(&config)?;
    // Refuse to serve as a role that could switch the journal's protections off (D-038).
    match sandbox_db::roles::check_runtime(&db).await {
        Ok(()) => {}
        Err(sandbox_db::roles::RoleCheckError::Refused(problem)) => anyhow::bail!("{problem}"),
        Err(sandbox_db::roles::RoleCheckError::Db(error)) => {
            return Err(error)
                .context("checking the database role (is the database migrated and reachable?)");
        }
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    let local = listener.local_addr().context("reading the bound address")?;
    // The checkout and the dashboard, each on its own listener (D-029, D-061), bound and
    // loaded before anything is served, so a missing build or a taken port fails startup.
    let checkout = bind_site(config.checkout.as_ref(), "checkout").await?;
    let dashboard = bind_site(config.dashboard.as_ref(), "dashboard").await?;
    let (grace, transport) = (config.shutdown_grace, config.transport);
    // The read edge (D-099): the same API on its own listener, whose connections get
    // their own serving places and a short bounded wait — write connections can never
    // queue ahead of them. `PS_READ_LISTEN` turns it on.
    let read = match config.read_edge.listen {
        Some(addr) => Some((
            TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding the read edge on {addr}"))?,
            sandbox_http::Limits {
                accept_wait: config.read_edge.accept_wait,
                ..transport
            },
        )),
        None => None,
    };
    let state = AppState::new(config, db.clone(), read_db);
    let app = sandbox_server::app(&state);
    let sites = site_routers(&state, checkout, dashboard);
    // The background tasks run supervised (D-051): a panic is logged and restarted with
    // backoff, never silently gone, and readiness reports a task that is down or stuck.
    let (stop_reconciler, reconciler_stop) = sandbox_server::reconciler::shutdown_channel();
    let tasks = state.tasks.clone();
    let interval = state.config.reconcile_interval;
    tasks.register(
        sandbox_server::reconciler::TASK,
        interval.saturating_mul(10) + Duration::from_secs(120),
    );
    let reconciler = {
        let (state, stop) = (state.clone(), reconciler_stop.clone());
        tokio::spawn(supervise(
            tasks.clone(),
            sandbox_server::reconciler::TASK,
            reconciler_stop.clone(),
            move || sandbox_server::reconciler::run(state.clone(), stop.clone()),
        ))
    };
    spawn_pool_metrics(&state, &tasks, &reconciler_stop);
    spawn_auth_listener(&state, &tasks, &reconciler_stop);
    // The deliverer, when webhooks are on (PS_SECRET_KEY).
    let deliverer = sandbox_server::deliverer::Deliverer::new(
        &state,
        std::sync::Arc::new(sandbox_server::deliverer::SystemClock),
    )
    .map(|d| {
        tasks.register(sandbox_server::deliverer::TASK, Duration::from_secs(120));
        let stop = reconciler_stop.clone();
        tokio::spawn(supervise(
            tasks.clone(),
            sandbox_server::deliverer::TASK,
            reconciler_stop,
            move || d.clone().run(stop.clone()),
        ))
    });
    if deliverer.is_none() {
        tracing::warn!("webhooks are off: set PS_SECRET_KEY (64 hex characters) to deliver events");
    }
    tracing::info!(%local, "listening");

    let outcome = serve_all(listener, app, sites, read, transport, grace, signal).await?;
    match outcome {
        shutdown::ShutdownOutcome::Graceful => tracing::info!("drained; exiting"),
        shutdown::ShutdownOutcome::DeadlineExceeded => {
            tracing::warn!("shutdown deadline exceeded; exiting with requests in flight");
        }
    }
    let _ = stop_reconciler.send(true);
    if tokio::time::timeout(Duration::from_secs(5), reconciler)
        .await
        .is_err()
    {
        tracing::warn!("the reconciler did not stop within 5 s");
    }
    if let Some(deliverer) = deliverer
        && tokio::time::timeout(Duration::from_secs(5), deliverer)
            .await
            .is_err()
    {
        tracing::warn!("the deliverer did not stop within 5 s");
    }
    db.close().await;
    Ok(())
}

/// The pool sampler (D-101): records each pool's occupancy so a saturated pool is
/// evidence in a load run, not a guess. Stops on the same shutdown channel as the
/// reconciler; a panic restarts it and readiness notices a missed beat.
fn spawn_pool_metrics(
    state: &AppState,
    tasks: &std::sync::Arc<sandbox_server::supervisor::Tasks>,
    stop: &tokio::sync::watch::Receiver<bool>,
) {
    tasks.register(sandbox_server::poolstat::TASK, Duration::from_secs(30));
    let (tasks, state, stop) = (tasks.clone(), state.clone(), stop.clone());
    tokio::spawn(supervise(
        tasks,
        sandbox_server::poolstat::TASK,
        stop.clone(),
        move || sandbox_server::poolstat::run(state.clone(), stop.clone()),
    ));
}

/// The `auth_epoch` listener (D-102's addendum): a revocation or dataset reset in ANY
/// process — this one, another server, a psql — reaches every process's key cache, so
/// the 30 s TTL is only the missed-notification backstop.
fn spawn_auth_listener(
    state: &AppState,
    tasks: &std::sync::Arc<sandbox_server::supervisor::Tasks>,
    stop: &tokio::sync::watch::Receiver<bool>,
) {
    tasks.register(sandbox_server::authlisten::TASK, Duration::from_secs(30));
    let (tasks, state, stop) = (tasks.clone(), state.clone(), stop.clone());
    tokio::spawn(supervise(
        tasks,
        sandbox_server::authlisten::TASK,
        stop.clone(),
        move || sandbox_server::authlisten::run(state.clone(), stop.clone()),
    ));
}

/// A bound listener, its loaded site and its origin.
type BoundSite = (
    TcpListener,
    std::sync::Arc<sandbox_server::web::site::Site>,
    String,
);

/// The routers of the configured sites.
fn site_routers(
    state: &AppState,
    checkout: Option<BoundSite>,
    dashboard: Option<BoundSite>,
) -> Vec<(&'static str, TcpListener, axum::Router)> {
    let mut sites = Vec::new();
    if let Some((listener, site, origin)) = checkout {
        let router = sandbox_server::web::checkout::router(state, site, &origin);
        sites.push(("checkout", listener, router));
    }
    if let Some((listener, site, origin)) = dashboard {
        let sessions = std::sync::Arc::new(sandbox_server::web::dashboard::Sessions::default());
        let router = sandbox_server::web::dashboard::router(state, site, &origin, sessions);
        sites.push(("dashboard", listener, router));
    }
    sites
}

/// Serves the API and every site until `signal`, then drains each within `grace`.
async fn serve_all(
    listener: TcpListener,
    app: axum::Router,
    sites: Vec<(&'static str, TcpListener, axum::Router)>,
    read: Option<(TcpListener, sandbox_http::Limits)>,
    transport: sandbox_http::Limits,
    grace: Duration,
    signal: impl std::future::Future<Output = &'static str> + Send + 'static,
) -> anyhow::Result<shutdown::ShutdownOutcome> {
    // One signal stops every listener.
    let (stop_serving, stopping) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(async move {
        let name = signal.await;
        tracing::info!(signal = name, "shutdown signal received; draining");
        let _ = stop_serving.send(true);
    });
    let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };
    let mut site_tasks = tokio::task::JoinSet::new();
    if let Some((read_listener, read_limits)) = read {
        let addr = read_listener
            .local_addr()
            .context("reading the read edge's bound address")?;
        tracing::info!(site = "read", %addr, "listening");
        let rx = stopping.clone();
        let app = app.clone();
        site_tasks.spawn(async move {
            let served = shutdown::serve(read_listener, app, read_limits, grace, stopped(rx)).await;
            ("read", served)
        });
    }
    // S2-4: a site's cap is a share of the descriptor budget, not the API edge's.
    let site_limits = sandbox_http::Limits::with_connections(SITE_MAX_CONNECTIONS);
    for (name, site_listener, router) in sites {
        let addr = site_listener
            .local_addr()
            .context("reading a site's bound address")?;
        tracing::info!(site = name, %addr, "listening");
        let rx = stopping.clone();
        site_tasks.spawn(async move {
            let served =
                shutdown::serve(site_listener, router, site_limits, grace, stopped(rx)).await;
            (name, served)
        });
    }
    let outcome = shutdown::serve(listener, app, transport, grace, stopped(stopping))
        .await
        .context("serving");
    while let Some(joined) = site_tasks.join_next().await {
        match joined {
            Ok((_, Ok(_))) => {}
            Ok((name, Err(error))) => {
                tracing::error!(site = name, %error, "a site stopped serving");
            }
            Err(error) => tracing::error!(%error, "a site's serving task failed"),
        }
    }
    watcher.abort();
    outcome
}

/// Binds a site's listener and loads its build, if the site is configured.
async fn bind_site(
    site: Option<&sandbox_server::config::UiSite>,
    name: &str,
) -> anyhow::Result<Option<BoundSite>> {
    let Some(site) = site else { return Ok(None) };
    let dir = site.dir.clone();
    let loaded = tokio::task::spawn_blocking(move || sandbox_server::web::site::Site::load(&dir))
        .await
        .context("loading a site")?
        .with_context(|| format!("loading the {name} from {}", site.dir.display()))?;
    let listener = TcpListener::bind(site.listen)
        .await
        .with_context(|| format!("binding the {name} on {}", site.listen))?;
    Ok(Some((
        listener,
        std::sync::Arc::new(loaded),
        site.origin.clone(),
    )))
}

/// Applies migrations, retrying while the database starts, for at most 30 s.
async fn migrate(db: &sqlx::PgPool) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut delay = Duration::from_millis(200);
    loop {
        match tokio::time::timeout(Duration::from_secs(20), sandbox_db::MIGRATOR.run(db)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) if tokio::time::Instant::now() + delay < deadline => {
                tracing::warn!(%error, "migrations failed; retrying");
            }
            Ok(Err(error)) => return Err(error).context("applying migrations"),
            Err(_) => anyhow::bail!("applying migrations exceeded 20 s"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults must fit the unit's LimitNOFILE=4096: API + read edge at the default
    /// 512+512 each, both sites at theirs, ~40 pool sockets — and the arithmetic itself
    /// saturates instead of wrapping when a config goes far past sane.
    #[test]
    fn the_default_descriptor_budget_fits_the_unit_limit() {
        let defaults = descriptor_need(2, 512 + 512, 2, 32 + 8 + 16);
        assert!(
            defaults < 4096 - 512,
            "the default budget {defaults} must leave room under 4096"
        );
        // A wildly over-configured budget is computable — the check can refuse it.
        let absurd = descriptor_need(2, 4_096 + 4_096, 2, 64);
        assert!(
            absurd > 4096,
            "an over-cap budget must exceed the unit limit"
        );
    }
}
