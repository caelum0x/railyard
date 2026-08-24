//! Durable split-payout projection. Mint remains the money ledger; this state
//! records which retry-safe external operations Vend still needs to drive.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ChargeId, VendError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutState {
    Pending,
    Complete,
    /// Mint did not capture the hold, so this pre-capture plan is permanently
    /// neutralized and must never execute transfers.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutLeg {
    Provider,
    Tenant,
    Platform,
}

/// One immutable payout plan and its locally observed Mint progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutExecution {
    pub charge_id: ChargeId,
    pub idem_key: String,
    pub clearing_account_id: String,
    pub provider_account_id: String,
    pub tenant_account_id: String,
    pub platform_account_id: String,
    pub provider_cost_micros: i64,
    pub tenant_net_micros: i64,
    pub platform_fee_micros: i64,
    pub captured_charge_micros: i64,
    pub provider_transferred: bool,
    pub tenant_transferred: bool,
    pub platform_transferred: bool,
    pub state: PayoutState,
    pub last_error: Option<String>,
}

#[async_trait]
pub trait PayoutRepo: Send + Sync {
    /// Insert the immutable plan before capture. An identical existing plan is
    /// a successful replay; a different plan for the charge is a conflict.
    async fn prepare_payout(&self, payout: &PayoutExecution) -> Result<(), VendError>;
    async fn load_payout(&self, charge_id: &ChargeId)
        -> Result<Option<PayoutExecution>, VendError>;
    /// Oldest incomplete plans whose local projection has not advanced since
    /// `older_than_unix_seconds`, bounded for one reconciliation pass.
    async fn pending_payouts_before(
        &self,
        older_than_unix_seconds: i64,
        limit: u32,
    ) -> Result<Vec<PayoutExecution>, VendError>;
    async fn mark_payout_leg(&self, charge_id: &ChargeId, leg: PayoutLeg) -> Result<(), VendError>;
    async fn mark_payout_complete(&self, charge_id: &ChargeId) -> Result<(), VendError>;
    async fn record_payout_error(
        &self,
        charge_id: &ChargeId,
        message: &str,
    ) -> Result<(), VendError>;
}
