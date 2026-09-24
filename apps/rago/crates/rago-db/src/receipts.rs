//! Payment receipts (one numbered receipt per settled payment) and draft
//! invoice deletion.

use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A `payment_receipts` row.
#[derive(Debug, Clone)]
pub struct PaymentReceiptRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub number: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub created_at: DateTime<Utc>,
}

fn receipt_row(r: &sqlx::postgres::PgRow) -> Result<PaymentReceiptRow> {
    Ok(PaymentReceiptRow {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        payment_id: r.try_get("payment_id")?,
        invoice_id: r.try_get("invoice_id")?,
        number: r.try_get("number")?,
        amount_cents: r.try_get("amount_cents")?,
        amount_currency: r.try_get("amount_currency")?,
        created_at: r.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
    })
}

impl Db {
    /// Issue the receipt of a succeeded payment — exactly once per payment.
    /// The receipt number is `<invoice number>-RCPT-<n>`. Returns `None` when
    /// the payment is unknown, not succeeded, or already has its receipt.
    pub async fn issue_payment_receipt(&self, payment_id: Uuid) -> Result<Option<PaymentReceiptRow>> {
        let mut tx = self.pool.begin().await?;
        let payment = sqlx::query(
            "SELECT p.id, p.organization_id, p.invoice_id, p.amount_cents, p.amount_currency, i.number \
             FROM payments p JOIN invoices i ON i.id = p.invoice_id \
             WHERE p.id = $1 AND p.status = 'succeeded' FOR UPDATE OF p",
        )
        .bind(payment_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(p) = payment else {
            tx.rollback().await?;
            return Ok(None);
        };
        let invoice_id: Uuid = p.try_get("invoice_id")?;
        let invoice_number: String = p.try_get("number")?;
        let seq: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint + 1 FROM payment_receipts WHERE invoice_id = $1",
        )
        .bind(invoice_id)
        .fetch_one(&mut *tx)
        .await?;
        let row = sqlx::query(
            "INSERT INTO payment_receipts (id, organization_id, payment_id, invoice_id, number, \
             amount_cents, amount_currency, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,now()) \
             ON CONFLICT (payment_id) DO NOTHING RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(p.try_get::<Uuid, _>("organization_id")?)
        .bind(payment_id)
        .bind(invoice_id)
        .bind(format!("{invoice_number}-RCPT-{seq}"))
        .bind(p.try_get::<i64, _>("amount_cents")?)
        .bind(p.try_get::<String, _>("amount_currency")?)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(receipt_row).transpose()
    }

    /// Receipts of an invoice, oldest first.
    pub async fn payment_receipts_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<PaymentReceiptRow>> {
        let rows = sqlx::query(
            "SELECT * FROM payment_receipts WHERE invoice_id = $1 ORDER BY created_at ASC",
        )
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(receipt_row).collect()
    }

    /// Receipts of an organization, newest first (paginated).
    pub async fn list_payment_receipts_for_org(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PaymentReceiptRow>> {
        let rows = sqlx::query(
            "SELECT * FROM payment_receipts WHERE organization_id = $1 \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(receipt_row).collect()
    }

    /// Delete a DRAFT invoice with its fees (and release its billing-period
    /// reservation so the period can be billed again). Finalized invoices are
    /// never deleted — they are voided. Returns whether a draft was deleted.
    pub async fn delete_draft_invoice(&self, org: Uuid, invoice_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let draft: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM invoices WHERE id = $1 AND organization_id = $2 AND status = 0 FOR UPDATE",
        )
        .bind(invoice_id)
        .bind(org)
        .fetch_optional(&mut *tx)
        .await?;
        // A draft that already moved wallet credit is not deletable (void it).
        let wallet_moves: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM wallet_transactions WHERE invoice_id = $1",
        )
        .bind(invoice_id)
        .fetch_one(&mut *tx)
        .await?;
        if draft.is_none() || wallet_moves > 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM fees WHERE invoice_id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM invoice_subscriptions WHERE invoice_id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        // Billing bookkeeping that points at the draft: a deleted progressive
        // invoice no longer counts as billed usage, a period-end draft that
        // credited progressive invoices releases them for the next invoice,
        // and pay-in-advance events billed on the draft can be billed again.
        sqlx::query("DELETE FROM progressive_billing_invoices WHERE invoice_id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE progressive_billing_invoices SET credited_invoice_id = NULL \
             WHERE credited_invoice_id = $1",
        )
        .bind(invoice_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM pay_in_advance_billings WHERE invoice_id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM invoices WHERE id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
}
