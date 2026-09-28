//! Shadow mode (`MINT_SHADOW=1`, RY-MT1): Mint runs on its production Postgres
//! with the full API, receipts and background sweeps, but no money can move.
//!
//! - Every deposit confirmation is refused, so `POST /v1/mint` can never credit
//!   an account (the deposit gate answers `DepositNotConfirmed`).
//! - Every payout is refused BEFORE the burn (`check_payout`), so `POST
//!   /v1/redeem` never destroys funds for a payout that cannot be sent.
//! - The binary refuses to start in shadow mode when a payment provider is
//!   configured or `MINT_DEV_FUNDING=1` is set: a hosted checkout could take a
//!   buyer's money that the refusing ramp would then never credit.

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Deposit, Payout, PayoutDest, Ramp, RampError};

/// `true` when `MINT_SHADOW=1`.
pub fn shadow_enabled() -> bool {
    std::env::var("MINT_SHADOW").ok().as_deref() == Some("1")
}

/// The funding ramp installed in shadow mode: refuses every deposit and payout.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShadowRamp;

#[async_trait]
impl Ramp for ShadowRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        tracing::warn!(
            deposit_ref,
            "shadow mode: deposit refused (no provider, no money moves)"
        );
        Err(RampError::Unsupported)
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<Payout, RampError> {
        tracing::warn!(payout_ref, "shadow mode: payout refused");
        Err(RampError::Unsupported)
    }

    async fn check_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<(), RampError> {
        Err(RampError::Unsupported)
    }
}
