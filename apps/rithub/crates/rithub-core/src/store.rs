//! Persistence behind a repository-pattern [`Store`] trait, with a Postgres
//! implementation ([`PgStore`]) over `sqlx`.
//!
//! Queries use the runtime (`query`/`query_as`) API with bound parameters — never
//! string-formatted SQL — so there is no SQL-injection surface and no compile-time
//! database connection is required. The trait keeps the store swappable (an
//! in-memory fake for tests, a different backend later) without touching callers.

use crate::domain::{
    Membership, NewOrganization, NewRepository, Organization, Repository, Role, Token, User,
    Visibility,
};
use crate::error::{CoreError, Result};
use sqlx::postgres::PgPool;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// The forge metadata store. Every method maps storage failures onto typed
/// [`CoreError`]s; implementations never leak raw driver errors to callers.
#[allow(async_fn_in_trait)]
pub trait Store {
    /// Create an organization. Conflicts on a duplicate name.
    async fn create_org(&self, input: &NewOrganization) -> Result<Organization>;
    /// List all organizations, newest first.
    async fn list_orgs(&self) -> Result<Vec<Organization>>;
    /// Fetch one organization by name.
    async fn get_org(&self, name: &str) -> Result<Organization>;

    /// Create a repository under an organization. `disk_path` is the resolved
    /// bare-repo path the caller has already created on disk.
    async fn create_repo(
        &self,
        org: &Organization,
        input: &NewRepository,
        disk_path: &str,
    ) -> Result<Repository>;
    /// List repositories in an organization, newest first.
    async fn list_repos(&self, org: &Organization) -> Result<Vec<Repository>>;
    /// Fetch one repository by (org name, repo name).
    async fn get_repo(&self, org_name: &str, repo_name: &str) -> Result<Repository>;

    /// Create a user account.
    async fn create_user(&self, username: &str, email: &str) -> Result<User>;
    /// Fetch one user by username.
    async fn get_user(&self, username: &str) -> Result<User>;
    /// Add or update a user's membership + role in an organization.
    async fn upsert_membership(&self, user_id: Uuid, org_id: Uuid, role: Role)
        -> Result<Membership>;
    /// A user's role in an organization, or `None` if they are not a member.
    async fn get_membership(&self, user_id: Uuid, org_id: Uuid) -> Result<Option<Role>>;

    /// Create a personal access token for a user, storing only its hash.
    async fn create_token(&self, user_id: Uuid, name: &str, token_hash: &str) -> Result<Token>;
    /// Resolve a token hash to its owning user, or `None` if no token matches.
    async fn find_user_by_token(&self, token_hash: &str) -> Result<Option<User>>;
}

/// A Postgres-backed [`Store`].
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Wrap an existing connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the pool (e.g. for the caller to run embedded migrations).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// --- row mapping ------------------------------------------------------------
// Explicit mapping keeps the domain types free of `sqlx::FromRow` derive noise
// and keeps the `Role` text<->enum conversion in one obvious place.

fn org_from_row(row: &sqlx::postgres::PgRow) -> Organization {
    Organization {
        id: row.get("id"),
        name: row.get("name"),
        display_name: row.get("display_name"),
        created_at: row.get::<OffsetDateTime, _>("created_at"),
    }
}

fn repo_from_row(row: &sqlx::postgres::PgRow) -> Repository {
    Repository {
        id: row.get("id"),
        org_id: row.get("org_id"),
        name: row.get("name"),
        description: row.get("description"),
        default_branch: row.get("default_branch"),
        visibility: Visibility::parse(row.get::<String, _>("visibility").as_str()),
        disk_path: row.get("disk_path"),
        created_at: row.get::<OffsetDateTime, _>("created_at"),
    }
}

/// Column list for a repository row (kept in sync with [`repo_from_row`]).
const REPO_COLUMNS: &str =
    "id, org_id, name, description, default_branch, visibility, disk_path, created_at";

impl Store for PgStore {
    async fn create_org(&self, input: &NewOrganization) -> Result<Organization> {
        let row = sqlx::query(
            "INSERT INTO organizations (name, display_name) VALUES ($1, $2) \
             RETURNING id, name, display_name, created_at",
        )
        .bind(&input.name)
        .bind(&input.display_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("organization", &input.name, e))?;
        Ok(org_from_row(&row))
    }

