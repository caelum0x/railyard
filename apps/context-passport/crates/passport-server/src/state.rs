//! Shared application state injected into every handler.

use std::sync::Arc;

use crate::service::PassportService;

#[derive(Clone)]
pub struct AppState {
    pub service: PassportService,
    /// Operator key (full access).
    pub api_key: Arc<str>,
    /// Per-owner tenant keys.
    pub owner_keys: Arc<crate::auth::OwnerKeys>,
}

/// Uniform success envelope: `{ "data": ... }`.
#[derive(serde::Serialize)]
pub struct Data<T> {
    pub data: T,
}
