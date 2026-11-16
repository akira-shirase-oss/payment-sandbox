//! `sandbox-server`: load config, connect, migrate, serve, drain.

use std::time::Duration;

use anyhow::Context as _;
use sandbox_server::{AppState, Config, shutdown, telemetry};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    telemetry::init(config.log_format, &config.log_filter).context("initialising telemetry")?;
    tracing::warn!(
        notice = sandbox_domain::SANDBOX_NOTICE,
        "starting payment sandbox server"
    );

    let signal = shutdown::os_signal().context("installing signal handlers")?;
    let db = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool)
        .context("configuring the database pool")?;
    if config.migrate_on_start {
        migrate(&db).await?;
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    let local = listener.local_addr().context("reading the bound address")?;
    let grace = config.shutdown_grace;
    let state = AppState::new(config, db.clone());
    let app = sandbox_server::app(&state);
    // The reconciler: a tracked task that stops on shutdown (startup sweep included).
    let (stop_reconciler, reconciler_stop) = sandbox_server::reconciler::shutdown_channel();
    let reconciler = tokio::spawn(sandbox_server::reconciler::run(
        state.clone(),
        reconciler_stop,
    ));
    tracing::info!(%local, "listening");

    let outcome = shutdown::serve(listener, app, grace, async move {
        let name = signal.await;
        tracing::info!(signal = name, "shutdown signal received; draining");
    })
    .await
    .context("serving")?;
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
    db.close().await;
    Ok(())
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
