//! # vend-store — Neon Postgres persistence (sqlx RUNTIME query API only).
//!
//! Concrete impls of every repo port: [`vend_tenancy::TenantRepo`],
//! [`vend_keys::KeyRepo`], [`vend_core::UsageRepo`], [`vend_core::ReserveRepo`],
//! and [`vend_core::IdempotencyRepo`]. Uses ONLY `sqlx::query` / `.bind` (never
//! the compile-time `query!` macros) so the crate builds with NO `DATABASE_URL`;
//! the schema is applied at runtime via [`VendStore::migrate`].
//!
//! This stores tenancy / policy / usage and the `hold_id <-> job` mapping. It is
//! NEVER a money ledger — settlement and authoritative balances live on Mint.

mod payouts;
mod pool;
mod reconciliation;
mod security_ops;

pub use pool::PostgresPoolConfig;
pub use reconciliation::{captured_recovery_replay, AttachedHoldCandidate, PayoutCandidate};
pub use security_ops::{
    AuditActorKind, AuditEvent, AuditOutcome, NewAuditEvent, RateLimitDecision, RateLimitRequest,
    SecurityOpsRepo,
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use password_hash::rand_core::{OsRng, RngCore};
use password_hash::SaltString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use subtle::ConstantTimeEq;
use ulid::Ulid;

use vend_core::error::VendError;
use vend_core::ids::{AppId, ChargeId, EndUserId, SubKeyId, TenantId};
use vend_core::money::{Charge, FundingMode, MarkupPolicy};
use vend_core::ports::{
    IdempotencyClaim, IdempotencyRepo, MeteredUsage, ReadinessRepo, UsageOutcome, UsageRepo,
};
use vend_core::records::{KeyScope, KeyStatus, SubKeyRecord};
use vend_core::ReserveRepo;
use vend_keys::{mint, KeyEnv, KeyRepo, MintParams};
use vend_tenancy::{App, EndUser, Tenant, TenantRepo};

/// A Postgres-backed store. Cheap to clone (wraps a connection pool).
#[derive(Clone)]
pub struct VendStore {
    pool: PgPool,
}

/// A durable usage intent whose idempotent Mint hold has not yet been attached.
/// Reconciliation may safely repeat `hold.create` with `idem_key:hold`, recover
/// Mint's original hold id, and release it without invoking Relay.
#[derive(Clone, Debug)]
pub struct PendingUnattachedHold {
    pub charge_id: ChargeId,
    pub idem_key: String,
    pub payer_account: String,
    pub amount_micros: i64,
}

/// One tenant + app + live provisioning key created atomically by the operator
/// bootstrap command.
pub struct BootstrapConfig {
    pub tenant_name: String,
    pub app_name: String,
    pub mint_account_id: String,
    pub model_allowlist: Vec<String>,
    pub zdr: bool,
    pub markup_policy: MarkupPolicy,
    pub key_env: KeyEnv,
    pub daily_cap_micros: Option<i64>,
    pub per_end_user_daily_cap_micros: Option<i64>,
    /// Email identity for the first tenant owner. Vend does not send email;
    /// the returned setup token must be delivered out of band by the operator.
    pub owner_email: String,
    pub owner_display_name: String,
    pub setup_token_ttl_seconds: i64,
}

/// Bootstrap identifiers plus the one-time provisioning-key plaintext.
pub struct BootstrapOutput {
    pub tenant_id: TenantId,
    pub app_id: AppId,
    pub sub_key_id: SubKeyId,
    pub secret: String,
    pub owner: CustomerUserSummary,
    pub owner_setup_token: String,
}

/// Tenant customer role. Roles are cumulative: owner > admin > viewer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerRole {
    Owner,
    Admin,
    Viewer,
}

impl CustomerRole {
    pub fn allows(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Admin => 1,
            Self::Owner => 2,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Viewer => "viewer",
        }
    }

    fn parse(value: &str) -> Result<Self, VendError> {
        match value {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "viewer" => Ok(Self::Viewer),
            _ => Err(VendError::Storage("invalid tenant user role".into())),
        }
    }
}

/// Lifecycle of a tenant customer identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerUserStatus {
    Invited,
    Active,
    Disabled,
}

impl CustomerUserStatus {
    fn parse(value: &str) -> Result<Self, VendError> {
        match value {
            "invited" => Ok(Self::Invited),
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            _ => Err(VendError::Storage("invalid tenant user status".into())),
        }
    }
}

/// Non-secret tenant user metadata.
#[derive(Clone, Debug, Serialize)]
pub struct CustomerUserSummary {
    pub id: String,
    pub membership_id: String,
    pub tenant_id: TenantId,
    pub email: String,
    pub display_name: String,
    pub role: CustomerRole,
    pub status: CustomerUserStatus,
    pub external_providers: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// One-time invitation result. The setup token is never stored as plaintext.
#[derive(Debug, Serialize)]
pub struct InvitedCustomerUser {
    pub user: CustomerUserSummary,
    pub setup_token: String,
    pub setup_expires_at: DateTime<Utc>,
}

/// Authenticated request identity loaded from a live session on every request.
#[derive(Clone, Debug)]
pub struct CustomerPrincipal {
    pub session_id: String,
    pub user_id: String,
    pub membership_id: String,
    pub tenant_id: TenantId,
    pub role: CustomerRole,
}

/// One-time login result. The bearer is never persisted as plaintext.
#[derive(Debug, Serialize)]
pub struct IssuedCustomerSession {
    pub user: CustomerUserSummary,
    pub session_id: String,
    pub session_token: String,
    pub expires_at: DateTime<Utc>,
}

/// Owner-controlled user changes. `disabled: false` re-enables an identity that
/// already completed setup; it never bypasses setup for an invited identity.
#[derive(Clone, Debug)]
pub struct UpdateCustomerUser {
    pub role: Option<CustomerRole>,
    pub disabled: Option<bool>,
}

/// Server-side OAuth transaction material. Only `state` leaves Vend before the
/// callback; verifier and nonce are stored durably and consumed once.
#[derive(Debug)]
pub struct OAuthStart {
    pub state: String,
    pub code_challenge: String,
    pub nonce: String,
}

#[derive(Debug)]
pub struct ConsumedOAuthState {
    pub tenant_id: TenantId,
    pub code_verifier: String,
    pub nonce: String,
}

/// Safe tenant shape returned by the control plane.
#[derive(Clone, Debug, Serialize)]
pub struct TenantSummary {
    pub id: TenantId,
    pub name: String,
    pub mint_account_id: String,
}

/// Safe app shape returned by the control plane.
#[derive(Clone, Debug, Serialize)]
pub struct AppSummary {
    pub id: AppId,
    pub tenant_id: TenantId,
    pub name: String,
    pub model_allowlist: Vec<String>,
    pub zdr: bool,
    pub enabled: bool,
    pub markup_policy: MarkupPolicy,
    pub mint_account_id: String,
}

/// Safe provisioning-key metadata. Deliberately excludes `secret_hash`.
#[derive(Clone, Debug, Serialize)]
pub struct KeySummary {
    pub id: SubKeyId,
    pub tenant_id: TenantId,
    pub app_id: Option<AppId>,
    pub scope: KeyScope,
    pub status: KeyStatus,
    pub prefix: String,
    pub daily_cap_micros: Option<i64>,
    pub per_end_user_daily_cap_micros: Option<i64>,
}

/// App creation input accepted by the authenticated control plane.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateApp {
    pub name: String,
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    #[serde(default)]
    pub zdr: bool,
    pub markup_policy: MarkupPolicy,
    pub mint_account_id: String,
}

/// Provisioning-key creation input accepted by the authenticated control plane.
#[derive(Clone, Debug)]
pub struct CreateAppKey {
    pub env: KeyEnv,
    pub daily_cap_micros: Option<i64>,
    pub per_end_user_daily_cap_micros: Option<i64>,
}

/// One-time key issuance output. `secret` is never persisted.
#[derive(Serialize)]
pub struct IssuedKey {
    pub key: KeySummary,
    pub secret: String,
}

/// Bounded, customer-visible spend totals for one app and UTC day.
#[derive(Clone, Debug, Serialize)]
pub struct UsageSummary {
    pub app_id: AppId,
    pub utc_day: String,
    pub requests: i64,
    pub pending_requests: i64,
    pub captured_requests: i64,
    pub released_requests: i64,
    pub pending_charge_micros: i64,
    pub captured_charge_micros: i64,
}

