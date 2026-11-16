//! `sandbox-sim`: the simulated card network, as its own process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use sandbox_sim::{SimState, faults::Faults, store::Store};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_new(std::env::var("PS_SIM_LOG").unwrap_or_else(|_| "info".to_owned()))?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();
    tracing::warn!(
        notice = sandbox_domain::SANDBOX_NOTICE,
        "starting the simulated network"
    );

    let listen: SocketAddr = std::env::var("PS_SIM_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:56081".to_owned())
        .parse()
        .context("PS_SIM_LISTEN is not a socket address")?;
    anyhow::ensure!(
        listen.ip().is_loopback(),
        "PS_SIM_LISTEN must be a loopback address (D-023)"
    );
    let path =
        PathBuf::from(std::env::var("PS_SIM_DB").unwrap_or_else(|_| "sandbox-sim.redb".to_owned()));
    let hold_ms: u64 = std::env::var("PS_SIM_LOST_REPLY_HOLD_MS")
        .unwrap_or_else(|_| "15000".to_owned())
        .parse()
        .context("PS_SIM_LOST_REPLY_HOLD_MS is not a number")?;

    let store = Store::open(&path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let state = SimState {
        store: store.clone(),
        faults: Arc::new(Faults::default()),
        lost_reply_hold: Duration::from_millis(hold_ms),
    };
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, store = %path.display(), "listening");
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let server = sandbox_sim::serve(listener, state, Duration::from_secs(5), async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    });
    server.await.context("serving")?;
    tokio::time::timeout(Duration::from_secs(5), store.close())
        .await
        .ok();
    Ok(())
}
