//! [`CompositeRamp`] — a [`Ramp`] that dispatches `confirm_deposit` /
//! `initiate_payout` to the right provider adapter by the `<provider>:` tag
//! prefix on the ref, so multiple live providers coexist behind ONE injected
//! ramp. An unknown prefix ⇒ [`RampError::DepositNotFound`]. The reserve keeps
//! its `"mint:{deposit_ref}"` idempotency key, so the prefix also namespaces
//! dedupe across providers.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};

use crate::provider::ProviderId;

/// Routes ramp calls by the `<provider>:` prefix on the ref.
pub struct CompositeRamp {
    ramps: HashMap<ProviderId, Arc<dyn Ramp>>,
}

impl CompositeRamp {
    /// Build a composite over a table of per-provider adapters.
    pub fn new(ramps: HashMap<ProviderId, Arc<dyn Ramp>>) -> Self {
        CompositeRamp { ramps }
    }

    /// Split a `"<provider>:<raw_ref>"` value into its provider + raw ref.
    fn route<'a>(&self, tagged_ref: &'a str) -> Result<(&Arc<dyn Ramp>, &'a str), RampError> {
        let (tag, rest) = tagged_ref
            .split_once(':')
            .ok_or(RampError::DepositNotFound)?;
        let provider = ProviderId::from_tag(tag).ok_or(RampError::DepositNotFound)?;
        let ramp = self
            .ramps
            .get(&provider)
            .ok_or(RampError::DepositNotFound)?;
        Ok((ramp, rest))
    }
}

#[async_trait]
impl Ramp for CompositeRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let (ramp, raw) = self.route(deposit_ref)?;
        // The inner adapter talks to the provider with the RAW (unprefixed) ref.
        ramp.confirm_deposit(raw).await
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        let (ramp, raw) = self.route(payout_ref)?;
        ramp.initiate_payout(raw, dest, amount).await
    }

    async fn check_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        // An unknown / inactive `<provider>:` prefix can never be paid out.
        let (ramp, raw) = self.route(payout_ref).map_err(|_| RampError::Unsupported)?;
        if raw.trim().is_empty() {
            return Err(RampError::Unsupported);
        }
        ramp.check_payout(raw, dest, amount).await
    }

    async fn payout_status(
        &self,
        payout_ref: &str,
        provider_ref: Option<&str>,
    ) -> Result<PayoutStatus, RampError> {
        let (ramp, raw) = self.route(payout_ref)?;
        ramp.payout_status(raw, provider_ref).await
    }
}
