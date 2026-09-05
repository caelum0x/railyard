//! Payment providers — the abstraction plus Stripe (fully implemented).
//!
//! Faithful port of `PaymentProviders::BaseService` + the provider `payments/
//! create_service.rb` flow: a payment intent is created at the provider, its
//! status is mapped to the invoice's `payable_payment_status`, and the invoice
//! `payment_status` is updated accordingly.
//!
//! GoCardless and Adyen implement the same [`PaymentProvider`] trait; only their
//! constructors exist here and they return [`PaymentError::NotImplemented`] —
//! they are flagged as follow-ups, never faked.

pub mod adyen;
pub mod gocardless;
pub mod provider_webhook;
pub mod stripe;
pub mod stripe_webhook;

use async_trait::async_trait;
use rago_core::enums::PaymentStatus;
use uuid::Uuid;

use crate::transport::TransportError;

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
}