    async fn list_orgs(&self) -> Result<Vec<Organization>> {
        let rows = sqlx::query(
            "SELECT id, name, display_name, created_at FROM organizations ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("organization", "*", e))?;
        Ok(rows.iter().map(org_from_row).collect())
    }

    async fn get_org(&self, name: &str) -> Result<Organization> {
        let row = sqlx::query(
            "SELECT id, name, display_name, created_at FROM organizations WHERE name = $1",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("organization", name, e))?;
        Ok(org_from_row(&row))
    }

    async fn create_repo(
        &self,
        org: &Organization,
        input: &NewRepository,
        disk_path: &str,
    ) -> Result<Repository> {
        let default_branch = input.default_branch.as_deref().unwrap_or("main");
        let visibility = input.visibility.unwrap_or(Visibility::Public);
        let row = sqlx::query(&format!(
            "INSERT INTO repositories (org_id, name, description, default_branch, visibility, disk_path) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {REPO_COLUMNS}"
        ))
        .bind(org.id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(default_branch)
        .bind(visibility.as_str())
        .bind(disk_path)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            CoreError::from_sqlx("repository", &format!("{}/{}", org.name, input.name), e)
        })?;
        Ok(repo_from_row(&row))
    }

    async fn list_repos(&self, org: &Organization) -> Result<Vec<Repository>> {
        let rows = sqlx::query(&format!(
            "SELECT {REPO_COLUMNS} FROM repositories WHERE org_id = $1 ORDER BY created_at DESC"
        ))
        .bind(org.id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("repository", &org.name, e))?;
        Ok(rows.iter().map(repo_from_row).collect())
    }

    async fn get_repo(&self, org_name: &str, repo_name: &str) -> Result<Repository> {
        let key = format!("{org_name}/{repo_name}");
        // `repositories.*` returns only the repository's own columns, so the
        // organization join cannot make `name` ambiguous in the result set.
        let row = sqlx::query(
            "SELECT repositories.* FROM repositories \
             JOIN organizations o ON o.id = repositories.org_id \
             WHERE o.name = $1 AND repositories.name = $2",
        )
        .bind(org_name)
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("repository", &key, e))?;
        Ok(repo_from_row(&row))
    }

    async fn create_user(&self, username: &str, email: &str) -> Result<User> {
        let row = sqlx::query(
            "INSERT INTO users (username, email) VALUES ($1, $2) \
             RETURNING id, username, email, created_at",
        )
        .bind(username)
        .bind(email)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("user", username, e))?;
        Ok(User {
            id: row.get("id"),
            username: row.get("username"),
            email: row.get("email"),
            created_at: row.get::<OffsetDateTime, _>("created_at"),
        })
    }

    async fn upsert_membership(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        role: Role,
    ) -> Result<Membership> {
        let row = sqlx::query(
            "INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, $3) \
             ON CONFLICT (user_id, org_id) DO UPDATE SET role = EXCLUDED.role \
             RETURNING id, user_id, org_id, role",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("membership", &user_id.to_string(), e))?;
        Ok(Membership {
            id: row.get("id"),
            user_id: row.get("user_id"),
            org_id: row.get("org_id"),
            role: Role::parse(row.get::<String, _>("role").as_str()),
        })
    }

    async fn get_user(&self, username: &str) -> Result<User> {
        let row = sqlx::query(
            "SELECT id, username, email, created_at FROM users WHERE username = $1",
        )
        .bind(username)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("user", username, e))?;
        Ok(User {
            id: row.get("id"),
            username: row.get("username"),
            email: row.get("email"),
            created_at: row.get::<OffsetDateTime, _>("created_at"),
        })
    }

    async fn get_membership(&self, user_id: Uuid, org_id: Uuid) -> Result<Option<Role>> {
        let row = sqlx::query("SELECT role FROM memberships WHERE user_id = $1 AND org_id = $2")
            .bind(user_id)
            .bind(org_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| CoreError::from_sqlx("membership", &user_id.to_string(), e))?;
        Ok(row.map(|r| Role::parse(r.get::<String, _>("role").as_str())))
    }

    async fn create_token(&self, user_id: Uuid, name: &str, token_hash: &str) -> Result<Token> {
        let row = sqlx::query(
            "INSERT INTO tokens (user_id, name, token_hash) VALUES ($1, $2, $3) \
             RETURNING id, user_id, name, created_at",
        )
        .bind(user_id)
        .bind(name)
        .bind(token_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("token", name, e))?;
        Ok(Token {
            id: row.get("id"),
            user_id: row.get("user_id"),
            name: row.get("name"),
            created_at: row.get::<OffsetDateTime, _>("created_at"),
        })
    }

    async fn find_user_by_token(&self, token_hash: &str) -> Result<Option<User>> {
        let row = sqlx::query(
            "SELECT u.id, u.username, u.email, u.created_at \
             FROM tokens t JOIN users u ON u.id = t.user_id \
             WHERE t.token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoreError::from_sqlx("token", "*", e))?;
        Ok(row.map(|r| User {
            id: r.get("id"),
            username: r.get("username"),
            email: r.get("email"),
            created_at: r.get::<OffsetDateTime, _>("created_at"),
        }))
    }
}
