//! `sandbox-server`: load config, connect, migrate, serve, drain.

use std::time::Duration;

use anyhow::Context as _;
use sandbox_server::supervisor::supervise;
use sandbox_server::{AppState, Config, shutdown, telemetry};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve().await,
        Some("setup") => setup().await,
        Some("migrate") => migrate_command().await,
        Some(other) => anyhow::bail!("unknown command {other:?}; use serve, setup or migrate"),
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
    let mut admin = sqlx::PgConnection::connect(&admin_url)
        .await
        .context("connecting as the administrator")?;
    sandbox_db::roles::bootstrap(&mut admin)
        .await
        .context("creating the roles")?;
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

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    telemetry::init(config.log_format, &config.log_filter).context("initialising telemetry")?;
    tracing::warn!(
        notice = sandbox_domain::SANDBOX_NOTICE,
        "starting payment sandbox server"
    );

    let signal = shutdown::os_signal().context("installing signal handlers")?;
    let db = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool)
        .context("configuring the database pool")?;
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
    let grace = config.shutdown_grace;
    let transport = config.transport;
    let state = AppState::new(config, db.clone());
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

    let outcome = serve_all(listener, app, sites, transport, grace, signal).await?;
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
    for (name, site_listener, router) in sites {
        let addr = site_listener
            .local_addr()
            .context("reading a site's bound address")?;
        tracing::info!(site = name, %addr, "listening");
        let rx = stopping.clone();
        site_tasks.spawn(async move {
            let served =
                shutdown::serve(site_listener, router, transport, grace, stopped(rx)).await;
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
