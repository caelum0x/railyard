//! Consuming credit-note balances on later invoices (Lago
//! `Credits::CreditNoteService`), and the draft credit notes a draft invoice
//! carries until it is finalized.
//!
//! A finalized, non-voided credit note with `credit_status = available` and a
//! positive `balance_amount_cents` is credit the customer holds. When one of
//! the customer's invoices is finalized, the balances are drawn against the
//! invoice total (after coupons and taxes), oldest note first, each note giving
//! at most what is still due. Every draw writes a `credits` row
//! (`credit_note_id`, `invoice_id`, amount), lowers the note's balance, and
//! marks the note `consumed` once its balance reaches zero. The invoice's
//! `credit_notes_amount_cents` grows and its `total_amount_cents` shrinks by
//! the same amount.
//!
//! Eligibility beyond Lago's `available` scope: the note's own invoice must be
//! paid. A credit note on an unpaid invoice already lowers what that invoice
//! owes (dunning offsets it), so it only becomes spendable on other invoices
//! once its own invoice is settled; this keeps one credit from being counted
//! twice.
//!
//! Everything runs in one transaction with the invoice and the candidate notes
//! locked, and an invoice is credited from notes at most once, so concurrent
//! or repeated finalization never spends a balance twice.

use chrono::{DateTime, Utc};
use rago_billing::credit_notes::{allocate_credit_notes, CreditNoteApplication, CreditNoteBalance};
use rago_core::models::Invoice;
use serde::Serialize;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

/// Lago `CreditNote.status` `draft` / `finalized`.
pub const CREDIT_NOTE_STATUS_DRAFT: i32 = 0;
pub const CREDIT_NOTE_STATUS_FINALIZED: i32 = 1;

/// Invoice types that never consume credit notes: a `credit` invoice is a
/// wallet top-up, paid for with money, not with credit.
const NON_CREDITABLE_INVOICE_TYPES: [i32; 1] = [2];

/// One credit-note draw shown on an invoice (REST `credits`, console).
#[derive(Debug, Clone, Serialize)]
pub struct InvoiceCreditNoteCredit {
    pub id: Uuid,
    pub credit_note_id: Uuid,
    pub credit_note_number: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub created_at: DateTime<Utc>,
}

/// Result of crediting one invoice from the customer's credit notes.
#[derive(Debug, Clone)]
pub struct AppliedCreditNotes {
    pub invoice: Invoice,
    pub applications: Vec<CreditNoteApplication>,
}

/// Flip the draft credit notes of `invoice_id` to finalized and number them
/// after the invoice's final number, on the invoice's finalization
/// transaction. Returns the ids of the notes finalized.
pub(crate) async fn finalize_draft_credit_notes_exec(
    conn: &mut PgConnection,
    invoice_id: Uuid,
    invoice_number: &str,
) -> Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "UPDATE credit_notes SET status = $3, number = $2 || '-CN' || sequential_id::text, \
                updated_at = now() \
         WHERE invoice_id = $1 AND status = $4 AND voided_at IS NULL \
         RETURNING id",
    )
    .bind(invoice_id)
    .bind(invoice_number)
    .bind(CREDIT_NOTE_STATUS_FINALIZED)
    .bind(CREDIT_NOTE_STATUS_DRAFT)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
}

