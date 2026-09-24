//! The post-finalization pipeline shared by every path that finalizes an
//! invoice (the FinalizeInvoices clock, REST `PUT /invoices/:id/finalize`,
//! GraphQL `finalizeInvoice` / `finalizeAllInvoices`) and by the billing cycle
//! when it issues an invoice already finalized.
//!
//! Mirrors Lago's `Invoices::FinalizeService` tail: provider taxes (Anrok /
//! Avalara, when the organization connected one), the `invoice.created` +
//! `invoice.generated` webhooks, then real-time wallet settlement (captures the
//! Mint holds reserved for the invoice's events and applies them as prepaid
//! credit), then payment collection of whatever is left through the durable
//! `invoices.collect` job.

use chrono::Utc;
use rago_core::enums::PaymentStatus;
use rago_core::models::Invoice;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;
use uuid::Uuid;

use crate::scheduler::invoice_webhook_object;
use crate::webhook_emit::{record_invoice_generated, record_webhook};
use crate::JobError;

/// Apply the organization's tax provider to `invoice` when one is connected,
/// returning the invoice with its provider-computed totals. A provider failure
/// is not fatal to finalization: it is logged, `customer.tax_provider_error` /
/// `fee.tax_provider_error` are emitted by the tax sync, and the invoice keeps
/// its organization-tax totals.
pub async fn apply_provider_taxes_if_configured(db: &Db, invoice: Invoice) -> Result<Invoice, JobError> {
    let Some(provider) = crate::integrations_sync::active_tax_provider(db, invoice.organization_id).await? else {
        return Ok(invoice);
    };
    let transport = rago_integrations::default_transport();
    match crate::integrations_sync::apply_provider_taxes_to_invoice(
        db,
        &transport,
        invoice.organization_id,
        invoice.id,
        &provider,
    )
    .await
    {
        Ok((taxed, _fees)) => Ok(taxed),
        Err(e) => {
            tracing::warn!(
                invoice_id = %invoice.id,
                organization_id = %invoice.organization_id,
                provider = %provider,
                error = %e,
                "provider taxes could not be applied at finalization; keeping organization taxes"
            );
            Ok(invoice)
        }
    }
}

/// Queue the durable collection job for a finalized invoice that has
/// something to collect.
pub async fn enqueue_collection(db: &Db, invoice: &Invoice) -> Result<bool, JobError> {
    if invoice.total_amount_cents <= 0
        || matches!(invoice.payment_status, PaymentStatus::Succeeded | PaymentStatus::Refunded)
    {
        return Ok(false);
    }
    db.enqueue_job(
        crate::queue::JOB_COLLECT_INVOICE,
        &json!({ "invoice_id": invoice.id.to_string() }),
        Utc::now().naive_utc(),
        5,
    )
    .await?;
    Ok(true)
}

/// Everything that follows a draft → finalized transition: provider taxes,
/// then (when `emit_webhooks`) `invoice.created` + `invoice.generated` carrying
/// the final number and totals, then collection. Returns the invoice as
/// finalized (with provider taxes applied).
pub async fn on_invoice_finalized(db: &Db, invoice: Invoice, emit_webhooks: bool) -> Result<Invoice, JobError> {
    let invoice = apply_provider_taxes_if_configured(db, invoice).await?;
    if emit_webhooks {
        let object = invoice_webhook_object(&invoice);
        record_webhook(db, invoice.organization_id, WebhookKind::InvoiceCreated, object.clone()).await?;
        record_invoice_generated(db, invoice.organization_id, invoice.id, object).await?;
    }
    let invoice = settle_realtime_wallets(db, invoice).await?;
    enqueue_collection(db, &invoice).await?;
    Ok(invoice)
}

/// Capture the Mint holds reserved for this invoice (real-time wallets) and
/// return the invoice with the captured amount applied. A Mint outage leaves
/// the holds reserved and the invoice collectable as usual.
async fn settle_realtime_wallets(db: &Db, invoice: Invoice) -> Result<Invoice, JobError> {
    let transport = rago_integrations::default_transport();
    match crate::wallet_realtime::settle_invoice(db, &transport, &invoice).await {
        Ok(0) => Ok(invoice),
        Ok(applied) => {
            tracing::info!(invoice_id = %invoice.id, applied_cents = applied, "real-time wallet holds captured");
            Ok(db.find_invoice(invoice.id).await?.unwrap_or(invoice))
        }
        Err(e) => {
            tracing::error!(invoice_id = %invoice.id, error = %e, "real-time wallet settlement failed");
            Ok(invoice)
        }
    }
}

/// Finalize one draft (numbering it in the same transaction) and run the
/// post-finalization pipeline. `Ok(None)` when it was not finalizable (already
/// finalized, voided, deleted, ...), so a concurrent finalizer never
/// double-emits or double-collects.
pub async fn finalize_invoice(db: &Db, invoice_id: Uuid) -> Result<Option<Invoice>, JobError> {
    match db.finalize_draft(invoice_id).await? {
        Some(finalized) => Ok(Some(on_invoice_finalized(db, finalized, true).await?)),
        None => Ok(None),
    }
}

/// Finalize every draft of an organization (GraphQL `finalizeAllInvoices`).
/// Returns the invoices this call finalized.
pub async fn finalize_all_invoices(db: &Db, organization_id: Uuid) -> Result<Vec<Invoice>, JobError> {
    let finalized = db.finalize_all_drafts(organization_id).await?;
    let mut out = Vec::with_capacity(finalized.len());
    for invoice in finalized {
        out.push(on_invoice_finalized(db, invoice, true).await?);
    }
    Ok(out)
}
