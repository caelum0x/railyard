//! Port of `main.go` — process entry point: logging, graceful shutdown,
//! tracing, Sentry, optional in-memory cache, then `StartProcessingEvents`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use rago::cache::Cache;
use rago::config::tracing as rago_tracing;
use rago::processors::{start_processing_events, Config};
use rago::utils::{env as envutil, logger};

const ENV_ENV: &str = "ENV";
const ENV_SENTRY_DSN: &str = "SENTRY_DSN";
const ENV_USE_MEMORY_CACHE: &str = "LAGO_USE_MEMORY_CACHE";
const ENV_DEBEZIUM_TOPIC_PREFIX: &str = "LAGO_DEBEZIUM_TOPIC_PREFIX";

#[tokio::main]
async fn main() {
    let env = envutil::get_env_or_default(ENV_ENV, "development");

    logger::init_logger(&env);

    let token = CancellationToken::new();
    setup_graceful_shutdown(token.clone());

    // Tracing provider (OTel/Datadog-via-OTLP, or a no-op empty provider).
    let tracer_provider = rago_tracing::init_tracer_provider();
    if tracer_provider.kind() == rago_tracing::TracingProviderKind::Empty {
        tracing::info!("Tracing disabled (no provider configured)");
    }

    // Sentry — a blank DSN disables it gracefully.
    let _sentry_guard = sentry::init((
        std::env::var(ENV_SENTRY_DSN).unwrap_or_default(),
        sentry::ClientOptions {
            environment: Some(env.clone().into()),
            attach_stacktrace: true,
            ..Default::default()
        },
    ));

    // Optional in-memory cache (Debezium CDC path). The read side is fully
    // ported; the snapshot/CDC *population* is the remaining memcache work
    // (see report). Without it we always use the DB-backed path.
    let mem_cache: Option<Arc<Cache>> = build_mem_cache();

    start_processing_events(token, Config { cache: mem_cache }).await;

    tracer_provider.stop();
}

#[cfg(feature = "memcache")]
fn build_mem_cache() -> Option<Arc<Cache>> {
    if std::env::var(ENV_USE_MEMORY_CACHE).as_deref() != Ok("true") {
        return None;
    }
    let prefix = std::env::var(ENV_DEBEZIUM_TOPIC_PREFIX).unwrap_or_default();
    match Cache::new(prefix) {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => rago::utils::error_tracker::log_and_panic(
            &rago::utils::RagoError::Message(e.to_string()),
            "Error creating the cache",
        ),
    }
}

#[cfg(not(feature = "memcache"))]
fn build_mem_cache() -> Option<Arc<Cache>> {
    if std::env::var(ENV_USE_MEMORY_CACHE).as_deref() == Ok("true") {
        tracing::error!(
            "LAGO_USE_MEMORY_CACHE=true but rago was built without the `memcache` feature; \
             using the database-backed path"
        );
    }
    let _ = ENV_DEBEZIUM_TOPIC_PREFIX;
    None
}

/// Port of `setupGracefulShutdown` — cancel the token on SIGINT/SIGTERM.
fn setup_graceful_shutdown(token: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
            let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! {
                _ = sigint.recv() => tracing::info!(signal = "SIGINT", "Received shutdown signal"),
                _ = sigterm.recv() => tracing::info!(signal = "SIGTERM", "Received shutdown signal"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!(signal = "CTRL_C", "Received shutdown signal");
        }
        token.cancel();
    });
}