/// Give the credit-note balance consumed by `invoice_id` back to the notes
/// (the invoice is being voided). Voided notes stay voided. Returns the cents
/// restored.
pub(crate) async fn restore_credit_note_balances_exec(
    conn: &mut PgConnection,
    invoice_id: Uuid,
) -> Result<i64> {
    let rows = sqlx::query(
        "SELECT credit_note_id, SUM(amount_cents)::bigint AS amount FROM credits \
         WHERE invoice_id = $1 AND credit_note_id IS NOT NULL GROUP BY credit_note_id",
    )
    .bind(invoice_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut restored = 0;
    for r in &rows {
        let note_id: Uuid = r.try_get("credit_note_id")?;
        let amount: i64 = r.try_get("amount")?;
        let n = sqlx::query(
            "UPDATE credit_notes SET balance_amount_cents = balance_amount_cents + $2, \
                    credit_status = 0, updated_at = now() \
             WHERE id = $1 AND voided_at IS NULL",
        )
        .bind(note_id)
        .bind(amount)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if n == 1 {
            restored += amount;
        }
    }
    Ok(restored)
}

impl Db {
    /// Credit the finalized invoice `invoice_id` from its customer's available
    /// credit-note balances (see the module docs). `Ok(None)` when nothing was
    /// applied: the invoice is not finalized, owes nothing, is a wallet top-up,
    /// was already credited from notes, or the customer holds no usable
    /// balance in its currency.
    pub async fn apply_available_credit_notes(
        &self,
        invoice_id: Uuid,
    ) -> Result<Option<AppliedCreditNotes>> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query(
            "SELECT * FROM invoices WHERE id = $1 AND status = 1 \
               AND NOT (invoice_type = ANY($2)) FOR UPDATE",
        )
        .bind(invoice_id)
        .bind(&NON_CREDITABLE_INVOICE_TYPES[..])
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let mut invoice = mapping::invoice(&row)?;
        let Some(customer_id) = invoice.customer_id else { return Ok(None) };
        if invoice.total_amount_cents <= 0 {
            return Ok(None);
        }
        let already: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM credits WHERE invoice_id = $1 AND credit_note_id IS NOT NULL)",
        )
        .bind(invoice_id)
        .fetch_one(&mut *tx)
        .await?;
        if already {
            return Ok(None);
        }
        let currency = invoice.currency().code();
        let candidates = sqlx::query(
            "SELECT n.id, n.balance_amount_cents FROM credit_notes n \
             JOIN invoices src ON src.id = n.invoice_id \
             WHERE n.organization_id = $1 AND n.customer_id = $2 AND n.invoice_id <> $3 \
               AND n.status = $4 AND n.credit_status = 0 AND n.voided_at IS NULL \
               AND n.balance_amount_cents > 0 AND n.credit_amount_currency = $5 \
               AND src.status = 1 AND src.payment_status = 1 \
             ORDER BY n.issuing_date ASC, n.created_at ASC, n.id ASC \
             FOR UPDATE OF n",
        )
        .bind(invoice.organization_id)
        .bind(customer_id)
        .bind(invoice_id)
        .bind(CREDIT_NOTE_STATUS_FINALIZED)
        .bind(&currency)
        .fetch_all(&mut *tx)
        .await?;
        let balances = candidates
            .iter()
            .map(|r| {
                Ok(CreditNoteBalance {
                    credit_note_id: r.try_get("id")?,
                    balance_amount_cents: r.try_get("balance_amount_cents")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let applications = allocate_credit_notes(invoice.total_amount_cents, &balances);
        if applications.is_empty() {
            return Ok(None);
        }
        for app in &applications {
            sqlx::query(
                "INSERT INTO credits (id, organization_id, invoice_id, credit_note_id, amount_cents, \
                 amount_currency, before_taxes, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, false, now(), now())",
            )
            .bind(Uuid::new_v4())
            .bind(invoice.organization_id)
            .bind(invoice_id)
            .bind(app.credit_note_id)
            .bind(app.amount_cents)
            .bind(&currency)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE credit_notes SET balance_amount_cents = $2, \
                        credit_status = CASE WHEN $2 = 0 THEN 1 ELSE credit_status END, \
                        updated_at = now() \
                 WHERE id = $1",
            )
            .bind(app.credit_note_id)
            .bind(app.remaining_balance_cents)
            .execute(&mut *tx)
            .await?;
        }
        rago_billing::credit_notes::apply_credit_note_applications(&mut invoice, &applications);
        sqlx::query(
            "UPDATE invoices SET credit_notes_amount_cents = $2, total_amount_cents = $3, \
                    payment_status = $4, updated_at = now() WHERE id = $1",
        )
        .bind(invoice_id)
        .bind(invoice.credit_notes_amount_cents)
        .bind(invoice.total_amount_cents)
        .bind(invoice.payment_status.as_i32())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(AppliedCreditNotes { invoice, applications }))
    }

    /// The credit-note draws applied to `invoice_id`, oldest first.
    pub async fn credit_note_credits_for_invoice(
        &self,
        invoice_id: Uuid,
    ) -> Result<Vec<InvoiceCreditNoteCredit>> {
        let rows = sqlx::query(
            "SELECT c.id, c.credit_note_id, n.number, c.amount_cents, c.amount_currency, c.created_at \
             FROM credits c JOIN credit_notes n ON n.id = c.credit_note_id \
             WHERE c.invoice_id = $1 ORDER BY c.created_at ASC, c.id ASC",
        )
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let created: chrono::NaiveDateTime = r.try_get("created_at")?;
                Ok(InvoiceCreditNoteCredit {
                    id: r.try_get("id")?,
                    credit_note_id: r.try_get("credit_note_id")?,
                    credit_note_number: r.try_get("number")?,
                    amount_cents: r.try_get("amount_cents")?,
                    amount_currency: r.try_get("amount_currency")?,
                    created_at: created.and_utc(),
                })
            })
            .collect()
    }

    /// The finalized credit notes of `invoice_id` (for the webhooks announcing
    /// notes finalized together with their invoice).
    pub async fn finalized_credit_note_ids_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<Uuid>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM credit_notes WHERE invoice_id = $1 AND status = $2 ORDER BY sequential_id",
        )
        .bind(invoice_id)
        .bind(CREDIT_NOTE_STATUS_FINALIZED)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }
}
