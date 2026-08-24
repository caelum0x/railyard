//! Repo PORT traits implemented by `vend-store` (Neon Postgres). These describe
//! Vend's OWN persistence — tenancy/policy/usage/hold-mapping. They are NEVER a
//! money ledger; settlement is on Mint.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::VendError;
use crate::ids::{AppId, ChargeId, EndUserId, SubKeyId, TenantId};
use crate::money::Charge;

/// Terminal state of a metered-usage row. Starts `Pending` at hold time and is
/// driven to `Captured` (2xx-with-usage) or `Released` (any other terminal).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    /// Hold reserved, settlement not yet driven.
    Pending,
    /// Exact actual captured on Mint.
    Captured,
    /// Hold released; no charge.
    Released,
}

/// One metered call: the priced breakdown plus the Mint `hold_id <-> job`
/// mapping. Written `Pending` at hold time; updated at settlement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeteredUsage {
    /// Row id (`chg_<ULID>`).
    pub charge_id: ChargeId,
    /// The key that authorized the call.
    pub sub_key_id: SubKeyId,
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning app.
    pub app_id: AppId,
    /// End-user, if the call was on their behalf.
    pub end_user_id: Option<EndUserId>,
    /// Stable operation prefix; Mint operation keys are derived as
    /// `{idem_key}:hold`, `{idem_key}:capture`, and `{idem_key}:release`.
    pub idem_key: String,
    /// Mint hold id backing this call. Empty only while the durable intent has
    /// been committed and the idempotent Mint hold is being opened.
    pub hold_id: String,
    /// Raw provider cost, in µ$.
    pub provider_cost_micros: i64,
    /// End-user charge, in µ$.
    pub charge_micros: i64,
    /// Platform fee on margin, in µ$.
    pub platform_fee_micros: i64,
    /// Tenant net, in µ$.
    pub tenant_net_micros: i64,
    /// Settlement outcome.
    pub outcome: UsageOutcome,
    /// UTC day bucket (`YYYY-MM-DD`) for spend-cap aggregation.
    pub utc_day: String,
}

/// Reads/writes the `metered_usage` table and the spend-cap aggregates.
#[async_trait]
pub trait UsageRepo: Send + Sync {
    /// Insert a `Pending` usage row. This is the plain insert with NO cap
    /// enforcement — used to seed rows or when the caller has no daily cap.
    async fn record_pending(&self, usage: &MeteredUsage) -> Result<(), VendError>;
    /// Atomically enforce the daily spend cap and insert the `Pending` row in a
    /// SINGLE serialized transaction.
    ///
    /// This is the authoritative, TOCTOU-safe spend-cap gate: it re-evaluates
    /// the projected `SUM(charge_micros) + usage.charge_micros` for the key's
    /// UTC day (and, when `per_end_user_daily_cap_micros` is set, the end-user's
    /// day) against the caps INSIDE the same transaction that inserts the row,
    /// under a per-key/per-end-user lock. Two concurrent reservations therefore
    /// cannot both read the same SUM and both slip under the cap. Returns
    /// [`VendError::SpendCapExceeded`] (rolling back, inserting nothing) when the
    /// projected total would exceed either cap. A `None` cap is unbounded.
    async fn record_pending_within_cap(
        &self,
        usage: &MeteredUsage,
        daily_cap_micros: Option<i64>,
        per_end_user_daily_cap_micros: Option<i64>,
    ) -> Result<(), VendError>;
    /// Attach Mint's returned hold id to a previously committed pending intent.
    /// Implementations must only attach once and only while the row is pending.
    async fn attach_hold(&self, charge_id: &ChargeId, hold_id: &str) -> Result<(), VendError>;
    /// Mark a row `Captured` with the complete rated actual breakdown.
    async fn mark_captured(&self, charge_id: &ChargeId, actual: &Charge) -> Result<(), VendError>;
    /// Mark a row `Released` (no charge).
    async fn mark_released(&self, charge_id: &ChargeId) -> Result<(), VendError>;
    /// `SUM(charge_micros)` for a key on a UTC day (spend-cap numerator).
    async fn sum_charge_for_key_day(
        &self,
        sub_key_id: &SubKeyId,
        utc_day: &str,
    ) -> Result<i64, VendError>;
    /// `SUM(charge_micros)` for an end-user on a UTC day.
    async fn sum_charge_for_end_user_day(
        &self,
        end_user_id: &EndUserId,
        utc_day: &str,
    ) -> Result<i64, VendError>;
}

/// Reads the tenant's local reserve mirror (a cache of Mint's snapshot used for
/// fast pre-checks; the authoritative reserve is always Mint).
#[async_trait]
pub trait ReserveRepo: Send + Sync {
    /// Last-known available reserve balance for a tenant, in µ$.
    async fn tenant_reserve_balance(&self, tenant_id: &TenantId) -> Result<i64, VendError>;
}

/// Minimal dependency probe used by the HTTP readiness endpoint.
#[async_trait]
pub trait ReadinessRepo: Send + Sync {
    /// Succeeds only when Vend's authoritative Postgres metadata store can
    /// serve a query. Mint and Relay failures remain request-level failures.
    async fn ready(&self) -> Result<(), VendError>;
}

/// Idempotency-key persistence for the edge (dedupes retried client requests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotencyClaim {
    /// This caller atomically acquired the key and may perform side effects.
    Acquired,
    /// A terminal response already exists and must be replayed verbatim.
    Replay(String),
    /// The same request is currently executing; callers must not duplicate it.
    InProgress,
    /// This key belongs to a different request body.
    Conflict,
}

#[async_trait]
pub trait IdempotencyRepo: Send + Sync {
    /// Atomically acquire a request key or observe its existing state.
    async fn claim(&self, key: &str, request_hash: &str) -> Result<IdempotencyClaim, VendError>;
    /// Store the terminal replay after all external settlement is complete.
    async fn complete(
        &self,
        key: &str,
        request_hash: &str,
        response_body: &str,
    ) -> Result<(), VendError>;
    /// Remove an acquired claim only when no billable upstream work occurred.
    async fn abort(&self, key: &str, request_hash: &str) -> Result<(), VendError>;
}
