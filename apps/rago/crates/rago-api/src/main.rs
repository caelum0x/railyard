//! Rago REST API server entrypoint.
//!
//! Connects to Postgres (`DATABASE_URL`), runs the embedded migrations, and
//! serves the v1 API. Set `RAGO_API_ADDR` to override the bind address.

use rago_api::AppState;
use rago_db::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Fail fast on a missing JWT secret (release builds refuse the insecure
    // development fallback) before accepting any traffic.
    rago_graphql::auth::validate_jwt_config()?;

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/rago".into());
    let db = Db::connect(&database_url).await?;
    db.migrate().await?;
    tracing::info!("connected to Postgres and applied migrations");

    #[cfg(feature = "live")]
    let state = AppState::new_live(db)?;
    #[cfg(not(feature = "live"))]
    let state = AppState::new(db);

    let addr = std::env::var("RAGO_API_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("rago-api listening on {addr}");

    axum::serve(listener, rago_api::router(state)).await?;
    Ok(())
}
