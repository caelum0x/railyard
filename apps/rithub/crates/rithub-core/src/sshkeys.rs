//! SSH public-key registration + lookup. The SSH transport authenticates a
//! connection by matching the offered public key's base64 blob against a
//! registered key, resolving it to the owning user.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use crate::User;
use sqlx::Row;
use uuid::Uuid;

/// SSH key persistence.
#[allow(async_fn_in_trait)]
pub trait SshKeyStore {
    /// Register a public key (base64 blob) for a user.
    async fn add_ssh_key(&self, user_id: Uuid, title: &str, key_blob: &str) -> Result<()>;
    /// Resolve a public-key blob to its owning user, if registered.
    async fn find_user_by_ssh_key(&self, key_blob: &str) -> Result<Option<User>>;
    /// List a user's registered key blobs.
    async fn list_ssh_keys(&self, user_id: Uuid) -> Result<Vec<String>>;
}

impl SshKeyStore for PgStore {
    async fn add_ssh_key(&self, user_id: Uuid, title: &str, key_blob: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO ssh_keys (user_id, title, key_blob) VALUES ($1, $2, $3) \
             ON CONFLICT (key_blob) DO NOTHING",
        )
        .bind(user_id)
        .bind(title)
        .bind(key_blob)
        .execute(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("ssh_key", title, e))?;
        Ok(())
    }

    async fn find_user_by_ssh_key(&self, key_blob: &str) -> Result<Option<User>> {
        let row = sqlx::query(
            "SELECT u.id, u.username, u.email, u.created_at FROM ssh_keys k \
             JOIN users u ON u.id = k.user_id WHERE k.key_blob = $1",
        )
        .bind(key_blob)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("ssh_key", "*", e))?;
        Ok(row.map(|r| User {
            id: r.get("id"),
            username: r.get("username"),
            email: r.get("email"),
            created_at: r.get("created_at"),
        }))
    }

    async fn list_ssh_keys(&self, user_id: Uuid) -> Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT key_blob FROM ssh_keys WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("ssh_key", "*", e))?;
        Ok(rows)
    }
}
