//! Public liveness/readiness probes.

use axum::extract::State;
use axum::http::StatusCode;

use crate::error::ApiError;
use crate::state::AppState;

pub async fn healthz() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn readyz(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    state.service.ready().await?;
    Ok(StatusCode::NO_CONTENT)
}
