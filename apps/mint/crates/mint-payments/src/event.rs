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

/// Why a provider took a deposit back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReversalReason {
    /// The merchant (or provider) refunded the payment.
    Refund,
    /// The payer's bank opened a chargeback / dispute.
    Dispute,
}

impl ReversalReason {
    /// Stable lowercase name.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReversalReason::Refund => "refund",
            ReversalReason::Dispute => "dispute",
        }
    }
}

/// A verified provider event that takes back (part of) an already-confirmed
/// deposit: a refund or a chargeback. The handler claws the amount back from
/// the credited account (or records a shortfall for the operator).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReversalEvent {
    /// The provider the original deposit came from.
    pub provider: ProviderId,
    /// The ORIGINAL deposit's raw reference (e.g. the PaymentIntent id),
    /// WITHOUT the `<provider>:` prefix.
    pub deposit_ref: String,
    /// Further raw references the original deposit may have been minted under
    /// (e.g. a Polar order paid via `checkout.updated` mints under the checkout
    /// id). Tried in order after `deposit_ref`; the first one Mint actually
    /// minted wins.
    pub alt_refs: Vec<String>,
    /// The CUMULATIVE amount reversed so far, in microdollars. `None` means
    /// the provider reversed the whole payment (IAP refunds are always full),
    /// so the whole minted deposit is clawed back.
    pub amount_micros: Option<i64>,
    /// Refund or dispute.
    pub reason: ReversalReason,
}

impl ReversalEvent {
    /// The `<provider>:<ref>` key of the original deposit.
    pub fn namespaced_ref(&self) -> String {
        self.provider.namespaced(&self.deposit_ref)
    }

    /// Every candidate `<provider>:<ref>` key, primary first, de-duplicated.
    pub fn namespaced_refs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(1 + self.alt_refs.len());
        for raw in std::iter::once(&self.deposit_ref).chain(self.alt_refs.iter()) {
            if raw.trim().is_empty() {
                continue;
            }
            let key = self.provider.namespaced(raw);
            if !out.contains(&key) {
                out.push(key);
            }
        }
        out
    }
}

/// A verified provider webhook, classified: a deposit to mint, or a reversal
/// (refund / chargeback) of a deposit Mint already minted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VerifiedEvent {
    /// A confirmed on-ramp deposit.
    Deposit(DepositEvent),
    /// A refund or dispute of an earlier deposit.
    Reversal(ReversalEvent),
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
