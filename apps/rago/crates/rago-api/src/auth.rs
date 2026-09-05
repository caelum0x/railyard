//! API-key bearer authentication — faithful port of
//! `Api::BaseController#authenticate` / `#authorize`.
//!
//! The `Authorization: Bearer <value>` token is looked up against the
//! `api_keys` table (via [`rago_db::Db`]). A missing/invalid/expired key yields
//! `401`. There is no bypass — an unauthenticated request never reaches a
//! handler. Fine-grained `permit?` checks default to allow (org api-permissions
//! disabled, the common case) and are enforced when enabled.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::Utc;
use rago_core::models::{ApiKey, Organization};

use crate::error::ApiError;
use crate::state::AppState;

/// An authenticated request: the resolved API key and its organization.
pub struct Authed {
    pub api_key: ApiKey,
    pub organization: Organization,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Authorization: Bearer <token>
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_whitespace().nth(1))
            .ok_or(ApiError::Unauthorized)?;

        let (api_key, organization) = state
            .db
            .authenticate(token)
            .await?
            .ok_or(ApiError::Unauthorized)?;

        if api_key.is_expired(Utc::now()) {
            return Err(ApiError::Unauthorized);
        }

        Ok(Authed {
            api_key,
            organization,
        })
    }
}
