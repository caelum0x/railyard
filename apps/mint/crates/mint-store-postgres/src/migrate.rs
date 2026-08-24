//! Forward-only migrations, embedded at compile time and applied at startup.
//!
//! `sqlx::migrate!` embeds `./migrations/*.sql` into the binary and tracks the
//! applied version in `_sqlx_migrations`, so re-running is a clean no-op. This
//! is the `migrate` FEATURE (embedding), NOT the `query!` compile-time macro —
//! it needs no live `DATABASE_URL` to build.

use sqlx::PgPool;

/// Apply every pending migration in `migrations/` to `pool`.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}
