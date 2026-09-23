//! Sealed provider API keys managed from the Relay UI.
//!
//! This adapter stores and returns ciphertext only; sealing and opening happen
//! in the gateway, which owns the `RELAY_CREDENTIALS_KEY` master key.

use serde::Serialize;
use sqlx::{PgPool, Row};

/// One sealed credential row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedProviderCredential {
    pub credential_name: String,
    pub provider: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub key_hint: String,
    pub updated_by: String,
    pub updated_at_ms: i64,
    pub test: Option<ProviderCredentialTest>,
}

/// Outcome of the most recent connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCredentialTest {
    pub ok: bool,
    pub message: String,
    pub tested_at_ms: i64,
}

#[derive(Clone)]
pub struct PostgresProviderCredentialStore {
    pool: PgPool,
}

impl PostgresProviderCredentialStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<SealedProviderCredential>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT credential_name, provider, nonce, ciphertext, key_hint, updated_by, \
             (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_at_ms, \
             (EXTRACT(EPOCH FROM last_tested_at) * 1000)::BIGINT AS tested_at_ms, \
             last_test_ok, last_test_message \
             FROM relay_provider_credentials ORDER BY credential_name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let tested_at_ms: Option<i64> = row.try_get("tested_at_ms")?;
                let ok: Option<bool> = row.try_get("last_test_ok")?;
                let message: Option<String> = row.try_get("last_test_message")?;
                Ok(SealedProviderCredential {
                    credential_name: row.try_get("credential_name")?,
                    provider: row.try_get("provider")?,
                    nonce: row.try_get("nonce")?,
                    ciphertext: row.try_get("ciphertext")?,
                    key_hint: row.try_get("key_hint")?,
                    updated_by: row.try_get("updated_by")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                    test: match (tested_at_ms, ok) {
                        (Some(tested_at_ms), Some(ok)) => Some(ProviderCredentialTest {
                            ok,
                            message: message.unwrap_or_default(),
                            tested_at_ms,
                        }),
                        _ => None,
                    },
                })
            })
            .collect()
    }

    /// Create or rotate a sealed credential. Rotation clears the previous
    /// connection-test result, which described the old key.
    pub async fn upsert(
        &self,
        credential_name: &str,
        provider: &str,
        nonce: &[u8],
        ciphertext: &[u8],
        key_hint: &str,
        updated_by: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO relay_provider_credentials \
             (credential_name, provider, nonce, ciphertext, key_hint, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (credential_name) DO UPDATE SET \
             provider = EXCLUDED.provider, nonce = EXCLUDED.nonce, \
             ciphertext = EXCLUDED.ciphertext, key_hint = EXCLUDED.key_hint, \
             updated_by = EXCLUDED.updated_by, updated_at = NOW(), \
             last_tested_at = NULL, last_test_ok = NULL, last_test_message = NULL",
        )
        .bind(credential_name)
        .bind(provider)
        .bind(nonce)
        .bind(ciphertext)
        .bind(key_hint)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, credential_name: &str) -> Result<bool, sqlx::Error> {
        let result =
            sqlx::query("DELETE FROM relay_provider_credentials WHERE credential_name = $1")
                .bind(credential_name)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn record_test(
        &self,
        credential_name: &str,
        ok: bool,
        message: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE relay_provider_credentials SET last_tested_at = NOW(), \
             last_test_ok = $2, last_test_message = $3 WHERE credential_name = $1",
        )
        .bind(credential_name)
        .bind(ok)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
