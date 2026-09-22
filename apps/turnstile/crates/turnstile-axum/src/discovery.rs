//! Discovery endpoint: serve the provider's paid-tool catalog.
//!
//! A calling agent should be able to learn what a Turnstile deployment charges
//! for — before it spends anything. This handler serves the shared
//! [`ToolCatalog`] (payee, currency, protocol version, and every priced tool)
//! that a provider builds from its [`ToolRegistry`](turnstile_core::ToolRegistry).
//!
//! Wire it as an OPEN route (never gated — discovery is free) whose state is the
//! catalog handle:
//!
//! ```ignore
//! use std::sync::Arc;
//! use axum::{routing::get, Router};
//! use turnstile_axum::discover;
//! use turnstile_core::ToolCatalog;
//!
//! let catalog: Arc<ToolCatalog> = Arc::new(registry.catalog());
//! let app = Router::new()
//!     .route("/.well-known/aitp-tools", get(discover))
//!     .with_state(catalog);
//! ```

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use turnstile_core::ToolCatalog;

/// `GET /…` — return the provider's paid-tool catalog as JSON.
///
/// Always `200` with the [`ToolCatalog`]; the catalog is static per deployment
/// so there is no failure path. Discovery is free and unauthenticated.
pub async fn discover(State(catalog): State<Arc<ToolCatalog>>) -> Response {
    Json(catalog.as_ref()).into_response()
}
