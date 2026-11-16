//! Graceful shutdown with a hard deadline: axum's graceful shutdown has none of its own.

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// How serving ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Every in-flight request finished.
    Graceful,
    /// The deadline elapsed with requests still running.
    DeadlineExceeded,
}

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

/// Serves until `trigger`, then stops accepting and drains for at most `grace`.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    grace: Duration,
    trigger: impl Future<Output = ()> + Send + 'static,
) -> io::Result<ShutdownOutcome> {
    let (triggered_tx, triggered_rx) = oneshot::channel::<()>();
    let stop = async move {
        trigger.await;
        let _ = triggered_tx.send(());
    };
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(stop)
        .into_future();
    let deadline = async move {
        if triggered_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(grace).await;
    };
    tokio::select! {
        result = server => result.map(|()| ShutdownOutcome::Graceful),
        () = deadline => Ok(ShutdownOutcome::DeadlineExceeded),
    }
}
