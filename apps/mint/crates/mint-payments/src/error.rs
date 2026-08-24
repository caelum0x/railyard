//! [`PaymentError`] — the crate-level error surface for the non-webhook call
//! paths (checkout create, re-confirm, payout). Webhook verification has its own
//! [`crate::event::WebhookError`] taxonomy.

use mint_reserve::RampError;

use crate::event::WebhookError;
use crate::provider::ProviderId;

/// A crate-level payment operation failure.
#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    /// The named provider is not configured (no api key / webhook secret).
    #[error("provider not configured: {0}")]
    NotConfigured(ProviderId),
    /// A provider HTTP/transport failure.
    #[error("transport error: {0}")]
    Transport(String),
    /// A webhook verification failure.
    #[error(transparent)]
    Webhook(#[from] WebhookError),
    /// A ramp-level failure surfaced by an adapter.
    #[error("ramp error: {0:?}")]
    Ramp(RampError),
}

impl From<RampError> for PaymentError {
    fn from(e: RampError) -> Self {
        PaymentError::Ramp(e)
    }
}