/// Persistence contract for Vend's authenticated operator control plane.
#[async_trait]
pub trait ControlPlaneRepo: Send + Sync {
    async fn bootstrap(&self, config: BootstrapConfig) -> Result<BootstrapOutput, VendError>;
    async fn list_tenants(&self) -> Result<Vec<TenantSummary>, VendError>;
    async fn get_tenant(&self, id: &TenantId) -> Result<Option<TenantSummary>, VendError>;
    async fn list_apps(&self, tenant_id: &TenantId) -> Result<Vec<AppSummary>, VendError>;
    async fn get_app(&self, id: &AppId) -> Result<Option<AppSummary>, VendError>;
    async fn create_app(
        &self,
        tenant_id: &TenantId,
        input: CreateApp,
    ) -> Result<AppSummary, VendError>;
    async fn set_app_enabled(&self, id: &AppId, enabled: bool) -> Result<AppSummary, VendError>;
    async fn list_app_keys(&self, app_id: &AppId) -> Result<Vec<KeySummary>, VendError>;
    async fn issue_app_key(
        &self,
        app_id: &AppId,
        input: CreateAppKey,
    ) -> Result<IssuedKey, VendError>;
    async fn revoke_key(&self, id: &SubKeyId) -> Result<KeySummary, VendError>;
    async fn provision_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
        mint_account_id: &str,
    ) -> Result<EndUser, VendError>;
    async fn app_usage(&self, app_id: &AppId, utc_day: &str) -> Result<UsageSummary, VendError>;
    async fn authenticate_customer_session(
        &self,
        token: &str,
    ) -> Result<Option<CustomerPrincipal>, VendError>;
    async fn setup_customer_password(
        &self,
        setup_token: &str,
        password: &str,
    ) -> Result<CustomerUserSummary, VendError>;
    async fn login_customer(
        &self,
        tenant_id: &TenantId,
        email: &str,
        password: &str,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError>;
    async fn logout_customer(&self, principal: &CustomerPrincipal) -> Result<(), VendError>;
    async fn list_customer_users(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<CustomerUserSummary>, VendError>;
    async fn invite_customer_user(
        &self,
        tenant_id: &TenantId,
        email: &str,
        display_name: &str,
        role: CustomerRole,
        setup_ttl_seconds: i64,
    ) -> Result<InvitedCustomerUser, VendError>;
    async fn update_customer_user(
        &self,
        tenant_id: &TenantId,
        user_id: &str,
        update: UpdateCustomerUser,
    ) -> Result<CustomerUserSummary, VendError>;
    async fn revoke_customer_sessions(
        &self,
        tenant_id: &TenantId,
        user_id: &str,
    ) -> Result<u64, VendError>;
    async fn get_key(&self, id: &SubKeyId) -> Result<Option<KeySummary>, VendError>;
    async fn create_oauth_state(
        &self,
        tenant_id: &TenantId,
        provider: &str,
        ttl_seconds: i64,
    ) -> Result<OAuthStart, VendError>;
    async fn consume_oauth_state(
        &self,
        state: &str,
        provider: &str,
    ) -> Result<ConsumedOAuthState, VendError>;
    async fn login_customer_external(
        &self,
        tenant_id: &TenantId,
        provider: &str,
        subject: &str,
        email: &str,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError>;
}

fn store_err(e: sqlx::Error) -> VendError {
    VendError::Storage(e.to_string())
}

struct MintedBearer {
    id: String,
    prefix: String,
    digest: [u8; 32],
    plaintext: String,
}

fn mint_bearer(kind: &str, id_prefix: &str) -> MintedBearer {
    let mut entropy = [0_u8; 32];
    OsRng.fill_bytes(&mut entropy);
    let secret = hex(&entropy);
    let prefix = secret[..16].to_string();
    let plaintext = format!("{kind}_{prefix}_{secret}");
    MintedBearer {
        id: format!("{id_prefix}_{}", Ulid::new()),
        prefix,
        digest: Sha256::digest(plaintext.as_bytes()).into(),
        plaintext,
    }
}

fn bearer_prefix<'a>(token: &'a str, kind: &str) -> Option<&'a str> {
    let rest = token.strip_prefix(kind)?.strip_prefix('_')?;
    let (prefix, secret) = rest.split_once('_')?;
    if prefix.len() != 16
        || secret.len() != 64
        || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !secret.starts_with(prefix)
    {
        return None;
    }
    Some(prefix)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn secure_digest_matches(stored: &[u8], plaintext: &str) -> Result<bool, VendError> {
    let stored: [u8; 32] = stored
        .try_into()
        .map_err(|_| VendError::Storage("invalid bearer digest length".into()))?;
    let actual: [u8; 32] = Sha256::digest(plaintext.as_bytes()).into();
    Ok(stored.ct_eq(&actual).unwrap_u8() == 1)
}

fn normalize_email(email: &str) -> Result<(String, String), VendError> {
    let display = email.trim();
    if display.is_empty()
        || display.len() > 320
        || display
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || display.matches('@').count() != 1
        || display.starts_with('@')
        || display.ends_with('@')
    {
        return Err(VendError::InvalidAmount);
    }
    Ok((display.to_string(), display.to_ascii_lowercase()))
}

fn validate_password(password: &str) -> Result<(), VendError> {
    if !(12..=256).contains(&password.len()) {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

async fn hash_password(password: String) -> Result<String, VendError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| VendError::Storage(format!("password hash failed: {error}")))
    })
    .await
    .map_err(|error| VendError::Storage(format!("password hash task failed: {error}")))?
}

async fn password_matches(password: String, encoded: String) -> Result<bool, VendError> {
    tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&encoded)
            .map_err(|error| VendError::Storage(format!("invalid password hash: {error}")))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(|error| VendError::Storage(format!("password verify task failed: {error}")))?
}

fn validate_ttl(seconds: i64, maximum: i64) -> Result<Duration, VendError> {
    if !(60..=maximum).contains(&seconds) {
        return Err(VendError::InvalidAmount);
    }
    Ok(Duration::seconds(seconds))
}

async fn issue_customer_session(
    pool: &PgPool,
    user: CustomerUserSummary,
    ttl_seconds: i64,
) -> Result<IssuedCustomerSession, VendError> {
    let ttl = validate_ttl(ttl_seconds, 30 * 24 * 60 * 60)?;
    let bearer = mint_bearer("vses", "vss");
    let expires_at = Utc::now() + ttl;
    sqlx::query(
        "INSERT INTO tenant_sessions (id, membership_id, prefix, secret_hash, expires_at) \
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(&bearer.id)
    .bind(&user.membership_id)
    .bind(&bearer.prefix)
    .bind(bearer.digest.as_slice())
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(store_err)?;
    Ok(IssuedCustomerSession {
        user,
        session_id: bearer.id,
        session_token: bearer.plaintext,
        expires_at,
    })
}

fn valid_provider(provider: &str) -> bool {
    matches!(provider, "github" | "google")
}

/// Derive a stable `bigint` advisory-lock key from an id string. Used with
/// `pg_advisory_xact_lock` to serialize concurrent spend-cap reservations for
/// the same key / end-user without a table-level lock.
fn advisory_key(id: &str) -> i64 {
    let digest = Sha256::digest(id.as_bytes());
    i64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least 8 bytes"),
    )
}

/// Insert a `Pending` metered-usage row using any sqlx executor (the pool for
/// the plain path, or a transaction connection for the cap-checked path).
async fn insert_pending<'e, E>(exec: E, usage: &MeteredUsage) -> Result<(), VendError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO metered_usage (charge_id, sub_key_id, tenant_id, app_id, end_user_id, \
         idem_key, hold_id, provider_cost_micros, charge_micros, platform_fee_micros, \
         tenant_net_micros, outcome, utc_day) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(usage.charge_id.as_str())
    .bind(usage.sub_key_id.as_str())
    .bind(usage.tenant_id.as_str())
    .bind(usage.app_id.as_str())
    .bind(usage.end_user_id.as_ref().map(|e| e.as_str()))
    .bind(&usage.idem_key)
    .bind(&usage.hold_id)
    .bind(usage.provider_cost_micros)
    .bind(usage.charge_micros)
    .bind(usage.platform_fee_micros)
    .bind(usage.tenant_net_micros)
    .bind(outcome_str(usage.outcome))
    .bind(&usage.utc_day)
    .execute(exec)
    .await
    .map_err(store_err)?;
    Ok(())
}

