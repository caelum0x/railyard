//! Provider refunds: the captured payment a refund applies to, the running
//! refunded total, and the `refunds` ledger that records what the provider
//! actually accepted.

use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A captured payment that can be refunded.
#[derive(Debug, Clone)]
pub struct RefundablePayment {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub provider: String,
    pub provider_payment_id: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    /// Sum of non-failed refunds already issued against this payment.
    pub refunded_cents: i64,
}

impl RefundablePayment {
    /// Amount still refundable at the provider.
    pub fn remaining_cents(&self) -> i64 {
        (self.amount_cents - self.refunded_cents).max(0)
    }
}

/// A recorded refund.
#[derive(Debug, Clone)]
pub struct RefundRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub payment_id: Uuid,
    pub credit_note_id: Option<Uuid>,
    pub provider: String,
    pub provider_refund_id: Option<String>,
    pub status: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub last_error: Option<String>,
}

/// A new refund row.
#[derive(Debug, Clone)]
pub struct NewRefund<'a> {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub payment_id: Uuid,
    pub credit_note_id: Option<Uuid>,
    pub provider: &'a str,
    pub provider_refund_id: Option<&'a str>,
    pub provider_status: Option<&'a str>,
    pub status: &'a str,
    pub amount_cents: i64,
    pub amount_currency: &'a str,
    pub last_error: Option<&'a str>,
}

fn refund_row(r: &sqlx::postgres::PgRow) -> RefundRow {
    RefundRow {
        id: r.get("id"),
        organization_id: r.get("organization_id"),
        invoice_id: r.get("invoice_id"),
        payment_id: r.get("payment_id"),
        credit_note_id: r.get("credit_note_id"),
        provider: r.get("provider"),
        provider_refund_id: r.get("provider_refund_id"),
        status: r.get("status"),
        amount_cents: r.get("amount_cents"),
        amount_currency: r.get("amount_currency"),
        last_error: r.get("last_error"),
    }
}

const REFUND_COLS: &str = "id, organization_id, invoice_id, payment_id, credit_note_id, provider, \
     provider_refund_id, status, amount_cents, amount_currency, last_error";

impl Db {
    /// The invoice's captured (succeeded, or partially refunded) provider
    /// payment, with the amount already refunded against it.
    pub async fn refundable_payment(&self, invoice_id: Uuid) -> Result<Option<RefundablePayment>> {
        let row = sqlx::query(
            "SELECT p.id, p.organization_id, p.invoice_id, p.provider, p.provider_payment_id, \
                    p.amount_cents, p.amount_currency, \
                    COALESCE((SELECT SUM(r.amount_cents) FROM refunds r \
                              WHERE r.payment_id = p.id AND r.status <> 'failed'), 0)::bigint AS refunded_cents \
             FROM payments p \
             WHERE p.invoice_id = $1 AND p.status IN ('succeeded', 'refunded') \
               AND p.provider_payment_id IS NOT NULL AND p.provider <> 'manual' \
             ORDER BY p.updated_at DESC LIMIT 1",
        )
        .bind(invoice_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| RefundablePayment {
            id: r.get("id"),
            organization_id: r.get("organization_id"),
            invoice_id: r.get("invoice_id"),
            provider: r.get("provider"),
            provider_payment_id: r.get("provider_payment_id"),
            amount_cents: r.get("amount_cents"),
            amount_currency: r.get("amount_currency"),
            refunded_cents: r.get("refunded_cents"),
        }))
    }

    /// The invoice's manually recorded (off-provider) succeeded payment:
    /// `(payment_id, still_refundable_cents, currency)`.
    pub async fn manual_payment_for_invoice(&self, invoice_id: Uuid) -> Result<Option<(Uuid, i64, String)>> {
        let row = sqlx::query(
            "SELECT p.id, p.amount_currency, \
                    (p.amount_cents - COALESCE((SELECT SUM(r.amount_cents) FROM refunds r \
                      WHERE r.payment_id = p.id AND r.status <> 'failed'), 0))::bigint AS remaining \
             FROM payments p \
             WHERE p.invoice_id = $1 AND p.provider = 'manual' AND p.status = 'succeeded' \
             ORDER BY p.updated_at DESC LIMIT 1",
        )
        .bind(invoice_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("id"), r.get::<i64, _>("remaining").max(0), r.get("amount_currency"))))
    }

    /// Persist a refund attempt.
    pub async fn insert_refund(&self, r: &NewRefund<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO refunds (id, organization_id, invoice_id, payment_id, credit_note_id, provider, \
             provider_refund_id, provider_status, status, amount_cents, amount_currency, last_error, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, now(), now()) \
             ON CONFLICT (id) DO UPDATE SET provider_refund_id = EXCLUDED.provider_refund_id, \
                 provider_status = EXCLUDED.provider_status, status = EXCLUDED.status, \
                 last_error = EXCLUDED.last_error, updated_at = now()",
        )
        .bind(r.id)
        .bind(r.organization_id)
        .bind(r.invoice_id)
        .bind(r.payment_id)
        .bind(r.credit_note_id)
        .bind(r.provider)
        .bind(r.provider_refund_id)
        .bind(r.provider_status)
        .bind(r.status)
        .bind(r.amount_cents)
        .bind(r.amount_currency)
        .bind(r.last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The refund recorded for a credit note, if any.
    pub async fn refund_for_credit_note(&self, credit_note_id: Uuid) -> Result<Option<RefundRow>> {
        let row = sqlx::query(&format!("SELECT {REFUND_COLS} FROM refunds WHERE credit_note_id = $1 LIMIT 1"))
            .bind(credit_note_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(refund_row))
    }

    /// Refunds issued against an invoice, newest first.
    pub async fn refunds_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<RefundRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {REFUND_COLS} FROM refunds WHERE invoice_id = $1 ORDER BY created_at DESC"
        ))
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(refund_row).collect())
    }

    /// Update a refund's status from a provider notification; returns the row.
    pub async fn update_refund_status_by_provider_id(
        &self,
        provider_refund_id: &str,
        status: &str,
        provider_status: &str,
    ) -> Result<Option<RefundRow>> {
        let row = sqlx::query(&format!(
            "UPDATE refunds SET status = $2, provider_status = $3, updated_at = now() \
             WHERE provider_refund_id = $1 RETURNING {REFUND_COLS}"
        ))
        .bind(provider_refund_id)
        .bind(status)
        .bind(provider_status)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(refund_row))
    }

    /// Remove a FAILED refund attempt so it can be retried (credit notes carry
    /// at most one refund).
    pub async fn delete_failed_refund(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM refunds WHERE id = $1 AND status = 'failed'")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Mark a payment fully refunded and flip the invoice `payment_status` to
    /// refunded (3).
    pub async fn mark_payment_refunded(&self, payment_id: Uuid, invoice_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE payments SET status = 'refunded', updated_at = now() WHERE id = $1")
            .bind(payment_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE invoices SET payment_status = 3, updated_at = now() WHERE id = $1")
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
