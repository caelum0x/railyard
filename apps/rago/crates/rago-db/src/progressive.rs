//! Progressive billing persistence: usage thresholds (plan-level defaults and
//! per-subscription overrides), the progressive invoices issued inside a
//! billing period, lifetime-usage inputs, and the weighted-sum carry-in.

use chrono::{DateTime, NaiveDateTime, Utc};
use rago_core::enums::{AggregationType, FeeType, InvoiceStatus, InvoiceType};
use rago_core::models::{BillableMetric, Fee, Invoice};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::{insert_fee_exec, insert_invoice_exec, Db};

/// A `usage_thresholds` row.
#[derive(Debug, Clone)]
pub struct UsageThresholdRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub subscription_id: Option<Uuid>,
    pub threshold_display_name: Option<String>,
    pub amount_cents: i64,
    pub recurring: bool,
    pub created_at: DateTime<Utc>,
}

/// A threshold to (re)define on a plan or subscription.
#[derive(Debug, Clone)]
pub struct NewUsageThreshold {
    pub threshold_display_name: Option<String>,
    pub amount_cents: i64,
    pub recurring: bool,
}

/// What a threshold set is attached to.
#[derive(Debug, Clone, Copy)]
pub enum ThresholdOwner {
    Plan(Uuid),
    Subscription(Uuid),
}

/// A progressive invoice issued inside a billing period.
#[derive(Debug, Clone)]
pub struct ProgressiveInvoiceRow {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub usage_amount_cents: i64,
    pub threshold_ids: Vec<Uuid>,
    pub credited_invoice_id: Option<Uuid>,
}

fn threshold_row(r: &sqlx::postgres::PgRow) -> Result<UsageThresholdRow> {
    Ok(UsageThresholdRow {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        plan_id: r.try_get("plan_id")?,
        subscription_id: r.try_get("subscription_id")?,
        threshold_display_name: r.try_get("threshold_display_name")?,
        amount_cents: r.try_get("amount_cents")?,
        recurring: r.try_get("recurring")?,
        created_at: r.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
    })
}

/// Validate a threshold set: positive amounts, at most one recurring threshold,
/// no duplicate fixed amounts.
pub fn validate_thresholds(items: &[NewUsageThreshold]) -> std::result::Result<(), &'static str> {
    if items.iter().any(|t| t.amount_cents <= 0) {
        return Err("amount_cents_must_be_positive");
    }
    if items.iter().filter(|t| t.recurring).count() > 1 {
        return Err("only_one_recurring_threshold");
    }
    let mut fixed: Vec<i64> = items.iter().filter(|t| !t.recurring).map(|t| t.amount_cents).collect();
    fixed.sort_unstable();
    if fixed.windows(2).any(|w| w[0] == w[1]) {
        return Err("duplicate_threshold_amount");
    }
    Ok(())
}

impl Db {
    /// Thresholds attached directly to `owner` (ordered fixed-ascending, then recurring).
    pub async fn usage_thresholds_for(&self, owner: ThresholdOwner) -> Result<Vec<UsageThresholdRow>> {
        let (col, id) = match owner {
            ThresholdOwner::Plan(id) => ("plan_id", id),
            ThresholdOwner::Subscription(id) => ("subscription_id", id),
        };
        // `col` is one of two static column names, never user input.
        let sql = format!(
            "SELECT * FROM usage_thresholds WHERE {col} = $1 AND deleted_at IS NULL \
             ORDER BY recurring ASC, amount_cents ASC"
        );
        let rows = sqlx::query(&sql).bind(id).fetch_all(&self.pool).await?;
        rows.iter().map(threshold_row).collect()
    }

    /// The thresholds in force for a subscription: its own override set when it
    /// has one, otherwise its plan's.
    pub async fn effective_usage_thresholds(
        &self,
        subscription_id: Uuid,
        plan_id: Uuid,
    ) -> Result<Vec<UsageThresholdRow>> {
        let own = self.usage_thresholds_for(ThresholdOwner::Subscription(subscription_id)).await?;
        if !own.is_empty() {
            return Ok(own);
        }
        self.usage_thresholds_for(ThresholdOwner::Plan(plan_id)).await
    }

