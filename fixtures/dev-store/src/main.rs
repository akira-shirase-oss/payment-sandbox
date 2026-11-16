//! `sandbox-dev-store`: the development store as its own process (loopback only).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use sandbox_dev_store::{DevStore, StoreConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().init();
    tracing::warn!(
        notice = sandbox_domain::SANDBOX_NOTICE,
        "starting the development store"
    );
    let listen: SocketAddr = std::env::var("PS_STORE_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:56082".to_owned())
        .parse()
        .context("PS_STORE_LISTEN")?;
    anyhow::ensure!(
        listen.ip().is_loopback(),
        "PS_STORE_LISTEN must be loopback (D-023)"
    );
    let path = PathBuf::from(
        std::env::var("PS_STORE_DB").unwrap_or_else(|_| "sandbox-dev-store.redb".to_owned()),
    );
    let config = StoreConfig {
        sandbox_url: std::env::var("PS_STORE_SANDBOX_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:56080".to_owned()),
        api_key: std::env::var("PS_STORE_API_KEY").unwrap_or_default(),
        secrets: std::env::var("PS_STORE_WEBHOOK_SECRETS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    let store = DevStore::open(&path, config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .context("binding")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    sandbox_dev_store::serve(listener, store, async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    })
    .await
    .context("serving")
}
