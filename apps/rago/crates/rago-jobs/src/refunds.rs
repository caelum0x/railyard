//! Provider refunds — `CreditNotes::Refunds::*CreateService` /
//! `Invoices::RefundService`.
//!
//! A refund is issued at the payment provider that captured the invoice's
//! payment (Stripe `POST /v1/refunds`, GoCardless `POST /refunds`, Adyen
//! `POST /payments/{psp}/refunds`, Creem `POST /v1/refunds`, Dodo
//! `POST /refunds`, Iyzico `/payment/refund`), recorded in the `refunds` ledger, and only
//! then reflected on the payment / invoice / credit note. Nothing is marked
//! refunded unless the provider accepted the refund (or the payment was a
//! manual, off-provider payment, which is recorded as refunded manually).
//!
//! Failures emit `credit_note.refund_failure` for credit-note refunds.

use rago_core::models::{CreditNote, Invoice};
use rago_db::{Db, NewRefund};
use rago_integrations::payments::PaymentProvider;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{
    AdyenProvider, CreemProvider, DodoProvider, GocardlessProvider, HttpTransport, RefundRequest,
    RefundState, StripeProvider,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::collection::resolve_provider_of_kind;
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// The result of a refund attempt.
#[derive(Debug, Clone)]
pub struct RefundResult {
    pub refund_id: Uuid,
    /// `stripe` | `gocardless` | `adyen` | `creem` | `dodo` | `iyzico` | `manual`.
    pub provider: String,
    pub provider_refund_id: Option<String>,
    pub state: RefundState,
    pub amount_cents: i64,
    pub currency: String,
    pub error: Option<String>,
}

/// Why a refund could not be attempted at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefundRejection {
    /// The invoice has no captured payment.
    NoCapturedPayment,
    /// The requested amount exceeds what is still refundable.
    AmountExceedsRefundable { refundable_cents: i64 },
    /// The payment's provider has no stored credentials.
    ProviderNotConfigured(String),
    /// Nothing to refund (`amount <= 0`).
    NothingToRefund,
    /// The provider's API refunds whole payments only (Creem) and the amount
    /// is less than what is still refundable.
    PartialRefundUnsupported { provider: String, refundable_cents: i64 },
}

impl RefundRejection {
    pub fn code(&self) -> String {
        match self {
            RefundRejection::NoCapturedPayment => "no_captured_payment".into(),
            RefundRejection::AmountExceedsRefundable { refundable_cents } => {
                format!("amount_exceeds_refundable ({refundable_cents} cents refundable)")
            }
            RefundRejection::ProviderNotConfigured(p) => format!("payment_provider_not_configured: {p}"),
            RefundRejection::NothingToRefund => "nothing_to_refund".into(),
            RefundRejection::PartialRefundUnsupported { provider, refundable_cents } => format!(
                "partial_refund_unsupported: {provider} refunds whole payments only; refund the full \
                 {refundable_cents} cents or issue the partial refund from the {provider} dashboard"
            ),
        }
    }
}

/// Providers whose refund API cannot refund part of a payment.
const WHOLE_PAYMENT_REFUND_PROVIDERS: &[&str] = &["creem"];

/// Whether refunding `amount_cents` of a payment at `provider` with
/// `refundable_cents` left (and `already_refunded_cents` refunded) is a
/// partial refund the provider's API refuses.
pub fn partial_refund_unsupported(provider: &str, amount_cents: i64, refundable_cents: i64, already_refunded_cents: i64) -> bool {
    WHOLE_PAYMENT_REFUND_PROVIDERS.contains(&provider)
        && (amount_cents < refundable_cents || already_refunded_cents > 0)
}

/// Check, before anything is created, that refunding `amount_cents` of the
/// invoice's captured payment can be done at its provider. `None` = fine (or
/// nothing captured at a provider: a manual refund is recorded instead).
pub async fn refund_precheck(db: &Db, invoice: &Invoice, amount_cents: i64) -> Result<Option<RefundRejection>, JobError> {
    let Some(payment) = db.refundable_payment(invoice.id).await? else {
        return Ok(None);
    };
    let remaining = payment.remaining_cents();
    if amount_cents > remaining {
        return Ok(Some(RefundRejection::AmountExceedsRefundable { refundable_cents: remaining }));
    }
    if partial_refund_unsupported(&payment.provider, amount_cents, remaining, payment.refunded_cents) {
        return Ok(Some(RefundRejection::PartialRefundUnsupported {
            provider: payment.provider.clone(),
            refundable_cents: remaining,
        }));
    }
    Ok(None)
}

/// Lago credit-note reason integer → reason code.
pub fn credit_note_reason(reason: i32) -> &'static str {
    match reason {
        0 => "duplicated_charge",
        1 => "product_unsatisfactory",
        2 => "order_change",
        3 => "order_cancellation",
        4 => "fraudulent_charge",
        _ => "other",
    }
}

