//! Single-use OAuth authorization state (login-CSRF protection) with the PKCE
//! code verifier bound to it.
//!
//! One table serves every server-driven OAuth redirect: SSO logins
//! (`purpose = 'sso_login'`) and integration connections
//! (`purpose = 'integration_oauth'`). A state is bound to the organization (and
//! integration) it was minted for, so a callback can never be replayed into
//! another tenant.

use chrono::NaiveDateTime;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// State purpose: an SSO login.
pub const PURPOSE_SSO_LOGIN: &str = "sso_login";
/// State purpose: connecting an integration (accounting / CRM OAuth app).
pub const PURPOSE_INTEGRATION_OAUTH: &str = "integration_oauth";

/// A consumed OAuth state.
#[derive(Debug, Clone)]
pub struct OAuthStateRow {
    pub provider: String,
    pub code_verifier: String,
    pub organization_id: Option<Uuid>,
    pub integration_id: Option<Uuid>,
}

/// A state to mint.
#[derive(Debug, Clone)]
pub struct NewOAuthState<'a> {
    pub state: &'a str,
    pub purpose: &'a str,
    pub provider: &'a str,
    pub code_verifier: &'a str,
    pub organization_id: Option<Uuid>,
    pub integration_id: Option<Uuid>,
    pub expires_at: NaiveDateTime,
}

impl Db {
    /// Store a freshly minted SSO `state` for `provider` (not bound to an
    /// organization — single-tenant IdP configured through the environment).
    pub async fn insert_sso_state(
        &self,
        state: &str,
        provider: &str,
        code_verifier: &str,
        expires_at: NaiveDateTime,
    ) -> Result<()> {
        self.insert_oauth_state(&NewOAuthState {
            state,
            purpose: PURPOSE_SSO_LOGIN,
            provider,
            code_verifier,
            organization_id: None,
            integration_id: None,
            expires_at,
        })
        .await
    }

    /// Store a freshly minted OAuth state (any purpose).
    pub async fn insert_oauth_state(&self, s: &NewOAuthState<'_>) -> Result<()> {
        // Opportunistic cleanup of long-expired states.
        sqlx::query("DELETE FROM sso_states WHERE expires_at < now() - interval '1 day'")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO sso_states (state, purpose, provider, code_verifier, organization_id, integration_id, expires_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())",
        )
        .bind(s.state)
        .bind(s.purpose)
        .bind(s.provider)
        .bind(s.code_verifier)
        .bind(s.organization_id)
        .bind(s.integration_id)
        .bind(s.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically consume an unexpired, unused `state` issued for `provider`,
    /// returning its PKCE code verifier. `None` when the state is unknown,
    /// expired, already used, or was issued for another provider.
    pub async fn consume_sso_state(&self, state: &str, provider: &str) -> Result<Option<String>> {
        Ok(self
            .consume_oauth_state(state, PURPOSE_SSO_LOGIN)
            .await?
            .filter(|s| s.provider == provider)
            .map(|s| s.code_verifier))
    }

    /// Atomically consume an unexpired, unused `state` minted for `purpose`.
    /// `None` when the state is unknown, expired, already used, or was minted
    /// for another purpose.
    pub async fn consume_oauth_state(&self, state: &str, purpose: &str) -> Result<Option<OAuthStateRow>> {
        let row = sqlx::query(
            "UPDATE sso_states SET consumed_at = now() \
             WHERE state = $1 AND purpose = $2 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING provider, code_verifier, organization_id, integration_id",
        )
        .bind(state)
        .bind(purpose)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| OAuthStateRow {
            provider: r.get("provider"),
            code_verifier: r.get("code_verifier"),
            organization_id: r.get("organization_id"),
            integration_id: r.get("integration_id"),
        }))
    }
}
