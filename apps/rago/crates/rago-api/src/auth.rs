//! Bearer authentication — faithful port of
//! `Api::BaseController#authenticate` / `#authorize`, plus dashboard sessions.
//!
//! The `Authorization: Bearer <value>` token is looked up against the
//! `api_keys` table (via [`rago_db::Db`]). When it is not an API key it may be
//! a dashboard session JWT (password or SSO login, signed with
//! `RAGO_JWT_SECRET`): the user must still be an active member of the
//! organization the session resolves to (an SSO session is pinned to the
//! organization whose IdP authenticated it). A missing/invalid/expired token
//! yields `401`. There is no bypass — an unauthenticated request never reaches
//! a handler. Fine-grained `permit?` checks default to allow (org
//! api-permissions disabled, the common case) and are enforced when enabled.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::Utc;
use rago_core::models::{ApiKey, Organization};
use serde_json::json;

use crate::error::ApiError;
use crate::state::AppState;

/// An authenticated request: the resolved API key and its organization.
///
/// For a dashboard session the key is synthesized (`id` = the user id, `name`
/// = `session:<email>`, empty `value`) so handlers stay credential-agnostic.
pub struct Authed {
    pub api_key: ApiKey,
    pub organization: Organization,
}

/// Resolve a bearer token (API key or session JWT) to its key + organization.
/// `Ok(None)` when the token is unknown, expired, or its session no longer
/// maps to an active membership.
pub async fn resolve_bearer(state: &AppState, token: &str) -> Result<Option<(ApiKey, Organization)>, ApiError> {
    if let Some((api_key, organization)) = state.db.authenticate(token).await? {
        if api_key.is_expired(Utc::now()) {
            return Ok(None);
        }
        return Ok(Some((api_key, organization)));
    }
    let Some((user_id, pinned_org)) = rago_graphql::auth::verify_jwt_claims(token) else {
        return Ok(None);
    };
    let session = match pinned_org {
        Some(org) => state.db.resolve_member_session(user_id, org).await?,
        None => state.db.resolve_user_session(user_id).await?,
    };
    let Some((organization, email)) = session else {
        return Ok(None);
    };
    let now = Utc::now();
    let api_key = ApiKey {
        id: user_id,
        organization_id: organization.id,
        value: String::new(),
        name: Some(format!("session:{email}")),
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    };
    Ok(Some((api_key, organization)))
}

/// The bearer token of a request, if any.
pub fn bearer_token(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_whitespace().nth(1))
}

#[axum::async_trait]
impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Authorization: Bearer <token>
        let token = bearer_token(parts).ok_or(ApiError::Unauthorized)?.to_string();
        let (api_key, organization) = resolve_bearer(state, &token).await?.ok_or(ApiError::Unauthorized)?;
        Ok(Authed {
            api_key,
            organization,
        })
    }
}