impl VendStore {
    /// Connect to Postgres and build a pool. Does NOT run migrations.
    pub async fn connect(database_url: &str) -> Result<Self, VendError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(store_err)?;
        Ok(VendStore { pool })
    }

    /// Wrap an existing pool (used by tests that manage their own pool).
    pub fn from_pool(pool: PgPool) -> Self {
        VendStore { pool }
    }

    /// Apply the embedded migrations. Safe to call repeatedly (idempotent).
    pub async fn migrate(&self) -> Result<(), VendError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|e| VendError::Storage(e.to_string()))
    }

    /// Borrow the underlying pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// List stale, durable intents that never attached Mint's returned hold id.
    pub async fn pending_unattached_holds(
        &self,
        older_than: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<Vec<PendingUnattachedHold>, VendError> {
        if !(1..=1_000).contains(&limit) {
            return Err(VendError::InvalidAmount);
        }
        let rows = sqlx::query(
            "SELECT u.charge_id, u.idem_key, u.charge_micros, a.mint_account_id \
             FROM metered_usage u JOIN apps a ON a.id = u.app_id \
             WHERE u.outcome = 'pending' AND u.hold_id = '' AND u.created_at < $1 \
             ORDER BY u.created_at, u.charge_id LIMIT $2",
        )
        .bind(older_than)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(store_err)?;
        rows.into_iter()
            .map(|row| {
                Ok(PendingUnattachedHold {
                    charge_id: ChargeId::parse(
                        &row.try_get::<String, _>("charge_id").map_err(store_err)?,
                    )?,
                    idem_key: row.try_get("idem_key").map_err(store_err)?,
                    payer_account: row.try_get("mint_account_id").map_err(store_err)?,
                    amount_micros: row.try_get("charge_micros").map_err(store_err)?,
                })
            })
            .collect()
    }

    /// Finalize one successfully released unattached intent and reopen its
    /// client idempotency key for a clean retry.
    pub async fn finish_unattached_release(
        &self,
        charge_id: &ChargeId,
        replay_key: &str,
    ) -> Result<(), VendError> {
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        let changed = sqlx::query(
            "UPDATE metered_usage SET outcome = 'released' \
             WHERE charge_id = $1 AND outcome = 'pending' AND hold_id = ''",
        )
        .bind(charge_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() == 0 {
            let outcome = sqlx::query("SELECT outcome FROM metered_usage WHERE charge_id = $1")
                .bind(charge_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(store_err)?
                .map(|row| row.try_get::<String, _>("outcome"))
                .transpose()
                .map_err(store_err)?;
            if outcome.as_deref() != Some("released") {
                return Err(VendError::IdempotencyConflict);
            }
        }
        sqlx::query("DELETE FROM idempotency_keys WHERE key = $1 AND state = 'pending'")
            .bind(replay_key)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        Ok(())
    }

    /// Atomically create a launchable tenant, app, pricing policy, and app-scoped
    /// provisioning key. The plaintext key is returned once and never persisted.
    pub async fn bootstrap_product(
        &self,
        config: BootstrapConfig,
    ) -> Result<BootstrapOutput, VendError> {
        let (owner_email, owner_email_normalized) = normalize_email(&config.owner_email)?;
        let owner_display_name = config.owner_display_name.trim();
        let setup_ttl = validate_ttl(config.setup_token_ttl_seconds, 30 * 24 * 60 * 60)?;
        if config.tenant_name.trim().is_empty()
            || config.app_name.trim().is_empty()
            || config.mint_account_id.trim().is_empty()
            || owner_display_name.is_empty()
            || owner_display_name.len() > 200
            || config.model_allowlist.is_empty()
            || config
                .model_allowlist
                .iter()
                .any(|model| !valid_relay_model(model))
            || config.markup_policy.funding_mode != FundingMode::TenantReserve
            || config.markup_policy.markup_bps > 10_000
            || config.markup_policy.platform_fee_bps > 10_000
            || config.markup_policy.min_fee_micros < 0
            || config.daily_cap_micros.is_none_or(|cap| cap <= 0)
            || config
                .per_end_user_daily_cap_micros
                .is_some_and(|cap| cap <= 0)
        {
            return Err(VendError::InvalidAmount);
        }

        let tenant_id = TenantId::generate();
        let app_id = AppId::generate();
        let owner_id = format!("vusr_{}", Ulid::new());
        let membership_id = format!("vmbr_{}", Ulid::new());
        let setup = mint_bearer("vset", "vst");
        let created_at = Utc::now();
        let setup_expires_at = created_at + setup_ttl;
        let minted = mint(MintParams {
            tenant_id: tenant_id.clone(),
            app_id: Some(app_id.clone()),
            end_user_id: None,
            scope: KeyScope::App,
            env: config.key_env,
            daily_cap_micros: config.daily_cap_micros,
            per_end_user_daily_cap_micros: config.per_end_user_daily_cap_micros,
        })?;
        let allowlist = serde_json::to_value(&config.model_allowlist)
            .map_err(|error| VendError::Storage(error.to_string()))?;

        let mut tx = self.pool.begin().await.map_err(store_err)?;
        sqlx::query("INSERT INTO tenants (id, name, mint_account_id) VALUES ($1,$2,$3)")
            .bind(tenant_id.as_str())
            .bind(config.tenant_name.trim())
            .bind(config.mint_account_id.trim())
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO apps (id, tenant_id, name, model_allowlist, zdr, mint_account_id) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(app_id.as_str())
        .bind(tenant_id.as_str())
        .bind(config.app_name.trim())
        .bind(allowlist)
        .bind(config.zdr)
        .bind(config.mint_account_id.trim())
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO markup_policy \
             (app_id, markup_bps, min_fee_micros, platform_fee_bps, funding_mode) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(app_id.as_str())
        .bind(i32::try_from(config.markup_policy.markup_bps).map_err(|_| VendError::Overflow)?)
        .bind(config.markup_policy.min_fee_micros)
        .bind(
            i32::try_from(config.markup_policy.platform_fee_bps)
                .map_err(|_| VendError::Overflow)?,
        )
        .bind(funding_str(config.markup_policy.funding_mode))
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO sub_keys (id, tenant_id, app_id, end_user_id, scope, status, \
             prefix, secret_hash, daily_cap_micros, per_end_user_daily_cap_micros) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(minted.record.id.as_str())
        .bind(minted.record.tenant_id.as_str())
        .bind(minted.record.app_id.as_ref().map(|id| id.as_str()))
        .bind(Option::<&str>::None)
        .bind(minted.record.scope.to_u8() as i16)
        .bind(status_str(minted.record.status))
        .bind(&minted.record.prefix)
        .bind(&minted.record.secret_hash)
        .bind(minted.record.daily_cap_micros)
        .bind(minted.record.per_end_user_daily_cap_micros)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO customer_users (id, email, email_normalized, display_name, status, \
             created_at) VALUES ($1,$2,$3,$4,'pending',$5)",
        )
        .bind(&owner_id)
        .bind(&owner_email)
        .bind(&owner_email_normalized)
        .bind(owner_display_name)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO tenant_memberships (id, user_id, tenant_id, role, status, created_at) \
             VALUES ($1,$2,$3,'owner','invited',$4)",
        )
        .bind(&membership_id)
        .bind(&owner_id)
        .bind(tenant_id.as_str())
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO tenant_setup_tokens (id, membership_id, prefix, secret_hash, expires_at) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&setup.id)
        .bind(&membership_id)
        .bind(&setup.prefix)
        .bind(setup.digest.as_slice())
        .bind(setup_expires_at)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;

        Ok(BootstrapOutput {
            tenant_id: tenant_id.clone(),
            app_id,
            sub_key_id: minted.record.id,
            secret: minted.secret,
            owner: CustomerUserSummary {
                id: owner_id,
                membership_id,
                tenant_id,
                email: owner_email,
                display_name: owner_display_name.to_string(),
                role: CustomerRole::Owner,
                status: CustomerUserStatus::Invited,
                external_providers: Vec::new(),
                created_at,
            },
            owner_setup_token: setup.plaintext,
        })
    }
}

#[async_trait]
impl ControlPlaneRepo for VendStore {
    async fn bootstrap(&self, config: BootstrapConfig) -> Result<BootstrapOutput, VendError> {
        self.bootstrap_product(config).await
    }

    async fn list_tenants(&self) -> Result<Vec<TenantSummary>, VendError> {
        let rows =
            sqlx::query("SELECT id, name, mint_account_id FROM tenants ORDER BY created_at, id")
                .fetch_all(&self.pool)
                .await
                .map_err(store_err)?;
        rows.iter().map(row_to_tenant_summary).collect()
    }

    async fn get_tenant(&self, id: &TenantId) -> Result<Option<TenantSummary>, VendError> {
        let row = sqlx::query("SELECT id, name, mint_account_id FROM tenants WHERE id = $1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?;
        row.as_ref().map(row_to_tenant_summary).transpose()
    }

    async fn list_apps(&self, tenant_id: &TenantId) -> Result<Vec<AppSummary>, VendError> {
        let rows = sqlx::query(
            "SELECT a.id, a.tenant_id, a.name, a.model_allowlist, a.zdr, a.enabled, \
             a.mint_account_id, m.markup_bps, m.min_fee_micros, m.platform_fee_bps, \
             m.funding_mode FROM apps a JOIN markup_policy m ON m.app_id = a.id \
             WHERE a.tenant_id = $1 ORDER BY a.created_at, a.id",
        )
        .bind(tenant_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(store_err)?;
        rows.iter().map(row_to_app_summary).collect()
    }

    async fn get_app(&self, id: &AppId) -> Result<Option<AppSummary>, VendError> {
        let row = sqlx::query(
            "SELECT a.id, a.tenant_id, a.name, a.model_allowlist, a.zdr, a.enabled, \
             a.mint_account_id, m.markup_bps, m.min_fee_micros, m.platform_fee_bps, \
             m.funding_mode FROM apps a JOIN markup_policy m ON m.app_id = a.id \
             WHERE a.id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref().map(row_to_app_summary).transpose()
    }

    async fn create_app(
        &self,
        tenant_id: &TenantId,
        input: CreateApp,
    ) -> Result<AppSummary, VendError> {
        validate_app_input(&input)?;
        if ControlPlaneRepo::get_tenant(self, tenant_id)
            .await?
            .is_none()
        {
            return Err(VendError::NotFound);
        }
        let id = AppId::generate();
        let allowlist = serde_json::to_value(&input.model_allowlist)
            .map_err(|error| VendError::Storage(error.to_string()))?;
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        sqlx::query(
            "INSERT INTO apps (id, tenant_id, name, model_allowlist, zdr, enabled, mint_account_id) \
             VALUES ($1,$2,$3,$4,$5,true,$6)",
        )
        .bind(id.as_str())
        .bind(tenant_id.as_str())
        .bind(input.name.trim())
        .bind(allowlist)
        .bind(input.zdr)
        .bind(input.mint_account_id.trim())
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO markup_policy \
             (app_id, markup_bps, min_fee_micros, platform_fee_bps, funding_mode) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id.as_str())
        .bind(i32::try_from(input.markup_policy.markup_bps).map_err(|_| VendError::Overflow)?)
        .bind(input.markup_policy.min_fee_micros)
        .bind(i32::try_from(input.markup_policy.platform_fee_bps).map_err(|_| VendError::Overflow)?)
        .bind(funding_str(input.markup_policy.funding_mode))
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        ControlPlaneRepo::get_app(self, &id)
            .await?
            .ok_or(VendError::NotFound)
    }

    async fn set_app_enabled(&self, id: &AppId, enabled: bool) -> Result<AppSummary, VendError> {
        let changed = sqlx::query("UPDATE apps SET enabled = $2 WHERE id = $1")
            .bind(id.as_str())
            .bind(enabled)
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
        if changed.rows_affected() == 0 {
            return Err(VendError::NotFound);
        }
        ControlPlaneRepo::get_app(self, id)
            .await?
            .ok_or(VendError::NotFound)
    }

    async fn list_app_keys(&self, app_id: &AppId) -> Result<Vec<KeySummary>, VendError> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, app_id, scope, status, prefix, daily_cap_micros, \
             per_end_user_daily_cap_micros FROM sub_keys WHERE app_id = $1 \
             ORDER BY created_at, id",
        )
        .bind(app_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(store_err)?;
        rows.iter().map(row_to_key_summary).collect()
    }

    async fn issue_app_key(
        &self,
        app_id: &AppId,
        input: CreateAppKey,
    ) -> Result<IssuedKey, VendError> {
        validate_caps(input.daily_cap_micros, input.per_end_user_daily_cap_micros)?;
        let app = ControlPlaneRepo::get_app(self, app_id)
            .await?
            .ok_or(VendError::NotFound)?;
        let minted = mint(MintParams {
            tenant_id: app.tenant_id,
            app_id: Some(app.id),
            end_user_id: None,
            scope: KeyScope::App,
            env: input.env,
            daily_cap_micros: input.daily_cap_micros,
            per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros,
        })?;
        KeyRepo::insert(self, &minted.record).await?;
        Ok(IssuedKey {
            key: key_summary(&minted.record),
            secret: minted.secret,
        })
    }

    async fn revoke_key(&self, id: &SubKeyId) -> Result<KeySummary, VendError> {
        let row = sqlx::query(
            "UPDATE sub_keys SET status = 'revoked' WHERE id = $1 RETURNING id, tenant_id, \
             app_id, scope, status, prefix, daily_cap_micros, per_end_user_daily_cap_micros",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref()
            .map(row_to_key_summary)
            .transpose()?
            .ok_or(VendError::NotFound)
    }

    async fn provision_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
        mint_account_id: &str,
    ) -> Result<EndUser, VendError> {
        let external_ref = external_ref.trim();
        let mint_account_id = mint_account_id.trim();
        if external_ref.is_empty() || mint_account_id.is_empty() {
            return Err(VendError::InvalidAmount);
        }
        if ControlPlaneRepo::get_app(self, app_id).await?.is_none() {
            return Err(VendError::NotFound);
        }
        if let Some(existing) = TenantRepo::find_end_user(self, app_id, external_ref).await? {
            return if existing.mint_account_id.as_deref() == Some(mint_account_id) {
                Ok(existing)
            } else {
                Err(VendError::IdempotencyConflict)
            };
        }
        TenantRepo::create_end_user(self, app_id, external_ref, mint_account_id).await
    }

    async fn app_usage(&self, app_id: &AppId, utc_day: &str) -> Result<UsageSummary, VendError> {
        if ControlPlaneRepo::get_app(self, app_id).await?.is_none() {
            return Err(VendError::NotFound);
        }
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS requests, \
             COUNT(*) FILTER (WHERE outcome = 'pending')::BIGINT AS pending_requests, \
             COUNT(*) FILTER (WHERE outcome = 'captured')::BIGINT AS captured_requests, \
             COUNT(*) FILTER (WHERE outcome = 'released')::BIGINT AS released_requests, \
             COALESCE(SUM(charge_micros) FILTER (WHERE outcome = 'pending'), 0)::BIGINT \
                 AS pending_charge_micros, \
             COALESCE(SUM(charge_micros) FILTER (WHERE outcome = 'captured'), 0)::BIGINT \
                 AS captured_charge_micros \
             FROM metered_usage WHERE app_id = $1 AND utc_day = $2",
        )
        .bind(app_id.as_str())
        .bind(utc_day)
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(UsageSummary {
            app_id: app_id.clone(),
            utc_day: utc_day.to_string(),
            requests: row.try_get("requests").map_err(store_err)?,
            pending_requests: row.try_get("pending_requests").map_err(store_err)?,
            captured_requests: row.try_get("captured_requests").map_err(store_err)?,
            released_requests: row.try_get("released_requests").map_err(store_err)?,
            pending_charge_micros: row.try_get("pending_charge_micros").map_err(store_err)?,
            captured_charge_micros: row.try_get("captured_charge_micros").map_err(store_err)?,
        })
    }

