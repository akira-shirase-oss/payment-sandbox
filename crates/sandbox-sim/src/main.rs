//! `sandbox-sim`: the simulated card network, as its own process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use sandbox_domain::access::{self, Credential, Profile};
use sandbox_sim::{Access, SimState, faults::Faults, store::Store};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version" | "-V") => {
            println!(
                "sandbox-sim {}\n{}",
                env!("CARGO_PKG_VERSION"),
                sandbox_domain::SANDBOX_NOTICE
            );
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "sandbox-sim: the simulated card network, with its own store.\n{}\n\nUsage: sandbox-sim (settings from PS_SIM_* and PS_PROFILE; docs/OPERATING.md lists them)",
                sandbox_domain::SANDBOX_NOTICE
            );
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown argument {other:?}; run sandbox-sim --help"),
    }
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

    let access = read_access()?;
    if access.control.is_some() {
        tracing::warn!(
            "the test control plane is ON: faults and the virtual clock answer PS_SIM_CONTROL_TOKEN"
        );
    }

    let store = Store::open(&path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let state = SimState {
        store: store.clone(),
        faults: Arc::new(Faults::default()),
        lost_reply_hold: Duration::from_millis(hold_ms),
        access: Arc::new(access),
        transport: sandbox_http::Limits::from_lookup(
            "PS_SIM_",
            |name| std::env::var(name).ok(),
            sandbox_sim::SIM_LIMITS,
        )
        .map_err(|problems| anyhow::anyhow!("{}", problems.join("; ")))?,
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

/// The credentials (D-046): `PS_SIM_TOKEN` is required; the control plane follows
/// `PS_SIM_CONTROL` (`on`/`off`), by default on only when `PS_PROFILE=dev`, and then needs
/// `PS_SIM_CONTROL_TOKEN`.
fn read_access() -> anyhow::Result<Access> {
    let var = |name: &str| std::env::var(name).ok();
    let profile = Profile::parse(var("PS_PROFILE").as_deref()).context("PS_PROFILE")?;
    let token = var("PS_SIM_TOKEN")
        .context("PS_SIM_TOKEN is required: the credential the server presents (D-046)")?;
    let network = Credential::new(&token).context("PS_SIM_TOKEN")?;
    let control = access::control_plane(
        profile,
        var("PS_SIM_CONTROL").as_deref(),
        var("PS_SIM_CONTROL_TOKEN").as_deref(),
        &[&token],
    )
    .context("the test control plane (PS_SIM_CONTROL, PS_SIM_CONTROL_TOKEN)")?;
    Ok(Access { network, control })
}
