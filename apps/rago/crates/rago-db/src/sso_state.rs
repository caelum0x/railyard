//! Single-use SSO authorization state (login-CSRF protection) with the PKCE
//! code verifier bound to it.

use chrono::NaiveDateTime;
use sqlx::Row;

use crate::error::Result;
use crate::repo::Db;

impl Db {
    /// Store a freshly minted SSO `state` for `provider`.
    pub async fn insert_sso_state(
        &self,
        state: &str,
        provider: &str,
        code_verifier: &str,
        expires_at: NaiveDateTime,
    ) -> Result<()> {
        // Opportunistic cleanup of long-expired states.
        sqlx::query("DELETE FROM sso_states WHERE expires_at < now() - interval '1 day'")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO sso_states (state, provider, code_verifier, expires_at, created_at) \
             VALUES ($1, $2, $3, $4, now())",
        )
        .bind(state)
        .bind(provider)
        .bind(code_verifier)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically consume an unexpired, unused `state` issued for `provider`,
    /// returning its PKCE code verifier. `None` when the state is unknown,
    /// expired, already used, or was issued for another provider.
    pub async fn consume_sso_state(&self, state: &str, provider: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "UPDATE sso_states SET consumed_at = now() \
             WHERE state = $1 AND provider = $2 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING code_verifier",
        )
        .bind(state)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("code_verifier")))
    }
}
