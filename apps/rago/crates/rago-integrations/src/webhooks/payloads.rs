//! Bespoke (non-plain-model) webhook payload serializers — faithful ports of the
//! `V1::…ErrorSerializer` / checkout / event-error classes that back the error,
//! checkout, and validation webhook kinds. Each returns the inner object `Value`
//! that [`super::WebhookKind::build`] nests under the kind's `object_type`.
//!
//! Plain model kinds (customer, invoice, subscription, wallet, plan, fee,
//! credit_note, …) are NOT here: their object is the resource's own V1 serializer
//! output, produced in `rago-graphql`/`rago-db`, and passed straight to
//! [`super::WebhookKind::build`].

use serde_json::{json, Value};

/// Normalize an optional string into a JSON value (`null` when absent), matching
/// Ruby `options[:key]` returning `nil`.
fn opt(s: Option<&str>) -> Value {
    match s {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

/// `V1::PaymentProviders::CustomerCheckoutSerializer`
/// (`customer.checkout_url_generated` → `payment_provider_customer_checkout_url`).
pub fn customer_checkout_url(
    lago_customer_id: &str,
    external_customer_id: &str,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    checkout_url: &str,
) -> Value {
    json!({
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "checkout_url": checkout_url,
    })
}

/// `V1::PaymentProviders::CustomerErrorSerializer`
/// (`customer.payment_provider_error` → `payment_provider_customer_error`).
///
/// The accounting (`accounting_provider_customer_error`) and CRM
/// (`crm_provider_customer_error`) variants share this exact shape — only the
/// enclosing `webhook_type`/`object_type` differ (see [`super::WebhookKind`]).
pub fn payment_provider_customer_error(
    lago_customer_id: &str,
    external_customer_id: &str,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::PaymentProviders::ErrorSerializer` (`payment_provider.error` →
/// `payment_provider_error`). `source`/`action` are hoisted out of
/// `provider_error`; the remainder stays nested.
pub fn payment_provider_error(
    lago_payment_provider_id: &str,
    payment_provider_code: Option<&str>,
    payment_provider_name: Option<&str>,
    mut provider_error: Value,
) -> Value {
    let source = provider_error
        .as_object_mut()
        .and_then(|m| m.remove("source"))
        .unwrap_or(Value::Null);
    let action = provider_error
        .as_object_mut()
        .and_then(|m| m.remove("action"))
        .unwrap_or(Value::Null);
    json!({
        "lago_payment_provider_id": lago_payment_provider_id,
        "payment_provider_code": opt(payment_provider_code),
        "payment_provider_name": opt(payment_provider_name),
        "source": source,
        "action": action,
        "provider_error": provider_error,
    })
}

/// `V1::PaymentProviders::InvoicePaymentErrorSerializer` (`invoice.payment_failure`
/// → `payment_provider_invoice_payment_error`).
#[allow(clippy::too_many_arguments)]
pub fn invoice_payment_error(
    lago_invoice_id: &str,
    lago_customer_id: &str,
    external_customer_id: &str,
    provider_customer_id: Option<&str>,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    provider_error: Value,
    error_details: Value,
) -> Value {
    json!({
        "lago_invoice_id": lago_invoice_id,
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "provider_customer_id": opt(provider_customer_id),
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "provider_error": provider_error,
        "error_details": error_details,
    })
}

/// `V1::PaymentProviders::PaymentRequestPaymentErrorSerializer`
/// (`payment_request.payment_failure` →
/// `payment_provider_payment_request_payment_error`).
#[allow(clippy::too_many_arguments)]
pub fn payment_request_payment_error(
    lago_payment_request_id: &str,
    lago_invoice_ids: Vec<String>,
    lago_customer_id: &str,
    external_customer_id: &str,
    provider_customer_id: Option<&str>,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_payment_request_id": lago_payment_request_id,
        "lago_invoice_ids": lago_invoice_ids,
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "provider_customer_id": opt(provider_customer_id),
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::PaymentProviders::WalletTransactionPaymentErrorSerializer`
/// (`wallet_transaction.payment_failure` →
/// `payment_provider_wallet_transaction_payment_error`).
pub fn wallet_transaction_payment_error(
    lago_wallet_transaction_id: &str,
    lago_customer_id: &str,
    external_customer_id: &str,
    provider_customer_id: Option<&str>,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_wallet_transaction_id": lago_wallet_transaction_id,
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "provider_customer_id": opt(provider_customer_id),
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::CreditNotes::PaymentProviderRefundErrorSerializer`
/// (`credit_note.refund_failure` → `credit_note_payment_provider_refund_error`).
pub fn credit_note_refund_error(
    lago_credit_note_id: &str,
    lago_customer_id: &str,
    external_customer_id: &str,
    provider_customer_id: Option<&str>,
    payment_provider: Option<&str>,
    payment_provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_credit_note_id": lago_credit_note_id,
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "provider_customer_id": opt(provider_customer_id),
        "payment_provider": opt(payment_provider),
        "payment_provider_code": opt(payment_provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::Integrations::ProviderErrorSerializer` (`integration.provider_error` →
/// `provider_error`).
pub fn integration_provider_error(
    lago_integration_id: &str,
    provider: Option<&str>,
    provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_integration_id": lago_integration_id,
        "provider": opt(provider),
        "provider_code": opt(provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::Integrations::Taxes::CustomerErrorSerializer`
/// (`customer.tax_provider_error` → `tax_provider_customer_error`).
pub fn tax_customer_error(
    lago_customer_id: &str,
    external_customer_id: &str,
    tax_provider: Option<&str>,
    tax_provider_code: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "lago_customer_id": lago_customer_id,
        "external_customer_id": external_customer_id,
        "tax_provider": opt(tax_provider),
        "tax_provider_code": opt(tax_provider_code),
        "provider_error": provider_error,
    })
}

/// `V1::Integrations::Taxes::FeeErrorSerializer` (`fee.tax_provider_error` →
/// `tax_provider_fee_error`).
pub fn tax_fee_error(
    tax_provider_code: Option<&str>,
    lago_charge_id: Option<&str>,
    event_transaction_id: Option<&str>,
    provider_error: Value,
) -> Value {
    json!({
        "tax_provider_code": opt(tax_provider_code),
        "lago_charge_id": opt(lago_charge_id),
        "event_transaction_id": opt(event_transaction_id),
        "provider_error": provider_error,
    })
}

/// `V1::EventErrorSerializer` (`event.error` → `event_error`). `error` is any
/// JSON value; `message` is its JSON-string encoding (Ruby `error.to_json`). The
/// serialized `event` is merged in at the top level.
pub fn event_error(error: &Value, event: Value) -> Value {
    json!({
        "status": 422,
        "error": "Unprocessable entity",
        "message": serde_json::to_string(error).unwrap_or_default(),
        "event": event,
    })
}

/// `V1::EventsValidationErrorsSerializer` (`events.errors` → `events_errors`).
/// Each field defaults to `null` when the errors hash omits it.
pub fn events_errors(errors: &Value) -> Value {
    let get = |k: &str| errors.get(k).cloned().unwrap_or(Value::Null);
    json!({
        "invalid_code": get("invalid_code"),
        "missing_aggregation_property": get("missing_aggregation_property"),
        "missing_group_key": get("missing_group_key"),
        "invalid_filter_values": get("invalid_filter_values"),
    })
}

/// `V1::Invoices::PaymentDisputeLostSerializer` (`invoice.payment_dispute_lost` →
/// `payment_dispute_lost`). `invoice` is the V1 invoice serializer output
/// (includes customer); `provider_error` is only present when non-empty.
pub fn payment_dispute_lost(invoice: Value, provider_error: Option<Value>) -> Value {
    let mut out = json!({ "invoice": invoice });
    if let Some(pe) = provider_error {
        if !pe.is_null() {
            out.as_object_mut()
                .expect("object")
                .insert("provider_error".into(), pe);
        }
    }
    out
}
