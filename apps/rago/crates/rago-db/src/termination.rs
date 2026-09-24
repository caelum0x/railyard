//! Persistence for mid-period terminations: the pay-in-advance subscription
//! fee a period was billed upfront, the amount already credited on a fee, and
//! the exactly-once credit note for the unused days of that fee.

use chrono::NaiveDateTime;
use rago_core::models::{CreditNote, Fee, Invoice, Subscription};
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

/// SQL sum of the non-voided credit-note items issued on a fee.
const CREDITED_ON_FEE_SQL: &str = "SELECT COALESCE(SUM(i.amount_cents), 0)::bigint AS credited \
     FROM credit_note_items i JOIN credit_notes n ON n.id = i.credit_note_id \
     WHERE i.fee_id = $1 AND n.voided_at IS NULL";

impl Db {
    /// The subscription fee billed on the recurring invoice of
    /// `(subscription, [from, to])` (the period's re-bill guard key) and that
    /// invoice, unless it was voided. For a pay-in-advance plan this is the fee
    /// the customer paid upfront for the period.
    pub async fn subscription_fee_for_period(
        &self,
        subscription_id: Uuid,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Result<Option<(Fee, Invoice)>> {
        let row = sqlx::query(
            "SELECT f.id AS fee_id, f.invoice_id AS fee_invoice_id FROM fees f \
             JOIN invoice_subscriptions s ON s.invoice_id = f.invoice_id \
             JOIN invoices i ON i.id = f.invoice_id \
             WHERE s.subscription_id = $1 AND s.recurring IS TRUE \
               AND s.charges_from_datetime = $2 AND s.charges_to_datetime = $3 \
               AND f.subscription_id = $1 AND f.fee_type = 2 AND i.status <> 2 \
             ORDER BY f.created_at ASC LIMIT 1",
        )
        .bind(subscription_id)
        .bind(from)
        .bind(to)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let fee_id: Uuid = row.try_get("fee_id")?;
        let invoice_id: Uuid = row.try_get("fee_invoice_id")?;
        let fee = sqlx::query("SELECT * FROM fees WHERE id = $1")
            .bind(fee_id)
            .fetch_one(&self.pool)
            .await?;
        let invoice = sqlx::query("SELECT * FROM invoices WHERE id = $1")
            .bind(invoice_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Some((mapping::fee(&fee)?, mapping::invoice(&invoice)?)))
    }

    /// Cents already credited on `fee_id` by non-voided credit notes.
    pub async fn credited_amount_on_fee(&self, fee_id: Uuid) -> Result<i64> {
        let row = sqlx::query(CREDITED_ON_FEE_SQL).bind(fee_id).fetch_one(&self.pool).await?;
        Ok(row.try_get("credited")?)
    }

    /// Persist `credit_note` (with its items) crediting `fee_id`, numbered as
    /// the invoice's next credit note, only if the fee's credited amount is
    /// still `expected_credited` — the amount the credit was computed against.
    /// The fee row is locked for the check, so two concurrent runs never both
    /// credit the same unused days. Returns `false` (nothing written) when the
    /// fee was credited meanwhile.
    pub async fn insert_fee_credit_note(
        &self,
        credit_note: &mut CreditNote,
        fee_id: Uuid,
        expected_credited: i64,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM fees WHERE id = $1 FOR UPDATE")
            .bind(fee_id)
            .fetch_one(&mut *tx)
            .await?;
        let credited: i64 = sqlx::query(CREDITED_ON_FEE_SQL)
            .bind(fee_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("credited")?;
        if credited != expected_credited {
            tx.rollback().await?;
            return Ok(false);
        }
        let seq: i32 = sqlx::query(
            "SELECT COALESCE(MAX(sequential_id), 0) + 1 AS n FROM credit_notes WHERE invoice_id = $1",
        )
        .bind(credit_note.invoice_id)
        .fetch_one(&mut *tx)
        .await?
        .try_get("n")?;
        let invoice_number: Option<String> = sqlx::query("SELECT number FROM invoices WHERE id = $1")
            .bind(credit_note.invoice_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("number")?;
        let invoice_number = invoice_number.unwrap_or_default();
        credit_note.sequential_id = seq;
        credit_note.number = format!("{invoice_number}-CN{seq}");
        let cn = &*credit_note;
        sqlx::query(
            "INSERT INTO credit_notes (id, organization_id, customer_id, invoice_id, sequential_id, \
             number, issuing_date, reason, credit_status, refund_status, status, credit_amount_cents, \
             credit_amount_currency, balance_amount_cents, refund_amount_cents, total_amount_cents, \
             total_amount_currency, taxes_amount_cents, taxes_rate, coupons_adjustment_amount_cents, \
             precise_coupons_adjustment_amount_cents, precise_taxes_amount_cents, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22, now(), now())",
        )
        .bind(cn.id).bind(cn.organization_id).bind(cn.customer_id).bind(cn.invoice_id).bind(cn.sequential_id)
        .bind(&cn.number).bind(cn.issuing_date).bind(cn.reason).bind(cn.credit_status).bind(cn.refund_status)
        .bind(cn.status).bind(cn.credit_amount_cents).bind(&cn.credit_amount_currency).bind(cn.balance_amount_cents)
        .bind(cn.refund_amount_cents).bind(cn.total_amount_cents).bind(&cn.total_amount_currency)
        .bind(cn.taxes_amount_cents).bind(cn.taxes_rate).bind(cn.coupons_adjustment_amount_cents)
        .bind(cn.precise_coupons_adjustment_amount_cents).bind(cn.precise_taxes_amount_cents)
        .execute(&mut *tx)
        .await?;
        for item in &cn.items {
            sqlx::query(
                "INSERT INTO credit_note_items (id, organization_id, credit_note_id, fee_id, amount_cents, \
                 precise_amount_cents, amount_currency, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
            )
            .bind(item.id)
            .bind(item.organization_id)
            .bind(cn.id)
            .bind(item.fee_id)
            .bind(item.amount_cents)
            .bind(item.precise_amount_cents)
            .bind(&item.amount_currency)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Active subscriptions of pay-in-advance plans with a fee and no free
    /// trial that started within the last year (a yearly plan's first period
    /// at most) — the candidates for a first-period invoice on starting.
    pub async fn starting_pay_in_advance_subscriptions(
        &self,
        now: NaiveDateTime,
    ) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(
            "SELECT s.* FROM subscriptions s JOIN plans p ON p.id = s.plan_id \
             WHERE s.status = 1 AND p.pay_in_advance AND COALESCE(p.amount_cents, 0) > 0 \
               AND COALESCE(p.trial_period, 0) = 0 AND s.started_at IS NOT NULL \
               AND s.started_at <= $1 AND s.started_at > $1 - INTERVAL '367 days' \
             ORDER BY s.started_at ASC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::subscription).collect()
    }
}
