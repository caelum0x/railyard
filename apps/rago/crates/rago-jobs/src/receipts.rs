//! Payment receipts — `PaymentReceipts::CreateService` + `GenerateService`.
//!
//! Every settled payment gets one numbered receipt; its PDF is rendered on
//! request at `/api/v1/invoices/:invoice_id/receipt`.

use rago_db::Db;
use serde_json::json;
use uuid::Uuid;

use crate::webhook_emit::{record_webhook, PUBLIC_URL_ENV};
use crate::JobError;
use rago_integrations::webhooks::WebhookKind;

/// URL of an invoice's payment-receipt PDF.
pub fn receipt_url(invoice_id: Uuid) -> String {
    let path = format!("/api/v1/invoices/{invoice_id}/receipt");
    match std::env::var(PUBLIC_URL_ENV).ok().filter(|v| !v.trim().is_empty()) {
        Some(base) => format!("{}{path}", base.trim_end_matches('/')),
        None => path,
    }
}

/// Issue the receipt of a succeeded payment (idempotent) and queue
/// `payment_receipt.created` + `payment_receipt.generated`.
pub async fn issue_for_payment(db: &Db, payment_id: Uuid) -> Result<(), JobError> {
    let Some(receipt) = db.issue_payment_receipt(payment_id).await? else {
        return Ok(());
    };
    let object = json!({
        "lago_id": receipt.id,
        "number": receipt.number,
        "lago_payment_id": receipt.payment_id,
        "lago_invoice_id": receipt.invoice_id,
        "amount_cents": receipt.amount_cents,
        "amount_currency": receipt.amount_currency,
        "created_at": receipt.created_at.to_rfc3339(),
    });
    record_webhook(db, receipt.organization_id, WebhookKind::PaymentReceiptCreated, object.clone()).await?;
    let mut generated = object;
    if let serde_json::Value::Object(map) = &mut generated {
        map.insert("file_url".into(), json!(receipt_url(receipt.invoice_id)));
    }
    record_webhook(db, receipt.organization_id, WebhookKind::PaymentReceiptGenerated, generated).await?;
    Ok(())
}