    /// Replace the threshold set of `owner` atomically (an empty set clears it).
    pub async fn replace_usage_thresholds(
        &self,
        organization_id: Uuid,
        owner: ThresholdOwner,
        items: &[NewUsageThreshold],
    ) -> Result<Vec<UsageThresholdRow>> {
        let (plan_id, subscription_id) = match owner {
            ThresholdOwner::Plan(id) => (Some(id), None),
            ThresholdOwner::Subscription(id) => (None, Some(id)),
        };
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE usage_thresholds SET deleted_at = now(), updated_at = now() \
             WHERE deleted_at IS NULL AND organization_id = $1 \
               AND (plan_id = $2 OR subscription_id = $3)",
        )
        .bind(organization_id)
        .bind(plan_id)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        for t in items {
            sqlx::query(
                "INSERT INTO usage_thresholds (id, organization_id, plan_id, subscription_id, \
                 threshold_display_name, amount_cents, recurring, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
            )
            .bind(Uuid::new_v4())
            .bind(organization_id)
            .bind(plan_id)
            .bind(subscription_id)
            .bind(t.threshold_display_name.as_deref())
            .bind(t.amount_cents)
            .bind(t.recurring)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.usage_thresholds_for(owner).await
    }

    /// Subscriptions that have thresholds in force (own or via their plan).
    pub async fn subscriptions_with_usage_thresholds(&self) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT DISTINCT s.id FROM subscriptions s \
             JOIN usage_thresholds t ON t.deleted_at IS NULL \
               AND (t.subscription_id = s.id OR t.plan_id = s.plan_id) \
             WHERE s.status = 1",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
    }

    /// `LifetimeUsage#invoiced_usage_amount_cents` — usage (charge fees) billed
    /// on the subscription's earlier, non-voided period invoices, excluding the
    /// period `[from, to]` itself.
    pub async fn invoiced_usage_amount_cents(
        &self,
        subscription_id: Uuid,
        current_from: NaiveDateTime,
    ) -> Result<i64> {
        let v: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(SUM(f.amount_cents), 0)::bigint FROM fees f \
             JOIN invoices i ON i.id = f.invoice_id \
             JOIN invoice_subscriptions s ON s.invoice_id = i.id AND s.recurring IS TRUE \
             WHERE f.subscription_id = $1 AND f.fee_type = $2 \
               AND i.invoice_type = $3 AND i.status <> $4 \
               AND s.charges_to_datetime < $5",
        )
        .bind(subscription_id)
        .bind(FeeType::Charge.as_i32())
        .bind(InvoiceType::Subscription.as_i32())
        .bind(InvoiceStatus::Voided.as_i32())
        .bind(current_from)
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(0))
    }

    /// Progressive invoices issued for `(subscription, [from, to])`, oldest first.
    pub async fn progressive_invoices_for_period(
        &self,
        subscription_id: Uuid,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Result<Vec<ProgressiveInvoiceRow>> {
        let rows = sqlx::query(
            "SELECT p.id, p.invoice_id, p.usage_amount_cents, p.threshold_ids, p.credited_invoice_id \
             FROM progressive_billing_invoices p JOIN invoices i ON i.id = p.invoice_id \
             WHERE p.subscription_id = $1 AND p.charges_from_datetime = $2 \
               AND p.charges_to_datetime = $3 AND i.status <> $4 \
             ORDER BY p.created_at ASC",
        )
        .bind(subscription_id)
        .bind(from)
        .bind(to)
        .bind(InvoiceStatus::Voided.as_i32())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ProgressiveInvoiceRow {
                    id: r.try_get("id")?,
                    invoice_id: r.try_get("invoice_id")?,
                    usage_amount_cents: r.try_get("usage_amount_cents")?,
                    threshold_ids: r.try_get("threshold_ids")?,
                    credited_invoice_id: r.try_get("credited_invoice_id")?,
                })
            })
            .collect()
    }

    /// Persist a progressive invoice with its fees, numbered from the org
    /// sequence. Serialized per subscription (`FOR UPDATE` on the subscription
    /// row) and re-checked against `expected_billed_cents` — the amount already
    /// progressively billed this period that the caller computed its fees from —
    /// so two concurrent threshold checks can never bill the same usage twice.
    /// Returns `false` (nothing written) when another check won the race.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_progressive_invoice(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
        subscription_id: Uuid,
        from: NaiveDateTime,
        to: NaiveDateTime,
        usage_amount_cents: i64,
        threshold_ids: &[Uuid],
        expected_billed_cents: i64,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM subscriptions WHERE id = $1 FOR UPDATE")
            .bind(subscription_id)
            .fetch_optional(&mut *tx)
            .await?;
        let billed: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(p.usage_amount_cents), 0)::bigint \
             FROM progressive_billing_invoices p JOIN invoices i ON i.id = p.invoice_id \
             WHERE p.subscription_id = $1 AND p.charges_from_datetime = $2 \
               AND p.charges_to_datetime = $3 AND i.status <> $4",
        )
        .bind(subscription_id)
        .bind(from)
        .bind(to)
        .bind(InvoiceStatus::Voided.as_i32())
        .fetch_one(&mut *tx)
        .await?;
        if billed != expected_billed_cents {
            tx.rollback().await?;
            return Ok(false);
        }
        crate::numbering::number_if_needed(&mut *tx, invoice).await?;
        insert_invoice_exec(&mut *tx, invoice).await?;
        for fee in fees {
            insert_fee_exec(&mut *tx, fee).await?;
        }
        sqlx::query(
            "INSERT INTO progressive_billing_invoices (id, organization_id, subscription_id, \
             invoice_id, charges_from_datetime, charges_to_datetime, usage_amount_cents, \
             threshold_ids, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now())",
        )
        .bind(Uuid::new_v4())
        .bind(invoice.organization_id)
        .bind(subscription_id)
        .bind(invoice.id)
        .bind(from)
        .bind(to)
        .bind(usage_amount_cents)
        .bind(threshold_ids)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Record that the period-end invoice credited these progressive invoices.
    pub async fn mark_progressive_invoices_credited(
        &self,
        progressive_ids: &[Uuid],
        final_invoice_id: Uuid,
    ) -> Result<()> {
        if progressive_ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE progressive_billing_invoices SET credited_invoice_id = $2 \
             WHERE id = ANY($1) AND credited_invoice_id IS NULL",
        )
        .bind(progressive_ids)
        .bind(final_invoice_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Running total carried into a weighted-sum period: for a *recurring*
    /// `weighted_sum_agg` metric, the sum of every event value before `before`
    /// (the units still active when the period opens). `0` for anything else.
    pub async fn weighted_sum_carry_in(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        metric: &BillableMetric,
        before: NaiveDateTime,
    ) -> Result<Decimal> {
        if !metric.recurring || !matches!(metric.aggregation_type, AggregationType::WeightedSumAgg) {
            return Ok(Decimal::ZERO);
        }
        let Some(field) = metric.field_name.as_deref().filter(|f| !f.is_empty()) else {
            return Ok(Decimal::ZERO);
        };
        let v: Option<Decimal> = sqlx::query_scalar(
            "SELECT COALESCE(SUM(CASE WHEN btrim(properties->>$4) ~ '^-?[0-9]+(\\.[0-9]+)?$' \
                 THEN btrim(properties->>$4)::numeric ELSE 0 END), 0) \
             FROM events WHERE organization_id = $1 AND external_subscription_id = $2 \
               AND code = $3 AND timestamp < $5 AND deleted_at IS NULL",
        )
        .bind(organization_id)
        .bind(external_subscription_id)
        .bind(&metric.code)
        .bind(field)
        .bind(before)
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(Decimal::ZERO))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(amount: i64, recurring: bool) -> NewUsageThreshold {
        NewUsageThreshold { threshold_display_name: None, amount_cents: amount, recurring }
    }

    #[test]
    fn rejects_bad_threshold_sets() {
        assert!(validate_thresholds(&[t(100, false), t(500, true)]).is_ok());
        assert!(validate_thresholds(&[t(0, false)]).is_err());
        assert!(validate_thresholds(&[t(100, true), t(200, true)]).is_err());
        assert!(validate_thresholds(&[t(100, false), t(100, false)]).is_err());
    }
}
