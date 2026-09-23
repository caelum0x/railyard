//! Payment providers — the abstraction plus Stripe, GoCardless and Adyen (all
//! fully implemented).
//!
//! Faithful port of `PaymentProviders::BaseService` + the provider `payments/
//! create_service.rb` flow: a payment intent is created at the provider, its
//! status is mapped to the invoice's `payable_payment_status`, and the invoice
//! `payment_status` is updated accordingly.
//!
//! Stripe, GoCardless and Adyen each implement the same [`PaymentProvider`]
//! trait against the real provider HTTP API (see [`stripe`], [`gocardless`],
//! [`adyen`]). [`PaymentError::NotImplemented`] is retained as a variant for any
//! provider added in the future but is not returned by the providers above.

pub mod adyen;
pub mod gocardless;
pub mod provider_webhook;
pub mod stripe;
pub mod stripe_webhook;

use async_trait::async_trait;
use rago_core::enums::PaymentStatus;
use uuid::Uuid;

use crate::transport::TransportError;

/// Provider idempotency key for attempt `attempt` (0-based) of a payment:
/// `payment-<id>` for the first attempt, `payment-<id>-<n>` for retries. A
/// replay of the same attempt reuses the key (deduplicated by the provider); a
/// legitimate retry of a declined charge gets a fresh key, because providers
/// cache 4xx results (e.g. Stripe `card_declined`) under a key for 24h.
/// Provider idempotency key for a refund.
pub fn refund_idempotency_key(refund_id: &Uuid) -> String {
    format!("refund-{refund_id}")
}

pub fn idempotency_key(payment_id: &Uuid, attempt: u32) -> String {
    if attempt == 0 {
        format!("payment-{payment_id}")
    } else {
        format!("payment-{payment_id}-{attempt}")
    }
}

/// A request to charge an invoice.
#[derive(Debug, Clone)]
pub struct PaymentRequest {
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub customer_id: Uuid,
    /// Provider-side customer id (e.g. Stripe `cus_…`).
    pub provider_customer_id: String,
    pub amount_cents: i64,
    /// ISO currency code (e.g. `USD`).
    pub currency: String,
    /// Human-readable reference (invoice number).
    pub reference: String,
    /// Provider mandate id (GoCardless direct-debit mandate), when applicable.
    pub provider_mandate_id: Option<String>,
}

impl PaymentRequest {
    /// Build a minimal request (no mandate).
    pub fn new(
        payment_id: Uuid,
        invoice_id: Uuid,
        customer_id: Uuid,
        provider_customer_id: impl Into<String>,
        amount_cents: i64,
        currency: impl Into<String>,
        reference: impl Into<String>,
    ) -> Self {
        Self {
            payment_id,
            invoice_id,
            customer_id,
            provider_customer_id: provider_customer_id.into(),
            amount_cents,
            currency: currency.into(),
            reference: reference.into(),
            provider_mandate_id: None,
        }
    }
}

/// The outcome of a provider charge.
#[derive(Debug, Clone)]
pub struct PaymentOutcome {
    /// Provider payment id (e.g. Stripe `pi_…`).
    pub provider_payment_id: String,
    /// Raw provider status string (e.g. `succeeded`, `requires_action`).
    pub provider_status: String,
    /// The mapped payable payment status.
    pub payable_status: PayableStatus,
    /// What the customer must do to complete the payment (Stripe SCA / 3DS
    /// `next_action`, e.g. `redirect_to_url`), when the provider needs it.
    pub next_action: Option<serde_json::Value>,
}

/// `Payment#payable_payment_status` — the invoice-facing status a provider
/// status maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayableStatus {
    Pending,
    Succeeded,
    Failed,
}

impl PayableStatus {
    /// The invoice `payment_status` this maps to (`pending` stays pending).
    pub fn invoice_payment_status(self) -> Option<PaymentStatus> {
        match self {
            PayableStatus::Succeeded => Some(PaymentStatus::Succeeded),
            PayableStatus::Failed => Some(PaymentStatus::Failed),
            PayableStatus::Pending => Some(PaymentStatus::Pending),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("provider `{0}` is not implemented")]
    NotImplemented(&'static str),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("provider error {code:?}: {message}")]
    Provider { code: Option<String>, message: String },
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),
}

/// A payment provider (`PaymentProviders::*::Payments::CreateService`).
#[async_trait]
pub trait PaymentProvider: Send + Sync {
    /// Provider identifier (`stripe`, `gocardless`, `adyen`).
    fn payment_type(&self) -> &'static str;

    /// Create and confirm a payment for the invoice, returning the mapped
    /// outcome.
    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError>;

    /// Refund (part of) a previously captured payment at the provider
    /// (`PaymentProviders::*::Refunds::CreateService`).
    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        let _ = req;
        Err(PaymentError::NotImplemented(self.payment_type()))
    }
}

/// A request to refund a captured payment.
#[derive(Debug, Clone)]
pub struct RefundRequest {
    /// Rago refund id (idempotency key `refund-<id>`).
    pub refund_id: Uuid,
    /// Provider payment id the refund applies to (Stripe `pi_…`, GoCardless
    /// `PM…`, Adyen PSP reference).
    pub provider_payment_id: String,
    pub amount_cents: i64,
    /// ISO currency code.
    pub currency: String,
    /// Total refunded on this payment INCLUDING this refund (GoCardless
    /// `total_amount_confirmation`).
    pub total_refunded_cents: i64,
    /// Lago credit-note reason (`duplicated_charge`, `fraudulent_charge`, …).
    pub reason: Option<String>,
    /// Human-readable reference (credit-note or invoice number).
    pub reference: String,
}

/// The provider-side state of a refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundState {
    Pending,
    Succeeded,
    Failed,
}

impl RefundState {
    pub fn as_str(self) -> &'static str {
        match self {
            RefundState::Pending => "pending",
            RefundState::Succeeded => "succeeded",
            RefundState::Failed => "failed",
        }
    }

    /// The credit-note `refund_status` integer (`pending`=0, `succeeded`=1,
    /// `failed`=2).
    pub fn credit_note_status(self) -> i32 {
        match self {
            RefundState::Pending => 0,
            RefundState::Succeeded => 1,
            RefundState::Failed => 2,
        }
    }
}

/// The outcome of a provider refund.
#[derive(Debug, Clone)]
pub struct RefundOutcome {
    pub provider_refund_id: String,
    pub provider_status: String,
    pub state: RefundState,
}