/// Refund `amount_cents` (default: everything still refundable) of the
/// invoice's captured payment at its provider.
///
/// `Ok(Err(rejection))` = the refund was not attempted (no payment, amount too
/// large, provider missing). `Ok(Ok(result))` = it was attempted; a provider
/// decline is `result.state == Failed` with `result.error` set. `Err` = a
/// database failure.
pub async fn refund_invoice<T: HttpTransport>(
    db: &Db,
    transport: &T,
    invoice: &Invoice,
    amount_cents: Option<i64>,
    credit_note: Option<&CreditNote>,
) -> Result<Result<RefundResult, RefundRejection>, JobError> {
    let Some(payment) = db.refundable_payment(invoice.id).await? else {
        // Manually recorded (off-provider) payment: the money moved outside
        // Rago, so the refund is recorded as manual.
        return refund_manual(db, invoice, amount_cents, credit_note).await;
    };
    let remaining = payment.remaining_cents();
    let amount = amount_cents.unwrap_or(remaining);
    if amount <= 0 {
        return Ok(Err(RefundRejection::NothingToRefund));
    }
    if amount > remaining {
        return Ok(Err(RefundRejection::AmountExceedsRefundable { refundable_cents: remaining }));
    }
    if partial_refund_unsupported(&payment.provider, amount, remaining, payment.refunded_cents) {
        return Ok(Err(RefundRejection::PartialRefundUnsupported {
            provider: payment.provider.clone(),
            refundable_cents: remaining,
        }));
    }
    let Some(provider) = resolve_provider_of_kind(db, invoice.organization_id, &payment.provider).await? else {
        return Ok(Err(RefundRejection::ProviderNotConfigured(payment.provider.clone())));
    };

    let refund_id = Uuid::new_v4();
    let reference = credit_note
        .map(|c| c.number.clone())
        .unwrap_or_else(|| invoice.number.clone());
    let req = RefundRequest {
        refund_id,
        provider_payment_id: payment.provider_payment_id.clone(),
        amount_cents: amount,
        currency: payment.amount_currency.clone(),
        total_refunded_cents: payment.refunded_cents + amount,
        reason: credit_note.map(|c| credit_note_reason(c.reason).to_string()),
        reference,
    };

    let outcome = match payment.provider.as_str() {
        "stripe" => StripeProvider::new(provider.secret.clone(), transport).refund_payment(&req).await,
        "gocardless" => GocardlessProvider::new(provider.secret.clone(), transport).refund_payment(&req).await,
        "adyen" => {
            let Some(merchant) = provider
                .settings
                .get("merchant_account")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| std::env::var("RAGO_ADYEN_MERCHANT_ACCOUNT").ok().filter(|v| !v.is_empty()))
            else {
                return Ok(Err(RefundRejection::ProviderNotConfigured("adyen merchant_account".into())));
            };
            AdyenProvider::new(provider.secret.clone(), merchant, transport).refund_payment(&req).await
        }
        "creem" | "dodo" => {
            let Some(product_id) = provider
                .settings
                .get("product_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
            else {
                return Ok(Err(RefundRejection::ProviderNotConfigured(format!("{} product_id", payment.provider))));
            };
            let test = provider
                .settings
                .get("mode")
                .and_then(Value::as_str)
                .map(|m| matches!(m, "test" | "sandbox"));
            if payment.provider == "creem" {
                let currency = provider
                    .settings
                    .get("currency")
                    .and_then(Value::as_str)
                    .unwrap_or("USD")
                    .to_string();
                let mut creem = CreemProvider::new(provider.secret.clone(), product_id, currency, transport);
                if let Some(test) = test {
                    creem = creem.with_test_mode(test);
                }
                creem.refund_payment(&req).await
            } else {
                DodoProvider::new(provider.secret.clone(), product_id, test.unwrap_or(false), transport)
                    .refund_payment(&req)
                    .await
            }
        }
        "iyzico" => {
            let Some(iyzico) = crate::collection::iyzico_provider(&provider, invoice.organization_id, transport) else {
                return Ok(Err(RefundRejection::ProviderNotConfigured("iyzico api_key".into())));
            };
            iyzico.refund_payment(&req).await
        }
        other => return Ok(Err(RefundRejection::ProviderNotConfigured(other.to_string()))),
    };

    let (state, provider_refund_id, provider_status, error) = match outcome {
        Ok(o) => (o.state, Some(o.provider_refund_id), Some(o.provider_status), None),
        Err(e) => (RefundState::Failed, None, None, Some(e.to_string())),
    };
    db.insert_refund(&NewRefund {
        id: refund_id,
        organization_id: invoice.organization_id,
        invoice_id: invoice.id,
        payment_id: payment.id,
        credit_note_id: credit_note.map(|c| c.id),
        provider: &payment.provider,
        provider_refund_id: provider_refund_id.as_deref(),
        provider_status: provider_status.as_deref(),
        status: state.as_str(),
        amount_cents: amount,
        amount_currency: &payment.amount_currency,
        last_error: error.as_deref(),
    })
    .await?;

    // A refund that brings the payment to zero flips the payment + invoice to
    // refunded once the provider has confirmed it.
    if state == RefundState::Succeeded && payment.refunded_cents + amount >= payment.amount_cents {
        db.mark_payment_refunded(payment.id, invoice.id).await?;
    }

    let result = RefundResult {
        refund_id,
        provider: payment.provider.clone(),
        provider_refund_id,
        state,
        amount_cents: amount,
        currency: payment.amount_currency.clone(),
        error,
    };
    if let Some(cn) = credit_note {
        apply_to_credit_note(db, invoice, cn, &result).await?;
    }
    Ok(Ok(result))
}