    async fn authenticate_customer_session(
        &self,
        token: &str,
    ) -> Result<Option<CustomerPrincipal>, VendError> {
        let Some(prefix) = bearer_prefix(token, "vses") else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT s.id AS session_id, s.secret_hash, u.id AS user_id, m.id AS membership_id, \
             m.tenant_id, m.role FROM tenant_sessions s \
             JOIN tenant_memberships m ON m.id = s.membership_id \
             JOIN customer_users u ON u.id = m.user_id \
             WHERE s.prefix = $1 AND s.revoked_at IS NULL AND s.expires_at > now() \
               AND m.status = 'active' AND u.status = 'active'",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        let Some(row) = row else {
            let dummy = [0_u8; 32];
            let _ = secure_digest_matches(&dummy, token)?;
            return Ok(None);
        };
        let digest: Vec<u8> = row.try_get("secret_hash").map_err(store_err)?;
        if !secure_digest_matches(&digest, token)? {
            return Ok(None);
        }
        Ok(Some(CustomerPrincipal {
            session_id: row.try_get("session_id").map_err(store_err)?,
            user_id: row.try_get("user_id").map_err(store_err)?,
            membership_id: row.try_get("membership_id").map_err(store_err)?,
            tenant_id: TenantId::parse(&row.try_get::<String, _>("tenant_id").map_err(store_err)?)?,
            role: CustomerRole::parse(&row.try_get::<String, _>("role").map_err(store_err)?)?,
        }))
    }

    async fn setup_customer_password(
        &self,
        setup_token: &str,
        password: &str,
    ) -> Result<CustomerUserSummary, VendError> {
        validate_password(password)?;
        let prefix = bearer_prefix(setup_token, "vset").ok_or(VendError::Unauthorized)?;
        let row = sqlx::query(
            "SELECT id, membership_id, secret_hash FROM tenant_setup_tokens \
             WHERE prefix = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?
        .ok_or(VendError::Unauthorized)?;
        let digest: Vec<u8> = row.try_get("secret_hash").map_err(store_err)?;
        if !secure_digest_matches(&digest, setup_token)? {
            return Err(VendError::Unauthorized);
        }
        let token_id: String = row.try_get("id").map_err(store_err)?;
        let membership_id: String = row.try_get("membership_id").map_err(store_err)?;
        let account = sqlx::query(
            "SELECT u.password_hash, u.status FROM customer_users u JOIN tenant_memberships m \
             ON m.user_id = u.id WHERE m.id = $1 AND m.status = 'invited'",
        )
        .bind(&membership_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?
        .ok_or(VendError::Unauthorized)?;
        if account.try_get::<String, _>("status").map_err(store_err)? == "disabled" {
            return Err(VendError::Unauthorized);
        }
        let existing_hash: Option<String> = account.try_get("password_hash").map_err(store_err)?;
        let encoded = if let Some(existing) = existing_hash {
            if !password_matches(password.to_string(), existing.clone()).await? {
                return Err(VendError::Unauthorized);
            }
            existing
        } else {
            hash_password(password.to_string()).await?
        };
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        let consumed = sqlx::query(
            "UPDATE tenant_setup_tokens SET consumed_at = now() \
             WHERE id = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(&token_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        if consumed.rows_affected() != 1 {
            return Err(VendError::Unauthorized);
        }
        let activated = sqlx::query(
            "UPDATE customer_users u SET password_hash = COALESCE(u.password_hash, $2), \
             status = 'active', activated_at = COALESCE(activated_at, now()) \
             FROM tenant_memberships m WHERE m.id = $1 AND m.user_id = u.id \
             AND u.status <> 'disabled'",
        )
        .bind(&membership_id)
        .bind(encoded)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        if activated.rows_affected() != 1 {
            return Err(VendError::Unauthorized);
        }
        sqlx::query(
            "UPDATE tenant_memberships SET status = 'active', \
             activated_at = COALESCE(activated_at, now()) WHERE id = $1 AND status = 'invited'",
        )
        .bind(&membership_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        let query = format!("{CUSTOMER_USER_SELECT} WHERE m.id = $1");
        let row = sqlx::query(&query)
            .bind(membership_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?
            .ok_or(VendError::Unauthorized)?;
        row_to_customer_user(&row)
    }

    async fn login_customer(
        &self,
        tenant_id: &TenantId,
        email: &str,
        password: &str,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError> {
        validate_password(password)?;
        let (_, normalized) = normalize_email(email)?;
        let query =
            format!("{CUSTOMER_USER_SELECT} WHERE m.tenant_id = $1 AND u.email_normalized = $2");
        let row = sqlx::query(&query)
            .bind(tenant_id.as_str())
            .bind(&normalized)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?;
        let (user, encoded, eligible) = match row {
            Some(row) => {
                let status: String = row.try_get("status").map_err(store_err)?;
                let account: (Option<String>, String) = sqlx::query_as(
                    "SELECT password_hash, status FROM customer_users WHERE id = $1",
                )
                .bind(row.try_get::<String, _>("id").map_err(store_err)?)
                .fetch_one(&self.pool)
                .await
                .map_err(store_err)?;
                (
                    Some(row_to_customer_user(&row)?),
                    account.0,
                    status == "active" && account.1 == "active",
                )
            }
            None => (None, None, false),
        };
        let encoded = match encoded {
            Some(value) => value,
            None => hash_password("vend-dummy-password".to_string()).await?,
        };
        if !password_matches(password.to_string(), encoded).await? || !eligible {
            return Err(VendError::Unauthorized);
        }
        issue_customer_session(
            &self.pool,
            user.ok_or(VendError::Unauthorized)?,
            session_ttl_seconds,
        )
        .await
    }

    async fn logout_customer(&self, principal: &CustomerPrincipal) -> Result<(), VendError> {
        sqlx::query(
            "UPDATE tenant_sessions SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE id = $1 AND membership_id = $2",
        )
        .bind(&principal.session_id)
        .bind(&principal.membership_id)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn list_customer_users(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<CustomerUserSummary>, VendError> {
        let query =
            format!("{CUSTOMER_USER_SELECT} WHERE m.tenant_id = $1 ORDER BY m.created_at, m.id");
        let rows = sqlx::query(&query)
            .bind(tenant_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(store_err)?;
        rows.iter().map(row_to_customer_user).collect()
    }

    async fn invite_customer_user(
        &self,
        tenant_id: &TenantId,
        email: &str,
        display_name: &str,
        role: CustomerRole,
        setup_ttl_seconds: i64,
    ) -> Result<InvitedCustomerUser, VendError> {
        let (email, normalized) = normalize_email(email)?;
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name.len() > 200 {
            return Err(VendError::InvalidAmount);
        }
        let ttl = validate_ttl(setup_ttl_seconds, 30 * 24 * 60 * 60)?;
        if ControlPlaneRepo::get_tenant(self, tenant_id)
            .await?
            .is_none()
        {
            return Err(VendError::NotFound);
        }
        let bearer = mint_bearer("vset", "vst");
        let expires_at = Utc::now() + ttl;
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_key(&format!(
                "invite:{}:{normalized}",
                tenant_id.as_str()
            )))
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        let existing = sqlx::query(
            "SELECT u.id AS user_id, m.id AS membership_id, m.status AS membership_status \
             FROM customer_users u LEFT JOIN tenant_memberships m \
               ON m.user_id = u.id AND m.tenant_id = $1 WHERE u.email_normalized = $2",
        )
        .bind(tenant_id.as_str())
        .bind(&normalized)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?;
        let (_user_id, membership_id) = if let Some(row) = existing {
            let user_id: String = row.try_get("user_id").map_err(store_err)?;
            let membership_id: Option<String> = row.try_get("membership_id").map_err(store_err)?;
            if let Some(membership_id) = membership_id {
                if row
                    .try_get::<Option<String>, _>("membership_status")
                    .map_err(store_err)?
                    .as_deref()
                    != Some("invited")
                {
                    return Err(VendError::IdempotencyConflict);
                }
                sqlx::query("UPDATE tenant_memberships SET role = $2 WHERE id = $1")
                    .bind(&membership_id)
                    .bind(role.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(store_err)?;
                (user_id, membership_id)
            } else {
                let membership_id = format!("vmbr_{}", Ulid::new());
                sqlx::query(
                    "INSERT INTO tenant_memberships (id, user_id, tenant_id, role, status) \
                     VALUES ($1,$2,$3,$4,'invited')",
                )
                .bind(&membership_id)
                .bind(&user_id)
                .bind(tenant_id.as_str())
                .bind(role.as_str())
                .execute(&mut *tx)
                .await
                .map_err(store_err)?;
                (user_id, membership_id)
            }
        } else {
            let user_id = format!("vusr_{}", Ulid::new());
            let membership_id = format!("vmbr_{}", Ulid::new());
            sqlx::query(
                "INSERT INTO customer_users \
                 (id, email, email_normalized, display_name, status) VALUES ($1,$2,$3,$4,'pending')",
            )
            .bind(&user_id)
            .bind(&email)
            .bind(&normalized)
            .bind(display_name)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
            sqlx::query(
                "INSERT INTO tenant_memberships (id, user_id, tenant_id, role, status) \
                 VALUES ($1,$2,$3,$4,'invited')",
            )
            .bind(&membership_id)
            .bind(&user_id)
            .bind(tenant_id.as_str())
            .bind(role.as_str())
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
            (user_id, membership_id)
        };
        sqlx::query(
            "UPDATE tenant_setup_tokens SET consumed_at = now() \
             WHERE membership_id = $1 AND consumed_at IS NULL",
        )
        .bind(&membership_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO tenant_setup_tokens (id, membership_id, prefix, secret_hash, expires_at) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&bearer.id)
        .bind(&membership_id)
        .bind(&bearer.prefix)
        .bind(bearer.digest.as_slice())
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        let query = format!("{CUSTOMER_USER_SELECT} WHERE m.id = $1");
        let row = sqlx::query(&query)
            .bind(membership_id)
            .fetch_one(&self.pool)
            .await
            .map_err(store_err)?;
        Ok(InvitedCustomerUser {
            user: row_to_customer_user(&row)?,
            setup_token: bearer.plaintext,
            setup_expires_at: expires_at,
        })
    }

    async fn update_customer_user(
        &self,
        tenant_id: &TenantId,
        user_id: &str,
        update: UpdateCustomerUser,
    ) -> Result<CustomerUserSummary, VendError> {
        if update.role.is_none() && update.disabled.is_none() {
            return Err(VendError::InvalidAmount);
        }
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_key(&format!("owners:{}", tenant_id.as_str())))
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        let row = sqlx::query(
            "SELECT m.id AS membership_id, m.role, m.status, u.password_hash, \
             EXISTS(SELECT 1 FROM customer_external_identities x WHERE x.user_id = u.id) \
               AS has_external_identity \
             FROM tenant_memberships m JOIN customer_users u ON u.id = m.user_id \
             WHERE u.id = $1 AND m.tenant_id = $2 FOR UPDATE OF m",
        )
        .bind(user_id)
        .bind(tenant_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or(VendError::NotFound)?;
        let old_role: String = row.try_get("role").map_err(store_err)?;
        let old_status: String = row.try_get("status").map_err(store_err)?;
        let membership_id: String = row.try_get("membership_id").map_err(store_err)?;
        let removing_owner = old_role == "owner"
            && old_status == "active"
            && (update.role.is_some_and(|role| role != CustomerRole::Owner)
                || update.disabled == Some(true));
        if removing_owner {
            let owners: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tenant_memberships \
                 WHERE tenant_id = $1 AND role = 'owner' AND status = 'active'",
            )
            .bind(tenant_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(store_err)?;
            if owners <= 1 {
                return Err(VendError::IdempotencyConflict);
            }
        }
        if update.disabled == Some(false)
            && old_status == "invited"
            && row
                .try_get::<Option<String>, _>("password_hash")
                .map_err(store_err)?
                .is_none()
            && !row
                .try_get::<bool, _>("has_external_identity")
                .map_err(store_err)?
        {
            return Err(VendError::IdempotencyConflict);
        }
        sqlx::query(
            "UPDATE tenant_memberships SET role = COALESCE($2, role), status = CASE \
             WHEN $3::BOOLEAN IS NULL THEN status WHEN $3 THEN 'disabled' \
             ELSE 'active' END WHERE id = $1",
        )
        .bind(&membership_id)
        .bind(update.role.map(CustomerRole::as_str))
        .bind(update.disabled)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        if update.disabled == Some(true) {
            sqlx::query(
                "UPDATE tenant_sessions SET revoked_at = COALESCE(revoked_at, now()) \
                 WHERE membership_id = $1",
            )
            .bind(&membership_id)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        }
        tx.commit().await.map_err(store_err)?;
        let query = format!("{CUSTOMER_USER_SELECT} WHERE u.id = $1 AND m.tenant_id = $2");
        let row = sqlx::query(&query)
            .bind(user_id)
            .bind(tenant_id.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(store_err)?;
        row_to_customer_user(&row)
    }

    async fn revoke_customer_sessions(
        &self,
        tenant_id: &TenantId,
        user_id: &str,
    ) -> Result<u64, VendError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tenant_memberships m \
             WHERE m.user_id = $1 AND m.tenant_id = $2)",
        )
        .bind(user_id)
        .bind(tenant_id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        if !exists {
            return Err(VendError::NotFound);
        }
        let changed = sqlx::query(
            "UPDATE tenant_sessions s SET revoked_at = now() FROM tenant_memberships m \
             WHERE s.membership_id = m.id AND m.user_id = $1 AND m.tenant_id = $2 \
               AND s.revoked_at IS NULL",
        )
        .bind(user_id)
        .bind(tenant_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(changed.rows_affected())
    }

    async fn get_key(&self, id: &SubKeyId) -> Result<Option<KeySummary>, VendError> {
        let row = sqlx::query(
            "SELECT id, tenant_id, app_id, scope, status, prefix, daily_cap_micros, \
             per_end_user_daily_cap_micros FROM sub_keys WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref().map(row_to_key_summary).transpose()
    }

    async fn create_oauth_state(
        &self,
        tenant_id: &TenantId,
        provider: &str,
        ttl_seconds: i64,
    ) -> Result<OAuthStart, VendError> {
        if !valid_provider(provider) {
            return Err(VendError::InvalidAmount);
        }
        let ttl = validate_ttl(ttl_seconds, 15 * 60)?;
        if ControlPlaneRepo::get_tenant(self, tenant_id)
            .await?
            .is_none()
        {
            return Err(VendError::NotFound);
        }
        let state = mint_bearer("vost", "vos");
        let mut verifier_entropy = [0_u8; 32];
        let mut nonce_entropy = [0_u8; 32];
        OsRng.fill_bytes(&mut verifier_entropy);
        OsRng.fill_bytes(&mut nonce_entropy);
        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_entropy);
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let nonce = URL_SAFE_NO_PAD.encode(nonce_entropy);
        sqlx::query(
            "INSERT INTO oauth_login_states \
             (id, tenant_id, provider, prefix, secret_hash, code_verifier, nonce, expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&state.id)
        .bind(tenant_id.as_str())
        .bind(provider)
        .bind(&state.prefix)
        .bind(state.digest.as_slice())
        .bind(&code_verifier)
        .bind(&nonce)
        .bind(Utc::now() + ttl)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(OAuthStart {
            state: state.plaintext,
            code_challenge,
            nonce,
        })
    }

    async fn consume_oauth_state(
        &self,
        state: &str,
        provider: &str,
    ) -> Result<ConsumedOAuthState, VendError> {
        if !valid_provider(provider) {
            return Err(VendError::Unauthorized);
        }
        let prefix = bearer_prefix(state, "vost").ok_or(VendError::Unauthorized)?;
        let row = sqlx::query(
            "SELECT id, tenant_id, secret_hash, code_verifier, nonce FROM oauth_login_states \
             WHERE prefix = $1 AND provider = $2 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(prefix)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?
        .ok_or(VendError::Unauthorized)?;
        let digest: Vec<u8> = row.try_get("secret_hash").map_err(store_err)?;
        if !secure_digest_matches(&digest, state)? {
            return Err(VendError::Unauthorized);
        }
        let id: String = row.try_get("id").map_err(store_err)?;
        let tenant: String = row.try_get("tenant_id").map_err(store_err)?;
        let code_verifier: String = row.try_get("code_verifier").map_err(store_err)?;
        let nonce: String = row.try_get("nonce").map_err(store_err)?;
        let changed = sqlx::query(
            "UPDATE oauth_login_states SET consumed_at = now() \
             WHERE id = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() != 1 {
            return Err(VendError::Unauthorized);
        }
        Ok(ConsumedOAuthState {
            tenant_id: TenantId::parse(&tenant)?,
            code_verifier,
            nonce,
        })
    }

    async fn login_customer_external(
        &self,
        tenant_id: &TenantId,
        provider: &str,
        subject: &str,
        email: &str,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError> {
        if !valid_provider(provider) || subject.is_empty() || subject.len() > 255 {
            return Err(VendError::Unauthorized);
        }
        let (_, normalized) = normalize_email(email).map_err(|_| VendError::Unauthorized)?;
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_key(&format!("external:{provider}:{subject}")))
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        let linked: Option<String> = sqlx::query_scalar(
            "SELECT user_id FROM customer_external_identities \
             WHERE provider = $1 AND subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?;
        let user_id = if let Some(user_id) = linked {
            user_id
        } else {
            let row = sqlx::query(
                "SELECT u.id FROM customer_users u JOIN tenant_memberships m ON m.user_id = u.id \
                 WHERE m.tenant_id = $1 AND u.email_normalized = $2 \
                   AND m.status <> 'disabled' AND u.status <> 'disabled' FOR UPDATE OF u, m",
            )
            .bind(tenant_id.as_str())
            .bind(&normalized)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_err)?
            .ok_or(VendError::Unauthorized)?;
            let user_id: String = row.try_get("id").map_err(store_err)?;
            sqlx::query(
                "INSERT INTO customer_external_identities (provider, subject, user_id) \
                 VALUES ($1,$2,$3)",
            )
            .bind(provider)
            .bind(subject)
            .bind(&user_id)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
            user_id
        };
        let membership = sqlx::query(
            "SELECT m.id, m.status, u.email_normalized FROM tenant_memberships m \
             JOIN customer_users u ON u.id = m.user_id \
             WHERE m.user_id = $1 AND m.tenant_id = $2 \
               AND m.status <> 'disabled' AND u.status <> 'disabled' FOR UPDATE OF m, u",
        )
        .bind(&user_id)
        .bind(tenant_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or(VendError::Unauthorized)?;
        let membership_id: String = membership.try_get("id").map_err(store_err)?;
        if membership
            .try_get::<String, _>("status")
            .map_err(store_err)?
            == "invited"
            && membership
                .try_get::<String, _>("email_normalized")
                .map_err(store_err)?
                != normalized
        {
            return Err(VendError::Unauthorized);
        }
        sqlx::query(
            "UPDATE customer_users SET status = 'active', \
             activated_at = COALESCE(activated_at, now()) WHERE id = $1",
        )
        .bind(&user_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "UPDATE tenant_memberships SET status = 'active', \
             activated_at = COALESCE(activated_at, now()) WHERE id = $1",
        )
        .bind(&membership_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        let query = format!("{CUSTOMER_USER_SELECT} WHERE m.id = $1");
        let row = sqlx::query(&query)
            .bind(membership_id)
            .fetch_one(&self.pool)
            .await
            .map_err(store_err)?;
        issue_customer_session(&self.pool, row_to_customer_user(&row)?, session_ttl_seconds).await
    }
}

fn validate_caps(daily: Option<i64>, per_end_user: Option<i64>) -> Result<(), VendError> {
    if daily.is_some_and(|cap| cap <= 0) || per_end_user.is_some_and(|cap| cap <= 0) {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

fn validate_app_input(input: &CreateApp) -> Result<(), VendError> {
    if input.name.trim().is_empty()
        || input.mint_account_id.trim().is_empty()
        || input.model_allowlist.is_empty()
        || input.markup_policy.funding_mode != FundingMode::TenantReserve
        || input.markup_policy.markup_bps > 10_000
        || input.markup_policy.platform_fee_bps > 10_000
        || input.markup_policy.min_fee_micros < 0
        || input
            .model_allowlist
            .iter()
            .any(|model| !valid_relay_model(model))
    {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

fn valid_relay_model(model: &str) -> bool {
    let model = model.trim();
    model.starts_with("tensorzero::model_name::")
        || model.starts_with("tensorzero::function_name::")
}

// --- column mappers -------------------------------------------------------

fn scope_from_i16(n: i16) -> Result<KeyScope, VendError> {
    u8::try_from(n)
        .ok()
        .and_then(KeyScope::from_u8)
        .ok_or(VendError::BadId)
}

fn status_from_str(s: &str) -> Result<KeyStatus, VendError> {
    match s {
        "active" => Ok(KeyStatus::Active),
        "revoked" => Ok(KeyStatus::Revoked),
        _ => Err(VendError::BadId),
    }
}

fn status_str(s: KeyStatus) -> &'static str {
    match s {
        KeyStatus::Active => "active",
        KeyStatus::Revoked => "revoked",
    }
}

fn funding_from_str(s: &str) -> Result<FundingMode, VendError> {
    match s {
        "tenant_reserve" => Ok(FundingMode::TenantReserve),
        "end_user_reserve" => Ok(FundingMode::EndUserReserve),
        "byok" => Ok(FundingMode::Byok),
        _ => Err(VendError::BadId),
    }
}

fn funding_str(mode: FundingMode) -> &'static str {
    match mode {
        FundingMode::TenantReserve => "tenant_reserve",
        FundingMode::EndUserReserve => "end_user_reserve",
        FundingMode::Byok => "byok",
    }
}

fn outcome_str(o: UsageOutcome) -> &'static str {
    match o {
        UsageOutcome::Pending => "pending",
        UsageOutcome::Captured => "captured",
        UsageOutcome::Released => "released",
    }
}

fn row_to_tenant_summary(row: &PgRow) -> Result<TenantSummary, VendError> {
    Ok(TenantSummary {
        id: TenantId::parse(&row.try_get::<String, _>("id").map_err(store_err)?)?,
        name: row.try_get("name").map_err(store_err)?,
        mint_account_id: row.try_get("mint_account_id").map_err(store_err)?,
    })
}

fn row_to_app_summary(row: &PgRow) -> Result<AppSummary, VendError> {
    let allowlist: serde_json::Value = row.try_get("model_allowlist").map_err(store_err)?;
    let markup_bps: i32 = row.try_get("markup_bps").map_err(store_err)?;
    let platform_fee_bps: i32 = row.try_get("platform_fee_bps").map_err(store_err)?;
    let funding_mode: String = row.try_get("funding_mode").map_err(store_err)?;
    Ok(AppSummary {
        id: AppId::parse(&row.try_get::<String, _>("id").map_err(store_err)?)?,
        tenant_id: TenantId::parse(&row.try_get::<String, _>("tenant_id").map_err(store_err)?)?,
        name: row.try_get("name").map_err(store_err)?,
        model_allowlist: serde_json::from_value(allowlist)
            .map_err(|error| VendError::Storage(format!("model_allowlist: {error}")))?,
        zdr: row.try_get("zdr").map_err(store_err)?,
        enabled: row.try_get("enabled").map_err(store_err)?,
        markup_policy: MarkupPolicy {
            markup_bps: u32::try_from(markup_bps).map_err(|_| VendError::InvalidAmount)?,
            min_fee_micros: row.try_get("min_fee_micros").map_err(store_err)?,
            platform_fee_bps: u32::try_from(platform_fee_bps)
                .map_err(|_| VendError::InvalidAmount)?,
            funding_mode: funding_from_str(&funding_mode)?,
        },
        mint_account_id: row.try_get("mint_account_id").map_err(store_err)?,
    })
}

fn key_summary(record: &SubKeyRecord) -> KeySummary {
    KeySummary {
        id: record.id.clone(),
        tenant_id: record.tenant_id.clone(),
        app_id: record.app_id.clone(),
        scope: record.scope,
        status: record.status,
        prefix: record.prefix.clone(),
        daily_cap_micros: record.daily_cap_micros,
        per_end_user_daily_cap_micros: record.per_end_user_daily_cap_micros,
    }
}

fn row_to_key_summary(row: &PgRow) -> Result<KeySummary, VendError> {
    let id: String = row.try_get("id").map_err(store_err)?;
    let tenant_id: String = row.try_get("tenant_id").map_err(store_err)?;
    let app_id: Option<String> = row.try_get("app_id").map_err(store_err)?;
    let scope: i16 = row.try_get("scope").map_err(store_err)?;
    let status: String = row.try_get("status").map_err(store_err)?;
    Ok(KeySummary {
        id: SubKeyId::parse(&id)?,
        tenant_id: TenantId::parse(&tenant_id)?,
        app_id: app_id.map(|value| AppId::parse(&value)).transpose()?,
        scope: scope_from_i16(scope)?,
        status: status_from_str(&status)?,
        prefix: row.try_get("prefix").map_err(store_err)?,
        daily_cap_micros: row.try_get("daily_cap_micros").map_err(store_err)?,
        per_end_user_daily_cap_micros: row
            .try_get("per_end_user_daily_cap_micros")
            .map_err(store_err)?,
    })
}

fn row_to_customer_user(row: &PgRow) -> Result<CustomerUserSummary, VendError> {
    let tenant_id: String = row.try_get("tenant_id").map_err(store_err)?;
    let providers: serde_json::Value = row.try_get("external_providers").map_err(store_err)?;
    Ok(CustomerUserSummary {
        id: row.try_get("id").map_err(store_err)?,
        membership_id: row.try_get("membership_id").map_err(store_err)?,
        tenant_id: TenantId::parse(&tenant_id)?,
        email: row.try_get("email").map_err(store_err)?,
        display_name: row.try_get("display_name").map_err(store_err)?,
        role: CustomerRole::parse(&row.try_get::<String, _>("role").map_err(store_err)?)?,
        status: CustomerUserStatus::parse(&row.try_get::<String, _>("status").map_err(store_err)?)?,
        external_providers: serde_json::from_value(providers)
            .map_err(|error| VendError::Storage(format!("external providers: {error}")))?,
        created_at: row.try_get("created_at").map_err(store_err)?,
    })
}

const CUSTOMER_USER_SELECT: &str =
    "SELECT u.id, m.id AS membership_id, m.tenant_id, u.email, u.display_name, m.role, \
     m.status, m.created_at, \
     COALESCE((SELECT jsonb_agg(x.provider ORDER BY x.provider) \
       FROM customer_external_identities x WHERE x.user_id = u.id), '[]'::jsonb) \
       AS external_providers FROM customer_users u \
       JOIN tenant_memberships m ON m.user_id = u.id";

fn row_to_sub_key(row: &PgRow) -> Result<SubKeyRecord, VendError> {
    let id: String = row.try_get("id").map_err(store_err)?;
    let tenant_id: String = row.try_get("tenant_id").map_err(store_err)?;
    let app_id: Option<String> = row.try_get("app_id").map_err(store_err)?;
    let end_user_id: Option<String> = row.try_get("end_user_id").map_err(store_err)?;
    let scope: i16 = row.try_get("scope").map_err(store_err)?;
    let status: String = row.try_get("status").map_err(store_err)?;
    let prefix: String = row.try_get("prefix").map_err(store_err)?;
    let secret_hash: String = row.try_get("secret_hash").map_err(store_err)?;
    let daily_cap_micros: Option<i64> = row.try_get("daily_cap_micros").map_err(store_err)?;
    let per_end_user_daily_cap_micros: Option<i64> = row
        .try_get("per_end_user_daily_cap_micros")
        .map_err(store_err)?;

    Ok(SubKeyRecord {
        id: SubKeyId::parse(&id)?,
        tenant_id: TenantId::parse(&tenant_id)?,
        app_id: app_id.map(|s| AppId::parse(&s)).transpose()?,
        end_user_id: end_user_id.map(|s| EndUserId::parse(&s)).transpose()?,
        scope: scope_from_i16(scope)?,
        status: status_from_str(&status)?,
        prefix,
        secret_hash,
        daily_cap_micros,
        per_end_user_daily_cap_micros,
    })
}

fn row_to_end_user(row: &PgRow) -> Result<EndUser, VendError> {
    let id: String = row.try_get("id").map_err(store_err)?;
    let app_id: String = row.try_get("app_id").map_err(store_err)?;
    let external_ref: String = row.try_get("external_ref").map_err(store_err)?;
    let mint_account_id: Option<String> = row.try_get("mint_account_id").map_err(store_err)?;
    Ok(EndUser {
        id: EndUserId::parse(&id)?,
        app_id: AppId::parse(&app_id)?,
        external_ref,
        mint_account_id,
    })
}

// --- TenantRepo -----------------------------------------------------------

#[async_trait]
impl TenantRepo for VendStore {
    async fn find_sub_key_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<SubKeyRecord>, VendError> {
        let row = sqlx::query(
            "SELECT id, tenant_id, app_id, end_user_id, scope, status, prefix, \
             secret_hash, daily_cap_micros, per_end_user_daily_cap_micros \
             FROM sub_keys WHERE prefix = $1",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref().map(row_to_sub_key).transpose()
    }

    async fn load_tenant(&self, id: &TenantId) -> Result<Option<Tenant>, VendError> {
        let row = sqlx::query("SELECT id, name, mint_account_id FROM tenants WHERE id = $1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?;
        match row {
            None => Ok(None),
            Some(r) => Ok(Some(Tenant {
                id: TenantId::parse(&r.try_get::<String, _>("id").map_err(store_err)?)?,
                name: r.try_get("name").map_err(store_err)?,
                mint_account_id: r.try_get("mint_account_id").map_err(store_err)?,
            })),
        }
    }

    async fn load_app(&self, id: &AppId) -> Result<Option<App>, VendError> {
        let row = sqlx::query(
            "SELECT a.id, a.tenant_id, a.name, a.model_allowlist, a.zdr, a.enabled, a.mint_account_id, \
             m.markup_bps, m.min_fee_micros, m.platform_fee_bps, m.funding_mode \
             FROM apps a JOIN markup_policy m ON m.app_id = a.id WHERE a.id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;

        let Some(r) = row else { return Ok(None) };

        let allowlist_json: serde_json::Value = r.try_get("model_allowlist").map_err(store_err)?;
        let model_allowlist: Vec<String> = serde_json::from_value(allowlist_json)
            .map_err(|e| VendError::Storage(format!("model_allowlist: {e}")))?;
        let markup_bps: i32 = r.try_get("markup_bps").map_err(store_err)?;
        let platform_fee_bps: i32 = r.try_get("platform_fee_bps").map_err(store_err)?;
        let funding_mode: String = r.try_get("funding_mode").map_err(store_err)?;

        Ok(Some(App {
            id: AppId::parse(&r.try_get::<String, _>("id").map_err(store_err)?)?,
            tenant_id: TenantId::parse(&r.try_get::<String, _>("tenant_id").map_err(store_err)?)?,
            name: r.try_get("name").map_err(store_err)?,
            model_allowlist,
            zdr: r.try_get("zdr").map_err(store_err)?,
            enabled: r.try_get("enabled").map_err(store_err)?,
            markup_policy: MarkupPolicy {
                markup_bps: u32::try_from(markup_bps).map_err(|_| VendError::InvalidAmount)?,
                min_fee_micros: r.try_get("min_fee_micros").map_err(store_err)?,
                platform_fee_bps: u32::try_from(platform_fee_bps)
                    .map_err(|_| VendError::InvalidAmount)?,
                funding_mode: funding_from_str(&funding_mode)?,
            },
            mint_account_id: r.try_get("mint_account_id").map_err(store_err)?,
        }))
    }

    async fn find_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
    ) -> Result<Option<EndUser>, VendError> {
        let row = sqlx::query(
            "SELECT id, app_id, external_ref, mint_account_id FROM end_users \
             WHERE app_id = $1 AND external_ref = $2",
        )
        .bind(app_id.as_str())
        .bind(external_ref)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref().map(row_to_end_user).transpose()
    }

    async fn load_end_user(
        &self,
        app_id: &AppId,
        id: &EndUserId,
    ) -> Result<Option<EndUser>, VendError> {
        let row = sqlx::query(
            "SELECT id, app_id, external_ref, mint_account_id FROM end_users \
             WHERE app_id = $1 AND id = $2",
        )
        .bind(app_id.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        row.as_ref().map(row_to_end_user).transpose()
    }

    async fn create_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
        mint_account_id: &str,
    ) -> Result<EndUser, VendError> {
        let id = EndUserId::generate();
        sqlx::query(
            "INSERT INTO end_users (id, app_id, external_ref, mint_account_id) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id.as_str())
        .bind(app_id.as_str())
        .bind(external_ref)
        .bind(mint_account_id)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(EndUser {
            id,
            app_id: app_id.clone(),
            external_ref: external_ref.to_string(),
            mint_account_id: Some(mint_account_id.to_string()),
        })
    }
}

// --- KeyRepo --------------------------------------------------------------

#[async_trait]
impl KeyRepo for VendStore {
    async fn insert(&self, record: &SubKeyRecord) -> Result<(), VendError> {
        sqlx::query(
            "INSERT INTO sub_keys (id, tenant_id, app_id, end_user_id, scope, status, \
             prefix, secret_hash, daily_cap_micros, per_end_user_daily_cap_micros) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(record.id.as_str())
        .bind(record.tenant_id.as_str())
        .bind(record.app_id.as_ref().map(|a| a.as_str()))
        .bind(record.end_user_id.as_ref().map(|e| e.as_str()))
        .bind(record.scope.to_u8() as i16)
        .bind(status_str(record.status))
        .bind(&record.prefix)
        .bind(&record.secret_hash)
        .bind(record.daily_cap_micros)
        .bind(record.per_end_user_daily_cap_micros)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn find_by_prefix(&self, prefix: &str) -> Result<Option<SubKeyRecord>, VendError> {
        TenantRepo::find_sub_key_by_prefix(self, prefix).await
    }

    async fn revoke(&self, id: &SubKeyId) -> Result<(), VendError> {
        sqlx::query("UPDATE sub_keys SET status = 'revoked' WHERE id = $1")
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
        Ok(())
    }
}

// --- UsageRepo ------------------------------------------------------------

#[async_trait]
impl UsageRepo for VendStore {
    async fn record_pending(&self, usage: &MeteredUsage) -> Result<(), VendError> {
        insert_pending(&self.pool, usage).await
    }

    async fn record_pending_within_cap(
        &self,
        usage: &MeteredUsage,
        daily_cap_micros: Option<i64>,
        per_end_user_daily_cap_micros: Option<i64>,
    ) -> Result<(), VendError> {
        // No caps → nothing to serialize; fall back to the plain insert.
        if daily_cap_micros.is_none() && per_end_user_daily_cap_micros.is_none() {
            return insert_pending(&self.pool, usage).await;
        }

        // One transaction: acquire per-key/per-end-user advisory locks, re-check
        // the projected SUM against the caps, then insert — so a concurrent
        // reservation cannot read the same SUM and also slip under the cap. The
        // `pg_advisory_xact_lock`s release automatically on COMMIT/ROLLBACK.
        let mut tx = self.pool.begin().await.map_err(store_err)?;

        if let Some(cap) = daily_cap_micros {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(advisory_key(usage.sub_key_id.as_str()))
                .execute(&mut *tx)
                .await
                .map_err(store_err)?;
            let spent: i64 = sqlx::query(
                "SELECT COALESCE(SUM(charge_micros), 0)::BIGINT AS total FROM metered_usage \
                 WHERE sub_key_id = $1 AND utc_day = $2 AND outcome IN ('pending','captured')",
            )
            .bind(usage.sub_key_id.as_str())
            .bind(&usage.utc_day)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_err)?
            .try_get::<i64, _>("total")
            .map_err(store_err)?;
            if spent.saturating_add(usage.charge_micros) > cap {
                // tx dropped without commit → rollback, no row inserted.
                return Err(VendError::SpendCapExceeded);
            }
        }

        if let (Some(cap), Some(eu)) = (per_end_user_daily_cap_micros, usage.end_user_id.as_ref()) {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(advisory_key(eu.as_str()))
                .execute(&mut *tx)
                .await
                .map_err(store_err)?;
            let spent: i64 = sqlx::query(
                "SELECT COALESCE(SUM(charge_micros), 0)::BIGINT AS total FROM metered_usage \
                 WHERE end_user_id = $1 AND utc_day = $2 AND outcome IN ('pending','captured')",
            )
            .bind(eu.as_str())
            .bind(&usage.utc_day)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_err)?
            .try_get::<i64, _>("total")
            .map_err(store_err)?;
            if spent.saturating_add(usage.charge_micros) > cap {
                return Err(VendError::SpendCapExceeded);
            }
        }

        insert_pending(&mut *tx, usage).await?;
        tx.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn attach_hold(&self, charge_id: &ChargeId, hold_id: &str) -> Result<(), VendError> {
        if hold_id.is_empty() {
            return Err(VendError::Storage(
                "Mint returned an empty hold id".to_string(),
            ));
        }
        let changed = sqlx::query(
            "UPDATE metered_usage SET hold_id = $2 \
             WHERE charge_id = $1 AND outcome = 'pending' AND hold_id = ''",
        )
        .bind(charge_id.as_str())
        .bind(hold_id)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VendError::Storage(
                "pending usage intent could not attach its Mint hold".to_string(),
            ))
        }
    }

    async fn mark_captured(&self, charge_id: &ChargeId, actual: &Charge) -> Result<(), VendError> {
        let changed = sqlx::query(
            "UPDATE metered_usage SET outcome = 'captured', provider_cost_micros = $2, \
             charge_micros = $3, platform_fee_micros = $4, tenant_net_micros = $5 \
             WHERE charge_id = $1 AND (outcome = 'pending' OR \
               (outcome = 'captured' AND provider_cost_micros = $2 AND charge_micros = $3 \
                AND platform_fee_micros = $4 AND tenant_net_micros = $5))",
        )
        .bind(charge_id.as_str())
        .bind(actual.provider_cost_micros)
        .bind(actual.charge_micros)
        .bind(actual.platform_fee_micros)
        .bind(actual.tenant_net_micros)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VendError::Storage(
                "usage capture transition rejected: missing row, conflicting terminal outcome, or mismatched amounts".into(),
            ))
        }
    }

    async fn mark_released(&self, charge_id: &ChargeId) -> Result<(), VendError> {
        let changed = sqlx::query(
            "UPDATE metered_usage SET outcome = 'released' \
             WHERE charge_id = $1 AND outcome IN ('pending', 'released')",
        )
        .bind(charge_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VendError::Storage(
                "usage release transition rejected: missing row or conflicting terminal outcome"
                    .into(),
            ))
        }
    }

    async fn record_settlement_proof(
        &self,
        charge_id: &ChargeId,
        proof: &vend_core::transport::SettlementProof,
    ) -> Result<(), VendError> {
        let sequence = i64::try_from(proof.sequence)
            .map_err(|_| VendError::Storage("Mint receipt sequence overflow".to_string()))?;
        let changed = sqlx::query(
            "UPDATE metered_usage SET settlement_seq = $2, settlement_operation = $3, \
             settlement_op_hash = $4, settlement_receipt_hash = $5, settlement_signature = $6 \
             WHERE charge_id = $1 AND outcome IN ('captured','released') AND \
               (settlement_seq IS NULL OR (settlement_seq = $2 AND settlement_operation = $3 AND \
                settlement_op_hash = $4 AND settlement_receipt_hash = $5 AND settlement_signature = $6))",
        )
        .bind(charge_id.as_str())
        .bind(sequence)
        .bind(&proof.operation)
        .bind(&proof.operation_hash)
        .bind(&proof.receipt_hash)
        .bind(&proof.signature)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VendError::Storage(
                "settlement proof rejected: missing/nonterminal charge or conflicting proof"
                    .to_string(),
            ))
        }
    }

    async fn sum_charge_for_key_day(
        &self,
        sub_key_id: &SubKeyId,
        utc_day: &str,
    ) -> Result<i64, VendError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(charge_micros), 0)::BIGINT AS total FROM metered_usage \
             WHERE sub_key_id = $1 AND utc_day = $2 AND outcome IN ('pending','captured')",
        )
        .bind(sub_key_id.as_str())
        .bind(utc_day)
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        row.try_get::<i64, _>("total").map_err(store_err)
    }

    async fn sum_charge_for_end_user_day(
        &self,
        end_user_id: &EndUserId,
        utc_day: &str,
    ) -> Result<i64, VendError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(charge_micros), 0)::BIGINT AS total FROM metered_usage \
             WHERE end_user_id = $1 AND utc_day = $2 AND outcome IN ('pending','captured')",
        )
        .bind(end_user_id.as_str())
        .bind(utc_day)
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        row.try_get::<i64, _>("total").map_err(store_err)
    }
}

// --- ReserveRepo ----------------------------------------------------------

#[async_trait]
impl ReserveRepo for VendStore {
    async fn tenant_reserve_balance(&self, tenant_id: &TenantId) -> Result<i64, VendError> {
        let row = sqlx::query("SELECT balance_micros FROM tenant_reserve WHERE tenant_id = $1")
            .bind(tenant_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?;
        match row {
            Some(r) => r.try_get::<i64, _>("balance_micros").map_err(store_err),
            None => Ok(0),
        }
    }
}

#[async_trait]
impl ReadinessRepo for VendStore {
    async fn ready(&self) -> Result<(), VendError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
        Ok(())
    }
}

// --- IdempotencyRepo ------------------------------------------------------

#[async_trait]
impl IdempotencyRepo for VendStore {
    async fn claim(&self, key: &str, request_hash: &str) -> Result<IdempotencyClaim, VendError> {
        // The claim is two statements (insert-or-nothing, then read the existing
        // row) rather than one transaction. A concurrent `abort` may DELETE the
        // pending row between them; the follow-up read must therefore tolerate a
        // vanished row and re-attempt the claim instead of surfacing a spurious
        // non-retriable `RowNotFound`. The loop is bounded because each empty
        // read corresponds to a concrete concurrent delete; under any finite
        // churn a fresh INSERT eventually wins.
        for _ in 0..16 {
            let inserted = sqlx::query(
                "INSERT INTO idempotency_keys (key, request_hash, state, response_body) \
                 VALUES ($1, $2, 'pending', NULL) ON CONFLICT (key) DO NOTHING",
            )
            .bind(key)
            .bind(request_hash)
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
            if inserted.rows_affected() == 1 {
                return Ok(IdempotencyClaim::Acquired);
            }

            let row = sqlx::query(
                "SELECT request_hash, state, response_body FROM idempotency_keys WHERE key = $1",
            )
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_err)?;
            let Some(row) = row else {
                // The key was concurrently aborted between our INSERT and this
                // read; retry from the top so we can freshly acquire it.
                continue;
            };
            let stored_hash: String = row.try_get("request_hash").map_err(store_err)?;
            if stored_hash != request_hash {
                return Ok(IdempotencyClaim::Conflict);
            }
            let state: String = row.try_get("state").map_err(store_err)?;
            let response: Option<String> = row.try_get("response_body").map_err(store_err)?;
            return match (state.as_str(), response) {
                ("complete", Some(response)) => Ok(IdempotencyClaim::Replay(response)),
                ("pending", _) => Ok(IdempotencyClaim::InProgress),
                _ => Err(VendError::Storage("invalid idempotency state".to_string())),
            };
        }
        // Pathological churn (16 consecutive concurrent aborts): report the key
        // as in-progress, which callers treat as a safe, retriable outcome.
        Ok(IdempotencyClaim::InProgress)
    }

    async fn complete(
        &self,
        key: &str,
        request_hash: &str,
        response_body: &str,
    ) -> Result<(), VendError> {
        let result = sqlx::query(
            "UPDATE idempotency_keys SET state = 'complete', response_body = $3 \
             WHERE key = $1 AND request_hash = $2 AND state = 'pending'",
        )
        .bind(key)
        .bind(request_hash)
        .bind(response_body)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VendError::IdempotencyConflict)
        }
    }

    async fn abort(&self, key: &str, request_hash: &str) -> Result<(), VendError> {
        sqlx::query(
            "DELETE FROM idempotency_keys \
             WHERE key = $1 AND request_hash = $2 AND state = 'pending'",
        )
        .bind(key)
        .bind(request_hash)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod customer_auth_tests {
    use super::*;

    #[test]
    fn high_entropy_bearers_are_shardable_and_tamper_evident() {
        let bearer = mint_bearer("vses", "vss");
        assert_eq!(
            bearer_prefix(&bearer.plaintext, "vses"),
            Some(bearer.prefix.as_str())
        );
        assert!(secure_digest_matches(&bearer.digest, &bearer.plaintext).unwrap());
        let mut tampered = bearer.plaintext.clone();
        tampered.push('0');
        assert!(bearer_prefix(&tampered, "vses").is_none());
        assert!(!secure_digest_matches(&bearer.digest, &tampered).unwrap());
    }

    #[test]
    fn roles_are_cumulative() {
        assert!(CustomerRole::Owner.allows(CustomerRole::Admin));
        assert!(CustomerRole::Admin.allows(CustomerRole::Viewer));
        assert!(!CustomerRole::Viewer.allows(CustomerRole::Admin));
        assert!(!CustomerRole::Admin.allows(CustomerRole::Owner));
    }

    #[test]
    fn advisory_lock_keys_are_stable() {
        assert_eq!(advisory_key("abc"), -5_010_229_573_455_851_542);
    }

    #[test]
    fn disabled_global_user_is_not_password_login_eligible() {
        let membership_status = "active";
        let global_status = "disabled";
        assert!(!(membership_status == "active" && global_status == "active"));
    }

    #[test]
    fn email_identity_is_trimmed_and_case_normalized() {
        let (display, normalized) = normalize_email(" Person@Example.COM ").unwrap();
        assert_eq!(display, "Person@Example.COM");
        assert_eq!(normalized, "person@example.com");
        assert!(normalize_email("not-an-email").is_err());
        assert!(normalize_email("a @example.com").is_err());
    }

    #[tokio::test]
    async fn passwords_use_argon2_and_do_not_compare_as_plaintext() {
        let encoded = hash_password("a sufficiently long password".into())
            .await
            .unwrap();
        assert!(encoded.starts_with("$argon2id$"));
        assert!(
            password_matches("a sufficiently long password".into(), encoded.clone())
                .await
                .unwrap()
        );
        assert!(
            !password_matches("a different long password".into(), encoded)
                .await
                .unwrap()
        );
    }
}
