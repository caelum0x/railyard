//! OAuth2 / SSO external identities: domain model + Postgres-backed store (a
//! focused [`OauthStore`] trait implemented for [`crate::PgStore`]).
//!
//! An [`OauthIdentity`] links a forge [`User`] to an account at an external
//! identity provider (GitHub, Google, ...). The `(provider, provider_user_id)`
//! pair is the provider's stable subject and is unique per provider, so a
//! returning user is matched to the same forge account on every login. This
//! module owns **no** provider HTTP concerns — the authorization-code flow and
//! the provider abstraction live in `rithub-server`; here we only persist the
//! link and expose the lookups the login callback needs.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use crate::User;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A link between a forge user and an external identity-provider account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OauthIdentity {
    pub id: Uuid,
    pub user_id: Uuid,
    /// Provider slug, e.g. `"github"` or `"google"`.
    pub provider: String,
    /// The provider's stable, opaque subject id for this account.
    pub provider_user_id: String,
    /// The email the provider reported when the link was created, if any.
    pub email: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Persistence + lookup for external OAuth identities and the user records the
/// login callback resolves them against.
#[allow(async_fn_in_trait)]
pub trait OauthStore {
    /// Every external identity linked to a user, oldest first.
    async fn list_oauth_identities(&self, user_id: Uuid) -> Result<Vec<OauthIdentity>>;
    /// Resolve a provider account `(provider, provider_user_id)` to its linked
    /// identity, or `None` if that account has never signed in here.
    async fn find_oauth_identity(
        &self,
        provider: &str,
        provider_user_id: &str,
    ) -> Result<Option<OauthIdentity>>;

    /// Link a provider account to a forge user. Conflicts if the account is
    /// already linked (the `(provider, provider_user_id)` uniqueness).
    async fn create_oauth_identity(
        &self,
        user_id: Uuid,
        provider: &str,
        provider_user_id: &str,
        email: Option<&str>,
    ) -> Result<OauthIdentity>;

    /// Fetch one user by id, or `None` if no such user exists.
    async fn get_user_by_id(&self, id: Uuid) -> Result<Option<User>>;

    /// Fetch one user by email (case-sensitive, as stored), or `None`.
    /// Used to attach a first OAuth login to a pre-existing account.
    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>>;
}

// --- row mapping ------------------------------------------------------------

fn identity_from_row(row: &sqlx::postgres::PgRow) -> OauthIdentity {
    OauthIdentity {
        id: row.get("id"),
        user_id: row.get("user_id"),
        provider: row.get("provider"),
        provider_user_id: row.get("provider_user_id"),
        email: row.get("email"),
        created_at: row.get::<OffsetDateTime, _>("created_at"),
    }
}

fn user_from_row(row: &sqlx::postgres::PgRow) -> User {
    User {
        id: row.get("id"),
        username: row.get("username"),
        email: row.get("email"),
        created_at: row.get::<OffsetDateTime, _>("created_at"),
    }
}

const IDENTITY_COLUMNS: &str = "id, user_id, provider, provider_user_id, email, created_at";

// --- PgStore impl -----------------------------------------------------------

impl OauthStore for PgStore {
    async fn list_oauth_identities(&self, user_id: Uuid) -> Result<Vec<OauthIdentity>> {
        let rows = sqlx::query(&format!(
            "SELECT {IDENTITY_COLUMNS} FROM oauth_identities WHERE user_id = $1 ORDER BY created_at",
        ))
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("oauth_identity", "*", e))?;
        Ok(rows.iter().map(identity_from_row).collect())
    }

    async fn find_oauth_identity(
        &self,
        provider: &str,
        provider_user_id: &str,
    ) -> Result<Option<OauthIdentity>> {
        let row = sqlx::query(&format!(
            "SELECT {IDENTITY_COLUMNS} FROM oauth_identities \
             WHERE provider = $1 AND provider_user_id = $2",
        ))
        .bind(provider)
        .bind(provider_user_id)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("oauth_identity", provider, e))?;
        Ok(row.as_ref().map(identity_from_row))
    }

    async fn create_oauth_identity(
        &self,
        user_id: Uuid,
        provider: &str,
        provider_user_id: &str,
        email: Option<&str>,
    ) -> Result<OauthIdentity> {
        let row = sqlx::query(&format!(
            "INSERT INTO oauth_identities (user_id, provider, provider_user_id, email) \
             VALUES ($1, $2, $3, $4) \
             RETURNING {IDENTITY_COLUMNS}",
        ))
        .bind(user_id)
        .bind(provider)
        .bind(provider_user_id)
        .bind(email)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("oauth_identity", provider, e))?;
        Ok(identity_from_row(&row))
    }

    async fn get_user_by_id(&self, id: Uuid) -> Result<Option<User>> {
        let row = sqlx::query("SELECT id, username, email, created_at FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("user", &id.to_string(), e))?;
        Ok(row.as_ref().map(user_from_row))
    }

    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let row = sqlx::query("SELECT id, username, email, created_at FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("user", email, e))?;
        Ok(row.as_ref().map(user_from_row))
    }
}
