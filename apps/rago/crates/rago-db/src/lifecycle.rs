//! Lifecycle sweeps behind the clock jobs: draft grace periods and
//! finalization, draft refresh, incomplete subscriptions, quote / order-form
//! expiry and scheduled order execution, and EU VIES results.

use chrono::{NaiveDate, NaiveDateTime};
use rago_core::models::Invoice;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

/// Longest accepted draft grace period, in days (Lago caps it at 365).
pub const MAX_INVOICE_GRACE_PERIOD_DAYS: i32 = 365;

/// SQL for the grace period (days) in force for invoice alias `i`, given the
/// joined organization `o` and (left-joined) customer `c`: the customer
/// override wins, else the organization default.
const GRACE_EXPR: &str = "COALESCE(c.invoice_grace_period, o.invoice_grace_period, 0)";

/// A subscription moved by a lifecycle sweep.
#[derive(Debug, Clone)]
pub struct SubscriptionRef {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub external_id: String,
}

fn subscription_ref(r: &sqlx::postgres::PgRow) -> Result<SubscriptionRef> {
    Ok(SubscriptionRef {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        customer_id: r.try_get("customer_id")?,
        external_id: r.try_get("external_id")?,
    })
}

/// An order form moved to `expired`.
#[derive(Debug, Clone)]
pub struct ExpiredOrderForm {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Option<Uuid>,
    pub quote_id: Option<Uuid>,
    pub number: String,
    pub expires_at: Option<NaiveDateTime>,
}

/// The outcome of one VIES lookup, as stored on the customer.
#[derive(Debug, Clone)]
pub struct ViesResult {
    pub checked_number: String,
    pub valid: Option<bool>,
    pub name: Option<String>,
    pub address: Option<String>,
    pub error: Option<String>,
}

impl Db {
    // ---- draft grace period ------------------------------------------------

