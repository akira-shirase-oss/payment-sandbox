//! Signals, and serving within transport limits with a hard drain deadline (D-054):
//! axum's graceful shutdown has no deadline and its server no transport timeouts.

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;

pub use sandbox_http::ShutdownOutcome;

/// Installs SIGTERM and Ctrl-C handlers now (a failure is a startup error) and returns a
/// future that resolves on either.
pub fn os_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if result.is_err() {
                    std::future::pending::<()>().await;
                }
                "SIGINT"
            }
            _ = terminate.recv() => "SIGTERM",
        }
    })
}

/// Serves until `trigger` within the transport `limits` (D-054), then stops accepting and
/// drains for at most `grace`.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: sandbox_http::Limits,
    grace: Duration,
    trigger: impl Future<Output = ()> + Send + 'static,
) -> io::Result<ShutdownOutcome> {
    sandbox_http::serve(listener, app, limits, grace, trigger).await
}
