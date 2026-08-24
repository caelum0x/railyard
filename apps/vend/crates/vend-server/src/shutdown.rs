//! Bounded graceful shutdown for HTTP and long-lived SSE connections.

use std::future::{Future, IntoFuture};
use std::io;
use std::time::Duration;

use axum::Router;
use tokio::sync::watch;

/// Serve until the shutdown future resolves, allow in-flight requests to drain,
/// then force-close after `grace` so a broken client cannot block deployment.
pub async fn serve_until<F>(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: F,
    grace: Duration,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut observed_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    });

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut shutdown_rx = shutdown_rx;
            while !*shutdown_rx.borrow() && shutdown_rx.changed().await.is_ok() {}
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result,
        changed = observed_shutdown.changed() => {
            if changed.is_err() {
                return server.await;
            }
            match tokio::time::timeout(grace, &mut server).await {
                Ok(result) => result,
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Vend graceful shutdown exceeded {} seconds", grace.as_secs()),
                )),
            }
        }
    }
}

/// Wait for SIGINT or SIGTERM. Signal registration failures are logged and the
/// remaining available signal continues to work.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("Vend shutdown signal received");
}
