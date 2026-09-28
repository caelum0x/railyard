//! # switchyard-store
//!
//! One [`Store`] trait, two implementations:
//! - [`MemStore`]: in-memory, used by tests and `SWITCHYARD_STORE=memory` dev runs;
//! - [`PgStore`]: Postgres through sqlx with embedded migrations (`/migrations`).
//!
//! The call audit log is append-only and hash-chained per project. Recording a
//! call, its outbox events (Rago usage, stats) and its idempotency entry is one
//! atomic operation, so a crash can never keep a capture and lose its receipt.

pub mod mem;
pub mod models;
pub mod pg;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub use mem::MemStore;
pub use models::*;
pub use pg::PgStore;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database error: {0}")]
    Db(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => StoreError::NotFound("row".into()),
            sqlx::Error::Database(d) if d.is_unique_violation() => StoreError::Conflict(d.message().to_string()),
            other => StoreError::Db(other.to_string()),
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[async_trait]
pub trait Store: Send + Sync {
    // ---- orgs, projects, members ----
    async fn create_org(&self, org: Org) -> StoreResult<Org>;
    async fn get_org(&self, id: &str) -> StoreResult<Org>;
    async fn list_orgs(&self) -> StoreResult<Vec<Org>>;
    async fn create_project(&self, p: Project) -> StoreResult<Project>;
    async fn get_project(&self, id: &str) -> StoreResult<Project>;
    async fn update_project(&self, p: Project) -> StoreResult<Project>;
    async fn list_projects(&self, org_id: &str) -> StoreResult<Vec<Project>>;
    async fn upsert_member(&self, m: Member) -> StoreResult<()>;
    async fn get_member(&self, org_id: &str, user_id: &str) -> StoreResult<Option<Member>>;

    // ---- api keys (scopes/project binding for Relay keys) ----
    async fn insert_api_key(&self, k: ApiKey) -> StoreResult<ApiKey>;
    async fn get_api_key(&self, public_id: &str) -> StoreResult<Option<ApiKey>>;
    async fn list_api_keys(&self, project_id: &str) -> StoreResult<Vec<ApiKey>>;
    async fn update_api_key(&self, k: ApiKey) -> StoreResult<ApiKey>;

    // ---- vault ----
    /// Insert the org's wrapped DEK unless one exists; returns the stored one.
    async fn ensure_org_key(&self, k: OrgKey) -> StoreResult<OrgKey>;
    async fn get_org_key(&self, org_id: &str) -> StoreResult<Option<OrgKey>>;
    async fn list_org_keys(&self) -> StoreResult<Vec<OrgKey>>;
    async fn update_org_key(&self, k: OrgKey) -> StoreResult<()>;
    async fn insert_credential(&self, c: CredentialRow) -> StoreResult<CredentialRow>;
    async fn get_credential(&self, org_id: &str, id: &str) -> StoreResult<Option<CredentialRow>>;
    async fn list_credentials(&self, org_id: &str) -> StoreResult<Vec<CredentialRow>>;
    /// The newest credential for a provider.
    async fn find_credential(&self, org_id: &str, provider_id: &str) -> StoreResult<Option<CredentialRow>>;
    async fn delete_credential(&self, org_id: &str, id: &str) -> StoreResult<bool>;
    async fn mark_credential(&self, id: &str, ok: bool, error: Option<String>) -> StoreResult<()>;

    // ---- bundles ----
    async fn upsert_bundle(&self, b: BundleRow) -> StoreResult<BundleRow>;
    async fn list_bundles(&self, org_id: &str) -> StoreResult<Vec<BundleRow>>;
    async fn get_bundle(&self, org_id: &str, id: &str) -> StoreResult<Option<BundleRow>>;

    // ---- BYO tools ----
    async fn upsert_byo_tool(&self, t: ByoTool) -> StoreResult<ByoTool>;
    async fn get_byo_tool(&self, org_id: &str, id: &str) -> StoreResult<Option<ByoTool>>;
    async fn list_byo_tools(&self, org_id: Option<&str>) -> StoreResult<Vec<ByoTool>>;

    // ---- calls, idempotency, outbox (one atomic write) ----
    async fn record_call(&self, call: NewCall, outbox: Vec<OutboxItem>, idem: Option<IdempotencyRow>) -> StoreResult<CallRow>;
    async fn get_call(&self, project_id: &str, id: &str) -> StoreResult<Option<CallRow>>;
    /// Newest first; `before` is a call id cursor.
    async fn list_calls(&self, project_id: &str, before: Option<&str>, limit: i64) -> StoreResult<Vec<CallRow>>;
    /// Oldest first, for chain verification up to and including `upto_seq`.
    async fn chain(&self, project_id: &str, upto_seq: i64) -> StoreResult<Vec<CallRow>>;
    async fn spend_since(&self, filter: SpendFilter) -> StoreResult<i64>;
    async fn usage(&self, project_id: &str, from: DateTime<Utc>, to: DateTime<Utc>, group_by: GroupBy) -> StoreResult<Vec<UsageRow>>;
    async fn get_idempotency(&self, project_id: &str, key: &str, now: DateTime<Utc>) -> StoreResult<Option<IdempotencyRow>>;
    async fn due_outbox(&self, now: DateTime<Utc>, limit: i64) -> StoreResult<Vec<OutboxItem>>;
    async fn mark_outbox(&self, id: &str, delivered: bool, error: Option<String>, next_attempt_at: DateTime<Utc>) -> StoreResult<()>;
    async fn record_stat(&self, tool_id: &str, success: bool, latency_ms: i64, cost_micros: i64) -> StoreResult<()>;
    async fn stats(&self) -> StoreResult<Vec<ToolStat>>;

    // ---- OAuth 2.1 ----
    async fn insert_client(&self, c: OAuthClient) -> StoreResult<OAuthClient>;
    async fn get_client(&self, client_id: &str) -> StoreResult<Option<OAuthClient>>;
    async fn insert_code(&self, c: OAuthCode) -> StoreResult<()>;
    /// Single use: returns and deletes.
    async fn take_code(&self, code_hash: &str) -> StoreResult<Option<OAuthCode>>;
    async fn insert_grant(&self, g: OAuthGrant) -> StoreResult<()>;
    async fn find_grant_by_refresh(&self, refresh_hash: &str) -> StoreResult<Option<OAuthGrant>>;
    /// Mark a refresh token used; returns false if it was already used (replay).
    async fn consume_grant(&self, id: &str) -> StoreResult<bool>;
    async fn revoke_family(&self, family_id: &str) -> StoreResult<()>;
    async fn family_revoked(&self, family_id: &str) -> StoreResult<bool>;
    async fn add_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<()>;
    async fn has_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<bool>;
    async fn insert_device(&self, d: DeviceCode) -> StoreResult<()>;
    async fn get_device_by_user_code(&self, user_code: &str) -> StoreResult<Option<DeviceCode>>;
    async fn get_device(&self, device_hash: &str) -> StoreResult<Option<DeviceCode>>;
    async fn update_device(&self, d: DeviceCode) -> StoreResult<()>;
}
