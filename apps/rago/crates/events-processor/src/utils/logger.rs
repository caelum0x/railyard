//! Port of the logging setup in `main.go` + `utils/logger.go`.
//!
//! Go uses `slog` with a JSON handler and a `service=post_process` attribute,
//! at Debug level in development and Info otherwise. Here we configure a
//! `tracing_subscriber` JSON formatter with the equivalent level.

use tracing_subscriber::EnvFilter;

/// Initialise the global JSON logger. `env` is the `ENV` value ("development"
/// enables debug-level logging, like the Go service).
pub fn init_logger(env: &str) {
    let default_level = if env == "development" {
        "debug"
    } else {
        "info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    // `try_init` so double-initialisation in tests is a no-op rather than a panic.
    let _ = tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_env_filter(filter)
        .try_init();
}