    /// The organization-wide draft grace period (days).
    pub async fn organization_invoice_grace_period(&self, org: Uuid) -> Result<i32> {
        let v: Option<i32> =
            sqlx::query_scalar("SELECT invoice_grace_period FROM organizations WHERE id = $1")
                .bind(org)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.unwrap_or(0))
    }

    /// Set the organization-wide draft grace period (days, clamped to
    /// `0..=365`). Only invoices billed from now on are affected, plus drafts
    /// still waiting on their grace window.
    pub async fn set_organization_invoice_grace_period(&self, org: Uuid, days: i32) -> Result<()> {
        sqlx::query(
            "UPDATE organizations SET invoice_grace_period = $2, updated_at = now() WHERE id = $1",
        )
        .bind(org)
        .bind(days.clamp(0, MAX_INVOICE_GRACE_PERIOD_DAYS))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The grace period in force for an invoice of `customer_id` in `org`: the
    /// customer's override when set, else the organization default.
    pub async fn invoice_grace_period_for(&self, org: Uuid, customer_id: Option<Uuid>) -> Result<i32> {
        let v: Option<i32> = sqlx::query_scalar(&format!(
            "SELECT {GRACE_EXPR} FROM organizations o \
             LEFT JOIN customers c ON c.id = $2 AND c.organization_id = o.id \
             WHERE o.id = $1"
        ))
        .bind(org)
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v.unwrap_or(0).max(0))
    }

    /// Drafts whose grace window has ended on `today` (issuing date + grace
    /// period ≤ today), oldest first, bounded to `limit`.
    pub async fn drafts_past_grace(&self, today: NaiveDate, limit: i64) -> Result<Vec<Uuid>> {
        let ids = sqlx::query_scalar(&format!(
            "SELECT i.id FROM invoices i \
             JOIN organizations o ON o.id = i.organization_id \
             LEFT JOIN customers c ON c.id = i.customer_id \
             WHERE i.status = 0 \
               AND COALESCE(i.issuing_date, i.created_at::date) + {GRACE_EXPR} <= $1 \
             ORDER BY i.created_at LIMIT $2"
        ))
        .bind(today)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    /// Drafts in a real grace window (grace > 0) that end by tomorrow and have
    /// not been announced yet: stamps them and returns them, so
    /// `invoice.ready_to_finalize` fires exactly once per draft.
    pub async fn claim_drafts_ready_to_finalize(&self, today: NaiveDate) -> Result<Vec<Invoice>> {
        let rows = sqlx::query(&format!(
            "WITH due AS ( \
                 SELECT i.id FROM invoices i \
                 JOIN organizations o ON o.id = i.organization_id \
                 LEFT JOIN customers c ON c.id = i.customer_id \
                 WHERE i.status = 0 AND i.ready_to_finalize_notified_at IS NULL \
                   AND {GRACE_EXPR} > 0 \
                   AND COALESCE(i.issuing_date, i.created_at::date) + {GRACE_EXPR} <= $1::date + 1 \
             ) \
             UPDATE invoices i SET ready_to_finalize_notified_at = now() \
             FROM due WHERE i.id = due.id AND i.ready_to_finalize_notified_at IS NULL \
             RETURNING i.*"
        ))
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::invoice).collect()
    }

    /// `Invoices::RefreshDraftService` (totals) for every draft whose persisted
    /// fees no longer match its rollups. Unchanged drafts are not touched, so
    /// the caller emits `invoice.drafted` only for real changes.
    pub async fn refresh_changed_drafts(&self) -> Result<Vec<Invoice>> {
        let rows = sqlx::query(
            "WITH agg AS ( \
                 SELECT f.invoice_id, \
                        COALESCE(SUM(f.amount_cents), 0)::bigint AS fees, \
                        COALESCE(SUM(f.taxes_amount_cents), 0)::bigint AS taxes \
                 FROM fees f JOIN invoices d ON d.id = f.invoice_id AND d.status = 0 \
                 GROUP BY f.invoice_id \
             ) \
             UPDATE invoices i SET \
                 fees_amount_cents = agg.fees, \
                 taxes_amount_cents = agg.taxes, \
                 sub_total_excluding_taxes_amount_cents = agg.fees, \
                 sub_total_including_taxes_amount_cents = agg.fees + agg.taxes, \
                 total_amount_cents = GREATEST(agg.fees + agg.taxes - i.coupons_amount_cents \
                     - i.credit_notes_amount_cents - i.prepaid_credit_amount_cents, 0), \
                 updated_at = now() \
             FROM agg \
             WHERE i.id = agg.invoice_id AND i.status = 0 \
               AND (i.fees_amount_cents IS DISTINCT FROM agg.fees \
                    OR i.taxes_amount_cents IS DISTINCT FROM agg.taxes) \
             RETURNING i.*",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::invoice).collect()
    }

    // ---- incomplete subscriptions -------------------------------------------

    /// Whether activating `subscription_id` needs a payment method up front:
    /// a pay-in-advance plan with a positive fee and no trial is charged the
    /// moment it starts (Lago's `incomplete` path).
    pub async fn subscription_requires_payment_method(&self, subscription_id: Uuid) -> Result<bool> {
        let v: Option<bool> = sqlx::query_scalar(
            "SELECT (p.pay_in_advance AND COALESCE(p.amount_cents, 0) > 0 \
                     AND COALESCE(p.trial_period, 0) <= 0) \
             FROM subscriptions s JOIN plans p ON p.id = s.plan_id WHERE s.id = $1",
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v.unwrap_or(false))
    }

    /// Pending subscriptions whose start date has arrived.
    pub async fn pending_subscriptions_due(&self, now: NaiveDateTime) -> Result<Vec<SubscriptionRef>> {
        let rows = sqlx::query(
            "SELECT id, organization_id, customer_id, external_id FROM subscriptions \
             WHERE status = 0 AND subscription_at IS NOT NULL AND subscription_at <= $1 \
             ORDER BY subscription_at",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(subscription_ref).collect()
    }

    /// Pending → incomplete (activation blocked on a missing payment method).
    /// Returns whether this call moved it.
    pub async fn mark_subscription_incomplete(&self, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 4, updated_at = now() WHERE id = $1 AND status = 0",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Activate one pending subscription. Returns whether this call moved it.
    pub async fn activate_pending_subscription(&self, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 1, started_at = COALESCE(started_at, now()), \
                    updated_at = now() WHERE id = $1 AND status = 0",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Every incomplete subscription (re-checked each tick for a payment method).
    pub async fn incomplete_subscriptions(&self) -> Result<Vec<SubscriptionRef>> {
        let rows = sqlx::query(
            "SELECT id, organization_id, customer_id, external_id FROM subscriptions \
             WHERE status = 4 ORDER BY updated_at LIMIT 500",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(subscription_ref).collect()
    }

    /// Incomplete → active once the customer has a payment method.
    pub async fn activate_incomplete_subscription(&self, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 1, started_at = COALESCE(started_at, now()), \
                    updated_at = now() WHERE id = $1 AND status = 4",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Incomplete subscriptions whose activation window (since their start
    /// date) elapsed before `cutoff` → canceled. Returns those canceled.
    pub async fn cancel_expired_incomplete_subscriptions(
        &self,
        cutoff: NaiveDateTime,
    ) -> Result<Vec<SubscriptionRef>> {
        let rows = sqlx::query(
            "UPDATE subscriptions SET status = 3, canceled_at = now(), updated_at = now() \
             WHERE status = 4 AND COALESCE(subscription_at, created_at) <= $1 \
             RETURNING id, organization_id, customer_id, external_id",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(subscription_ref).collect()
    }

    // ---- quotes / order forms / orders --------------------------------------

    /// Approved quote versions past their expiry → `expired`, unless the quote
    /// already has a signed order form (a signed commitment outlives the offer).
    /// Returns `(version_id, organization_id, quote_id)` of those expired.
    pub async fn expire_quote_versions(&self, now: NaiveDateTime) -> Result<Vec<(Uuid, Uuid, Uuid)>> {
        let rows = sqlx::query(
            "UPDATE quote_versions v SET status = 'expired', updated_at = now() \
             WHERE v.status = 'approved' AND v.expires_at IS NOT NULL AND v.expires_at <= $1 \
               AND NOT EXISTS (SELECT 1 FROM order_forms f \
                               WHERE f.quote_id = v.quote_id AND f.status = 'signed') \
             RETURNING v.id, v.organization_id, v.quote_id",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("organization_id"), r.get("quote_id")))
            .collect())
    }

    /// Unsigned order forms past their expiry → `expired`. Returns those moved.
    pub async fn expire_order_forms(&self, now: NaiveDateTime) -> Result<Vec<ExpiredOrderForm>> {
        let rows = sqlx::query(
            "UPDATE order_forms SET status = 'expired', expired_at = now(), updated_at = now() \
             WHERE status = 'draft' AND expires_at IS NOT NULL AND expires_at <= $1 \
             RETURNING id, organization_id, customer_id, quote_id, number, expires_at",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ExpiredOrderForm {
                    id: r.try_get("id")?,
                    organization_id: r.try_get("organization_id")?,
                    customer_id: r.try_get("customer_id")?,
                    quote_id: r.try_get("quote_id")?,
                    number: r.try_get("number")?,
                    expires_at: r.try_get("expires_at")?,
                })
            })
            .collect()
    }

    /// Set (or clear) an order form's expiry.
    pub async fn set_order_form_expires_at(&self, id: Uuid, expires_at: Option<NaiveDateTime>) -> Result<()> {
        sqlx::query("UPDATE order_forms SET expires_at = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// An order form's `(status, expires_at)`.
    pub async fn order_form_state(&self, id: Uuid) -> Result<Option<(String, Option<NaiveDateTime>)>> {
        let row = sqlx::query("SELECT status, expires_at FROM order_forms WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("status"), r.get("expires_at"))))
    }

    /// Whether the quote has a signed order form.
    pub async fn quote_has_signed_order_form(&self, quote_id: Uuid) -> Result<bool> {
        let v: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM order_forms WHERE quote_id = $1 AND status = 'signed' LIMIT 1",
        )
        .bind(quote_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v.is_some())
    }

    /// Pending orders whose signed form scheduled execution at or before
    /// `now` (`execute_at`), except forms signed as `order_only` (executed
    /// outside billing). Returns `(order_id, organization_id)`.
    pub async fn orders_due_for_execution(&self, now: NaiveDateTime, limit: i64) -> Result<Vec<(Uuid, Uuid)>> {
        let rows = sqlx::query(
            "SELECT o.id, o.organization_id FROM orders o \
             JOIN order_forms f ON f.id = o.order_form_id \
             WHERE o.status = 'pending' AND f.status = 'signed' \
               AND f.execute_at IS NOT NULL AND f.execute_at <= $1 \
               AND COALESCE(f.execution_mode, '') <> 'order_only' \
             ORDER BY f.execute_at LIMIT $2",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("organization_id"))).collect())
    }

    // ---- VIES ----------------------------------------------------------------

    /// `(country, tax_identification_number, vies_checked_number)` of a customer.
    pub async fn customer_vat_identity(
        &self,
        customer_id: Uuid,
    ) -> Result<Option<(Option<String>, Option<String>, Option<String>)>> {
        let row = sqlx::query(
            "SELECT country, tax_identification_number, vies_checked_number \
             FROM customers WHERE id = $1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get("country"),
                r.get("tax_identification_number"),
                r.get("vies_checked_number"),
            )
        }))
    }

    /// Persist a VIES lookup on the customer.
    pub async fn record_vies_result(&self, customer_id: Uuid, result: &ViesResult) -> Result<()> {
        sqlx::query(
            "UPDATE customers SET vies_valid = $2, vies_checked_at = now(), vies_checked_number = $3, \
                    vies_name = $4, vies_address = $5, vies_error = $6, updated_at = now() \
             WHERE id = $1",
        )
        .bind(customer_id)
        .bind(result.valid)
        .bind(&result.checked_number)
        .bind(result.name.as_deref())
        .bind(result.address.as_deref())
        .bind(result.error.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
