//! White-label ("your own OpenRouter") persistence port: per-model markup
//! overrides, end-user wallet listing, tenant earnings and payout visibility.
//!
//! Money never lives here. Wallet balances are read from Mint; earnings are
//! derived from captured `metered_usage` rows and payout progress from the
//! durable payout-execution projection.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::VendError;
use crate::ids::{AppId, TenantId};
use crate::money::ModelMarkup;

/// One stored per-model markup override.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMarkupEntry {
    pub model: String,
    #[serde(flatten)]
    pub markup: ModelMarkup,
    /// Unix milliseconds of the last change.
    pub updated_at_ms: i64,
}

/// One end-user wallet of an app (Mint balance is fetched separately).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSummary {
    pub end_user_id: String,
    pub external_ref: String,
    pub mint_account_id: Option<String>,
    /// Non-released charges on the given UTC day (spend-cap numerator).
    pub spent_today_micros: i64,
    /// Unix milliseconds the wallet was created.
    pub created_at_ms: i64,
}

/// A tenant's captured revenue for one UTC day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EarningsDay {
    pub utc_day: String,
    pub captured_requests: i64,
    /// What end users paid.
    pub charge_micros: i64,
    /// Upstream provider cost inside those charges.
    pub provider_cost_micros: i64,
    /// Platform fee on the tenant's margin.
    pub platform_fee_micros: i64,
    /// What the tenant keeps (paid out to the tenant's Mint account).
    pub tenant_net_micros: i64,
}

/// One split payout (provider / tenant / platform legs) of a captured charge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutRow {
    pub charge_id: String,
    pub tenant_id: String,
    pub app_id: String,
    pub captured_charge_micros: i64,
    pub provider_cost_micros: i64,
    pub tenant_net_micros: i64,
    pub platform_fee_micros: i64,
    pub provider_transferred: bool,
    pub tenant_transferred: bool,
    pub platform_transferred: bool,
    /// `pending` or `complete`.
    pub state: String,
    pub last_error: Option<String>,
    pub tenant_account_id: String,
    /// Unix milliseconds.
    pub created_at_ms: i64,
}

/// Payout totals for a tenant (or the whole platform).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutTotals {
    pub complete_count: i64,
    pub pending_count: i64,
    /// Tenant-net amount already transferred to tenant accounts.
    pub tenant_paid_micros: i64,
    /// Tenant-net amount still owed on pending payouts.
    pub tenant_pending_micros: i64,
    /// Platform fee already transferred.
    pub platform_paid_micros: i64,
}

#[async_trait]
pub trait WhiteLabelRepo: Send + Sync {
    async fn list_model_markups(&self, app_id: &AppId) -> Result<Vec<ModelMarkupEntry>, VendError>;
    /// Insert or replace the override for `model`.
    async fn set_model_markup(
        &self,
        app_id: &AppId,
        model: &str,
        markup: ModelMarkup,
    ) -> Result<ModelMarkupEntry, VendError>;
    /// Remove the override; `true` when one existed.
    async fn delete_model_markup(&self, app_id: &AppId, model: &str) -> Result<bool, VendError>;
    /// Wallets of an app, newest first.
    async fn list_wallets(
        &self,
        app_id: &AppId,
        utc_day: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WalletSummary>, VendError>;
    /// Captured revenue per UTC day in `[from, to]` (inclusive, `YYYY-MM-DD`).
    async fn tenant_earnings(
        &self,
        tenant_id: &TenantId,
        from: &str,
        to: &str,
    ) -> Result<Vec<EarningsDay>, VendError>;
    /// Recent payouts, newest first; `tenant_id = None` lists every tenant.
    async fn list_payouts(
        &self,
        tenant_id: Option<&TenantId>,
        pending_only: bool,
        limit: i64,
    ) -> Result<Vec<PayoutRow>, VendError>;
    async fn payout_totals(&self, tenant_id: Option<&TenantId>) -> Result<PayoutTotals, VendError>;
}
