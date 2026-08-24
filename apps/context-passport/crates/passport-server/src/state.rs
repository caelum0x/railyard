//! Shared application state injected into every handler.

use std::sync::Arc;

use crate::service::PassportService;

#[derive(Clone)]
pub struct AppState {
    pub service: PassportService,
    pub api_key: Arc<str>,
}

/// Uniform success envelope: `{ "data": ... }`.
#[derive(serde::Serialize)]
pub struct Data<T> {
    pub data: T,
}
