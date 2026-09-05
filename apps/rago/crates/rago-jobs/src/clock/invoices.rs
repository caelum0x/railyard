//! Invoice-lifecycle clocks.
//!
//! Ports `Clock::{FinalizeInvoices, RefreshDraftInvoices,
//! MarkInvoicesAsPaymentOverdue, RetryFailedInvoices,
//! RetryGeneratingSubscriptionInvoices}Job`.

use chrono::{Duration, Utc};
use rago_db::Db;
use rago_core::enums::{InvoiceStatus, InvoiceType};
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::queue::JOB_BILL_SUBSCRIPTION;
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// `Clock::FinalizeInvoicesJob` → `Invoices::RefreshDraftAndFinalizeService`.
/// Draft invoices whose issuing date has arrived are finalized and each emits
/// `invoice.created` (the webhook Lago fires when a draft is finalized). Returns
/// the count finalized.
///
/// The finalized rows are captured with `RETURNING`, so a re-run selects nothing
/// (they are no longer draft) and the webhook fires exactly once per invoice.
pub async fn finalize_invoices(db: &Db) -> Result<u64, JobError> {
    let today = Utc::now().date_naive();
    let rows = sqlx::query(
        "UPDATE invoices SET status = $1, updated_at = now() \
         WHERE status = $2 AND (issuing_date IS NULL OR issuing_date <= $3) \
         RETURNING id, organization_id, number, payment_status, currency, total_amount_cents",
    )
    .bind(InvoiceStatus::Finalized.as_i32())
    .bind(InvoiceStatus::Draft.as_i32())
    .bind(today)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    for row in &rows {
        let org: Uuid = row.get("organization_id");
        record_webhook(db, org, WebhookKind::InvoiceCreated, invoice_object(row)).await?;
    }
    Ok(rows.len() as u64)
}

/// `Clock::MarkInvoicesAsPaymentOverdueJob` → per-invoice
/// `Invoices::Payments::MarkOverdueService`, which emits `invoice.payment_overdue`.
/// Finalized, not-succeeded invoices past their payment due date are flagged.
/// Returns the count flagged.
///
/// The ids that will flip are captured *before* the update (matching the same
/// predicate as [`Db::mark_payment_overdue`]); once flagged (`payment_overdue =
/// true`) they are no longer selected, so the webhook fires once per invoice.
pub async fn mark_invoices_as_payment_overdue(db: &Db) -> Result<u64, JobError> {
    let cutoff = Utc::now().naive_utc();
    let overdue = sqlx::query(
        "SELECT id, organization_id, number, payment_status, currency, total_amount_cents \
         FROM invoices \
         WHERE status = 1 AND payment_status <> 1 AND payment_overdue = false \
           AND ((payment_due_date IS NOT NULL AND payment_due_date <= $1::date) \
                OR (payment_due_date IS NULL AND created_at <= $1))",
    )
    .bind(cutoff)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let flagged = db.mark_payment_overdue(cutoff).await?;

    for row in &overdue {
        let org: Uuid = row.get("organization_id");
        record_webhook(db, org, WebhookKind::InvoicePaymentOverdue, invoice_object(row)).await?;
    }
    Ok(flagged)
}

/// A compact `invoice` webhook object from a finalized/overdue invoice row.
fn invoice_object(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let id: Uuid = row.get("id");
    let number: Option<String> = row.get("number");
    let payment_status: Option<i32> = row.get("payment_status");
    let currency: Option<String> = row.get("currency");
    let total: Option<i64> = row.get("total_amount_cents");
    json!({
        "lago_id": id,
        "number": number,
        "payment_status": payment_status,
        "currency": currency,
        "total_amount_cents": total,
    })
}

/// `Clock::RefreshDraftInvoicesJob` → `Invoices::RefreshDraftJob`. Recomputes
/// each draft invoice's monetary totals from its persisted fees, so a draft
/// reflects the latest usage before finalization. Idempotent (a recompute of
/// unchanged fees is a no-op). Returns the count refreshed.
pub async fn refresh_draft_invoices(db: &Db) -> Result<u64, JobError> {
    let r = sqlx::query(
        "WITH agg AS ( \
             SELECT invoice_id, \
                    COALESCE(SUM(amount_cents), 0)::bigint AS fees, \
                    COALESCE(SUM(taxes_amount_cents), 0)::bigint AS taxes \
             FROM fees WHERE invoice_id IS NOT NULL GROUP BY invoice_id \
         ) \
         UPDATE invoices i SET \
             fees_amount_cents = agg.fees, \
             taxes_amount_cents = agg.taxes, \
             sub_total_excluding_taxes_amount_cents = agg.fees, \
             sub_total_including_taxes_amount_cents = agg.fees + agg.taxes, \
             total_amount_cents = agg.fees + agg.taxes, \
             updated_at = now() \
         FROM agg \
         WHERE i.id = agg.invoice_id AND i.status = $1",
    )
    .bind(InvoiceStatus::Draft.as_i32())
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::RetryFailedInvoicesJob` → `Invoices::RetryService`. Invoices whose
/// generation failed are moved back to `generating` so the billing pipeline
/// reprocesses them. Idempotent: once retried they are no longer `failed`, so a
/// re-run selects nothing. Returns the count retried.
pub async fn retry_failed_invoices(db: &Db) -> Result<u64, JobError> {
    let r = sqlx::query(
        "UPDATE invoices SET status = $1, updated_at = now() WHERE status = $2",
    )
    .bind(InvoiceStatus::Generating.as_i32())
    .bind(InvoiceStatus::Failed.as_i32())
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::RetryGeneratingSubscriptionInvoicesJob`. Subscription invoices stuck
/// in `generating` for more than a day are re-driven by enqueuing a durable
/// `bill_subscription` job for the stalled (subscription, period). The billing
/// path's (subscription, period) unique guard makes the re-drive idempotent —
/// no duplicate invoice is produced. Returns the number of jobs enqueued.
pub async fn retry_generating_subscription_invoices(db: &Db) -> Result<u64, JobError> {
    let threshold = (Utc::now() - Duration::days(1)).naive_utc();
    let rows = sqlx::query(
        "SELECT s.organization_id AS org, s.external_id AS ext, \
                COALESCE(isub.to_datetime, isub.timestamp)::date AS billing_date \
         FROM invoices i \
         JOIN invoice_subscriptions isub ON isub.invoice_id = i.id \
         JOIN subscriptions s ON s.id = isub.subscription_id \
         WHERE i.invoice_type = $1 AND i.status = $2 AND i.created_at < $3",
    )
    .bind(InvoiceType::Subscription.as_i32())
    .bind(InvoiceStatus::Generating.as_i32())
    .bind(threshold)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let mut enqueued: u64 = 0;
    for row in &rows {
        let org: Uuid = row.get("org");
        let ext: String = row.get("ext");
        let billing_date: Option<chrono::NaiveDate> = row.get("billing_date");
        let Some(billing_date) = billing_date else { continue };
        let payload = json!({
            "organization_id": org.to_string(),
            "subscription_external_id": ext,
            "billing_date": billing_date.format("%Y-%m-%d").to_string(),
        });
        db.enqueue_job(JOB_BILL_SUBSCRIPTION, &payload, Utc::now().naive_utc(), 5)
            .await?;
        enqueued += 1;
    }
    Ok(enqueued)
}