/// Record a refund of a manual (off-provider) payment.
async fn refund_manual(
    db: &Db,
    invoice: &Invoice,
    amount_cents: Option<i64>,
    credit_note: Option<&CreditNote>,
) -> Result<Result<RefundResult, RefundRejection>, JobError> {
    let Some((payment_id, paid_cents, currency)) = db.manual_payment_for_invoice(invoice.id).await? else {
        return Ok(Err(RefundRejection::NoCapturedPayment));
    };
    let amount = amount_cents.unwrap_or(paid_cents);
    if amount <= 0 {
        return Ok(Err(RefundRejection::NothingToRefund));
    }
    if amount > paid_cents {
        return Ok(Err(RefundRejection::AmountExceedsRefundable { refundable_cents: paid_cents }));
    }
    let refund_id = Uuid::new_v4();
    db.insert_refund(&NewRefund {
        id: refund_id,
        organization_id: invoice.organization_id,
        invoice_id: invoice.id,
        payment_id,
        credit_note_id: credit_note.map(|c| c.id),
        provider: "manual",
        provider_refund_id: None,
        provider_status: Some("recorded"),
        status: RefundState::Succeeded.as_str(),
        amount_cents: amount,
        amount_currency: &currency,
        last_error: None,
    })
    .await?;
    if amount >= paid_cents {
        db.mark_payment_refunded(payment_id, invoice.id).await?;
    }
    let result = RefundResult {
        refund_id,
        provider: "manual".into(),
        provider_refund_id: None,
        state: RefundState::Succeeded,
        amount_cents: amount,
        currency,
        error: None,
    };
    if let Some(cn) = credit_note {
        apply_to_credit_note(db, invoice, cn, &result).await?;
    }
    Ok(Ok(result))
}

/// Reflect a refund attempt on its credit note and emit
/// `credit_note.refund_failure` when the provider declined it.
async fn apply_to_credit_note(
    db: &Db,
    invoice: &Invoice,
    cn: &CreditNote,
    result: &RefundResult,
) -> Result<(), JobError> {
    db.update_credit_note_refund_status(cn.id, result.state.credit_note_status()).await?;
    if result.state == RefundState::Failed {
        let _ = record_webhook(
            db,
            invoice.organization_id,
            WebhookKind::CreditNoteRefundFailure,
            json!({
                "lago_credit_note_id": cn.id,
                "credit_note_number": cn.number,
                "lago_invoice_id": invoice.id,
                "lago_customer_id": cn.customer_id,
                "refund_amount_cents": result.amount_cents,
                "provider": result.provider,
                "provider_error": { "message": result.error.clone().unwrap_or_else(|| "refund_failed".into()) },
            }),
        )
        .await;
    }
    Ok(())
}

/// Apply an asynchronous provider refund notification (GoCardless / Adyen /
/// Stripe `refund.updated`). Updates the ledger, the credit note, and — on a
/// completed full refund — the payment + invoice.
pub async fn apply_refund_notification(
    db: &Db,
    organization_id: Uuid,
    provider_refund_id: &str,
    state: RefundState,
    provider_status: &str,
) -> Result<bool, JobError> {
    let Some(row) = db
        .update_refund_status_by_provider_id(provider_refund_id, state.as_str(), provider_status)
        .await?
    else {
        return Ok(false);
    };
    if row.organization_id != organization_id {
        return Ok(false);
    }
    if let Some(cn_id) = row.credit_note_id {
        if let Some(cn) = db.find_credit_note(cn_id).await? {
            if let Some(invoice) = db.find_invoice(row.invoice_id).await? {
                let result = RefundResult {
                    refund_id: row.id,
                    provider: row.provider.clone(),
                    provider_refund_id: row.provider_refund_id.clone(),
                    state,
                    amount_cents: row.amount_cents,
                    currency: row.amount_currency.clone(),
                    error: (state == RefundState::Failed).then(|| provider_status.to_string()),
                };
                apply_to_credit_note(db, &invoice, &cn, &result).await?;
            }
        }
    }
    if state == RefundState::Succeeded {
        if let Some(p) = db.refundable_payment(row.invoice_id).await? {
            if p.id == row.payment_id && p.refunded_cents >= p.amount_cents {
                db.mark_payment_refunded(p.id, row.invoice_id).await?;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creem_refunds_only_whole_payments() {
        assert!(!partial_refund_unsupported("creem", 2_599, 2_599, 0));
        assert!(partial_refund_unsupported("creem", 1_000, 2_599, 0));
        assert!(partial_refund_unsupported("creem", 1_599, 1_599, 1_000));
        assert!(!partial_refund_unsupported("dodo", 1_000, 2_599, 0));
        assert!(RefundRejection::PartialRefundUnsupported { provider: "creem".into(), refundable_cents: 2_599 }
            .code()
            .contains("2599 cents"));
    }
}
