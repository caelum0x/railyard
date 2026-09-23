//! Route table. Health checks are public; everything else requires `X-API-Key`
//! (checked inside each handler for a per-route fail-closed guarantee).

use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::handlers;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/console", get(crate::console::page))
        .route("/readyz", get(handlers::health::readyz))
        .route(
            "/v1/passports",
            post(handlers::passports::create).get(handlers::passports::list),
        )
        .route(
            "/v1/passports/:id",
            get(handlers::passports::get).patch(handlers::passports::update_draft),
        )
        .route("/v1/passports/:id/lock", post(handlers::passports::lock))
        .route(
            "/v1/passports/:id/archive",
            post(handlers::passports::archive),
        )
        .route(
            "/v1/passports/:id/unarchive",
            post(handlers::passports::unarchive),
        )
        .route(
            "/v1/passports/:id/versions",
            post(handlers::versions::new_version).get(handlers::versions::list_versions),
        )
        .route(
            "/v1/passports/:id/versions/:version",
            get(handlers::versions::get_version),
        )
        .route(
            "/v1/passports/:id/resolve",
            post(handlers::generate::resolve),
        )
        .route(
            "/v1/passports/:id/generate",
            post(handlers::generate::generate),
        )
        .route("/v1/passports/:id/attest", post(handlers::generate::attest))
        .route(
            "/v1/passports/:id/generations",
            get(handlers::generate::list_generations),
        )
        .route(
            "/v1/passports/:id/generations/:generation_id/manifest",
            get(handlers::generate::generation_manifest),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
