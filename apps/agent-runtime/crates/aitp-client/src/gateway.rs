//! The payment seam: a trait owned by the agent runtime.
//!
//! This is the decoupling boundary. Today the only implementor is
//! [`crate::stub::StubPaymentGateway`] (Mint AITP-402 == AP3 is not ready).
//! Later a `MintPaymentGateway` (living in apps/mint or a thin adapter) will
//! implement the same trait against `POST /v1/aitp/verify` — the agent runtime
//! never imports Mint.

use async_trait::async_trait;

use crate::amount::AmountMicros;
use crate::error::PayError;
use crate::types::{HoldRef, PaymentAuthorization, PaymentChallenge, Receipt};

/// Caller-side AITP-402 payment gateway (SPEC §4–§6).
///
/// The happy path for metered inference is `authorize` (aitp-hold) →
/// serve the request → `capture` actual usage, or `release` on failure
/// (success-only billing, SPEC ADR-0002).
#[async_trait]
pub trait PaymentGateway: Send + Sync {
    /// Turn a decoded `402` challenge into a signed authorization. For an
    /// `aitp-hold` scheme this also reserves funds and yields a [`HoldRef`].
    async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError>;

    /// Settle a hold for the actual amount spent (SPEC `hold.capture`).
    async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError>;

    /// Release a hold without settling — the failure path where the agent
    /// pays nothing (SPEC `hold.release`).
    async fn release(&self, hold: &HoldRef) -> Result<(), PayError>;
}

/// Result of [`PaymentGateway::authorize`]: the header payload to send and,
/// for hold schemes, the reference used later to capture/release.
#[derive(Debug, Clone)]
pub struct Authorized {
    /// Goes into the `AITP-Authorization` request header (base64url-encoded via
    /// [`crate::codec::encode_authorization`]).
    pub authorization: PaymentAuthorization,
    /// Present for `aitp-hold`; `None` for `aitp-transfer` (already final).
    pub hold: Option<HoldRef>,
}
