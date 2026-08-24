//! [`DepositEvent`] — the verifier's output that a webhook handler feeds into
//! `reserve.mint`, and [`WebhookError`] — the outcome taxonomy the handler maps
//! onto HTTP status codes.

use mint_reserve::Asset;

use crate::provider::ProviderId;

/// A verified, confirmed on-ramp deposit extracted from a provider webhook
/// (AFTER signature verification, BEFORE any minting). The handler turns this
/// into a `MintRequest { to: parse(account_id), amount_micros, deposit_ref:
/// "<provider>:<ref>", idem_key: "<provider>:<ref>" }`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DepositEvent {
    /// The provider this deposit came from.
    pub provider: ProviderId,
    /// The provider's raw deposit reference (PaymentIntent id, charge code,
    /// transaction id, …) WITHOUT the `<provider>:` prefix.
    pub deposit_ref: String,
    /// The confirmed deposit amount in microdollars (`cents × 10_000`).
    pub amount_micros: i64,
    /// The settlement asset (`Usd` for card/IAP/fiat, `Usdc` for on-chain).
    pub asset: Asset,
    /// The Mint account id to credit, resolved from provider metadata
    /// (`metadata.account_id` / `app_user_id` / `custom_id` / `conversationId`).
    pub account_id: String,
}

impl DepositEvent {
    /// The `<provider>:<ref>` namespaced key used for both `deposit_ref` and
    /// `idem_key` on the `MintRequest` (namespaces dedupe across providers).
    pub fn namespaced_ref(&self) -> String {
        self.provider.namespaced(&self.deposit_ref)
    }
}

/// The outcome of verifying a provider webhook. The handler maps:
/// `BadSignature` → 401, `NotConfigured` → 500 (`"provider not configured"`),
/// `Ignored(_)` → 200 no-op, `Malformed(_)` → 400.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum WebhookError {
    /// The signature / MAC / shared-secret did not verify over the RAW bytes.
    #[error("bad_signature")]
    BadSignature,
    /// The provider has no configured signing secret (route returns 500).
    #[error("not_configured")]
    NotConfigured,
    /// A well-formed but non-crediting event (wrong event type, unrelated
    /// status). Acknowledged 200 with no mint.
    #[error("ignored: {0}")]
    Ignored(String),
    /// A signature-verified body that could not be parsed into a deposit.
    #[error("malformed: {0}")]
    Malformed(String),
}
