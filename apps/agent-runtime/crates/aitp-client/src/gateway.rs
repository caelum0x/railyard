//! The payment seam: a trait owned by the agent runtime.
//!
//! This is the decoupling boundary. Implementors:
//! - [`crate::mint::MintPaymentGateway`] — the production gateway: talks to a
//!   live mint-server over HTTP (`POST /v1/aitp/verify`, then
//!   `/v1/holds/:id/capture` or `/v1/holds/:id/release`) through the injectable
//!   [`crate::transport::HttpTransport`] seam, signing authorizations with the
//!   agent's Ed25519 key;
//! - [`crate::stub::StubPaymentGateway`] — an in-process fake for tests and the
//!   network-free `agent-runtime-demo`.
//!
//! The agent runtime depends only on this trait, never on Mint's crates.

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

    /// What this gateway remembers about `hold` between `authorize` and
    /// `capture`/`release`, as JSON, so a durable capture journal can settle
    /// the hold after a restart. `None` when there is nothing to remember.
    fn hold_context(&self, _hold: &HoldRef) -> Option<serde_json::Value> {
        None
    }

    /// Restore a context saved by [`PaymentGateway::hold_context`] (after a
    /// restart, before settling the hold).
    fn restore_hold_context(&self, _hold: &HoldRef, _context: &serde_json::Value) -> Result<(), PayError> {
        Ok(())
    }
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
