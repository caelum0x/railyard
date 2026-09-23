//! Repository extensions for the dunning engine (automated failed-payment
//! recovery). Ports the reads/writes behind Lago's
//! `DunningCampaigns::BulkProcessService`, `ProcessAttemptService`, and
//! `PaymentRequests::CreateService`:
//!
//! * campaign resolution (customer override → org default) + thresholds,
//! * per-customer, per-currency overdue balances + attempt read/increment,
//! * payment_request ↔ invoice link inserts.
//!
//! All queries are org-scoped and parameterized.

use std::collections::BTreeMap;

use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A campaign's dunning policy, loaded from `dunning_campaigns`.
#[derive(Debug, Clone)]
pub struct DunningCampaignPolicy {
    pub id: Uuid,
    pub code: String,
    pub max_attempts: i32,
    pub days_between_attempts: i32,
}

/// A per-currency threshold row.
#[derive(Debug, Clone)]
pub struct DunningThresholdRow {
    pub currency: String,
    pub amount_cents: i64,
}

/// A dunning-eligible customer: not excluded, has ≥1 payment-overdue invoice,
/// carrying the customer-level campaign override (if any).
#[derive(Debug, Clone)]
pub struct DunningCandidate {
    pub customer_id: Uuid,
    pub external_id: String,
    pub email: Option<String>,
    pub applied_dunning_campaign_id: Option<Uuid>,
}

impl Db {
    // ---- campaign resolution ------------------------------------------------

    /// Load a campaign's dunning policy by id (org-scoped).
    pub async fn dunning_campaign_policy(
        &self,
        org: Uuid,
        id: Uuid,
    ) -> Result<Option<DunningCampaignPolicy>> {
        let row = sqlx::query(
            "SELECT id, code, max_attempts, days_between_attempts \
             FROM dunning_campaigns WHERE id = $1 AND organization_id = $2",
        )
        .bind(id)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| DunningCampaignPolicy {
            id: r.get("id"),
            code: r.get("code"),
            max_attempts: r.get("max_attempts"),
            days_between_attempts: r.get("days_between_attempts"),
        }))
    }

    /// The organization's default dunning campaign (`applied_to_organization`),
    /// used when a customer has no `applied_dunning_campaign_id`. Ports the
    /// billing-entity default campaign in Lago's `applicable_dunning_campaign`.
    pub async fn org_default_dunning_campaign(
        &self,
        org: Uuid,
    ) -> Result<Option<DunningCampaignPolicy>> {
        let row = sqlx::query(
            "SELECT id, code, max_attempts, days_between_attempts \
             FROM dunning_campaigns \
             WHERE organization_id = $1 AND applied_to_organization = true \
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| DunningCampaignPolicy {
            id: r.get("id"),
            code: r.get("code"),
            max_attempts: r.get("max_attempts"),
            days_between_attempts: r.get("days_between_attempts"),
        }))
    }

    /// Every threshold configured for a campaign.
    pub async fn dunning_campaign_thresholds(
        &self,
        org: Uuid,
        campaign_id: Uuid,
    ) -> Result<Vec<DunningThresholdRow>> {
        let rows = sqlx::query(
            "SELECT currency, amount_cents FROM dunning_campaign_thresholds \
             WHERE dunning_campaign_id = $1 AND organization_id = $2 \
             ORDER BY amount_cents ASC",
        )
        .bind(campaign_id)
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| DunningThresholdRow {
                currency: r.get("currency"),
                amount_cents: r.get("amount_cents"),
            })
            .collect())
    }

    /// Insert (or upsert) a per-currency threshold for a campaign.
    pub async fn upsert_dunning_campaign_threshold(
        &self,
        id: Uuid,
        org: Uuid,
        campaign_id: Uuid,
        currency: &str,
        amount_cents: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO dunning_campaign_thresholds \
                 (id, dunning_campaign_id, organization_id, currency, amount_cents) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (dunning_campaign_id, currency) \
             DO UPDATE SET amount_cents = EXCLUDED.amount_cents, updated_at = now()",
        )
        .bind(id)
        .bind(campaign_id)
        .bind(org)
        .bind(currency)
        .bind(amount_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- eligible customers -------------------------------------------------

    /// Organizations that have at least one finalized, payment-overdue invoice —
    /// the orgs a dunning sweep needs to visit.
    pub async fn dunning_organizations(&self) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT DISTINCT organization_id FROM invoices \
             WHERE payment_overdue = true AND status = 1",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| r.get::<Uuid, _>("organization_id"))
            .collect())
    }

    /// Customers eligible for a dunning sweep: `exclude_from_dunning_campaign`
    /// is false and the customer has at least one finalized, payment-overdue
    /// invoice. Ports `BulkProcessService#eligible_customers`.
    pub async fn dunning_candidates(&self, org: Uuid) -> Result<Vec<DunningCandidate>> {
        let rows = sqlx::query(
            "SELECT c.id, c.external_id, c.email, c.applied_dunning_campaign_id \
             FROM customers c \
             WHERE c.organization_id = $1 \
               AND c.exclude_from_dunning_campaign = false \
               AND c.deleted_at IS NULL \
               AND EXISTS ( \
                   SELECT 1 FROM invoices i \
                   WHERE i.customer_id = c.id \
                     AND i.payment_overdue = true \
                     AND i.status = 1 \
               ) \
             ORDER BY c.created_at ASC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| DunningCandidate {
                customer_id: r.get("id"),
                external_id: r.get("external_id"),
                email: r.get("email"),
                applied_dunning_campaign_id: r.get("applied_dunning_campaign_id"),
            })
            .collect())
    }

    /// A customer's overdue balance per currency — raw `SUM(total_amount_cents)`
    /// over finalized, payment-overdue invoices, grouped by currency. Ports
    /// `Customer#overdue_balances` (NOT net of credits — the net is applied only
    /// to the payment-request amount).
    pub async fn customer_overdue_balances(
        &self,
        org: Uuid,
        customer_id: Uuid,
    ) -> Result<BTreeMap<String, i64>> {
        let rows = sqlx::query(
            "SELECT COALESCE(currency, '') AS currency, SUM(total_amount_cents)::bigint AS total \
             FROM invoices \
             WHERE organization_id = $1 AND customer_id = $2 \
               AND payment_overdue = true AND status = 1 \
             GROUP BY currency",
        )
        .bind(org)
        .bind(customer_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>("currency"), r.get::<i64, _>("total")))
            .filter(|(c, _)| !c.is_empty())
            .collect())
    }

    // ---- attempt tracking ---------------------------------------------------

    /// Read a customer's `(dunning_currency_attempts, last_attempt_at)`.
    pub async fn customer_dunning_attempts(
        &self,
        customer_id: Uuid,
    ) -> Result<(BTreeMap<String, i64>, Option<NaiveDateTime>)> {
        let row = sqlx::query(
            "SELECT dunning_currency_attempts, last_dunning_campaign_attempt_at \
             FROM customers WHERE id = $1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok((BTreeMap::new(), None));
        };
        let raw: Value = row.get("dunning_currency_attempts");
        let last: Option<NaiveDateTime> = row.get("last_dunning_campaign_attempt_at");
        let attempts = raw
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), n)))
                    .collect()
            })
            .unwrap_or_default();
        Ok((attempts, last))
    }

    /// Persist a customer's updated attempt map + last-attempt instant. Written
    /// atomically as a single row update (Lago `customer.save!`).
    pub async fn update_customer_dunning_attempts(
        &self,
        customer_id: Uuid,
        attempts: &BTreeMap<String, i64>,
        last_attempt: NaiveDateTime,
    ) -> Result<()> {
        let json = Value::Object(
            attempts
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect(),
        );
        sqlx::query(
            "UPDATE customers SET dunning_currency_attempts = $2, \
             last_dunning_campaign_attempt_at = $3, updated_at = now() WHERE id = $1",
        )
        .bind(customer_id)
        .bind(json)
        .bind(last_attempt)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- payment request ↔ invoice links -----------------------------------

    /// Overdue invoices for a customer in one currency, with each invoice's
    /// finalized credit-note offset. Ports the invoice set + credit-note offset
    /// used by `PaymentRequests::CreateService`.
    ///
    /// `total_paid_amount_cents` is derived as `SUM(payments.amount_cents)` over
    /// `succeeded` payments recorded against the invoice, so a partially-paid
    /// invoice is dunned/charged only for the amount still owed (total − paid −
    /// credit notes). The credit-note offset uses finalized, non-voided credit
    /// notes' `credit_amount_cents` (the credited portion that reduces the amount
    /// owed, excluding cash refunds).
    pub async fn overdue_invoices_for_currency(
        &self,
        org: Uuid,
        customer_id: Uuid,
        currency: &str,
    ) -> Result<Vec<OverdueInvoiceRow>> {
        let rows = sqlx::query(
            "SELECT i.id, i.total_amount_cents, \
                 COALESCE(( \
                     SELECT SUM(cn.credit_amount_cents)::bigint FROM credit_notes cn \
                     WHERE cn.invoice_id = i.id AND cn.status = 1 AND cn.voided_at IS NULL \
                 ), 0) AS offset_cents, \
                 COALESCE(( \
                     SELECT SUM(p.amount_cents)::bigint FROM payments p \
                     WHERE p.invoice_id = i.id AND p.status = 'succeeded' \
                 ), 0) AS paid_cents \
             FROM invoices i \
             WHERE i.organization_id = $1 AND i.customer_id = $2 \
               AND i.currency = $3 AND i.payment_overdue = true AND i.status = 1 \
             ORDER BY i.created_at ASC",
        )
        .bind(org)
        .bind(customer_id)
        .bind(currency)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| OverdueInvoiceRow {
                id: r.get("id"),
                total_amount_cents: r.get("total_amount_cents"),
                total_paid_amount_cents: r.get::<i64, _>("paid_cents"),
                credit_note_offset_cents: r.get::<i64, _>("offset_cents"),
            })
            .collect())
    }

    /// Insert a payment_request → invoice link (idempotent on the pair).
    pub async fn insert_payment_request_applied_invoice(
        &self,
        id: Uuid,
        payment_request_id: Uuid,
        invoice_id: Uuid,
        org: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO payment_request_applied_invoices \
                 (id, payment_request_id, invoice_id, organization_id) \
             VALUES ($1,$2,$3,$4) \
             ON CONFLICT (payment_request_id, invoice_id) DO NOTHING",
        )
        .bind(id)
        .bind(payment_request_id)
        .bind(invoice_id)
        .bind(org)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert a payment_request carrying its originating dunning campaign.
    /// Mirrors [`Db::insert_payment_request`] but records `dunning_campaign_id`.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_dunning_payment_request(
        &self,
        id: Uuid,
        org: Uuid,
        customer_id: Uuid,
        external_customer_id: &str,
        email: Option<&str>,
        amount_cents: i64,
        currency: &str,
        dunning_campaign_id: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO payment_requests \
                 (id, organization_id, customer_id, external_customer_id, email, \
                  amount_cents, amount_currency, dunning_campaign_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(customer_id)
        .bind(external_customer_id)
        .bind(email)
        .bind(amount_cents)
        .bind(currency)
        .bind(dunning_campaign_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set a payment request's aggregate `payment_status` (`pending` |
    /// `succeeded` | `failed`); returns whether it changed.
    pub async fn set_payment_request_status(&self, id: Uuid, status: &str) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE payment_requests SET payment_status = $2, updated_at = now() \
             WHERE id = $1 AND payment_status <> $2",
        )
        .bind(id)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Invoice ids linked to a payment request (for the sweep's charge step and
    /// for tests).
    pub async fn payment_request_invoice_ids(&self, payment_request_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT invoice_id FROM payment_request_applied_invoices \
             WHERE payment_request_id = $1 ORDER BY created_at ASC",
        )
        .bind(payment_request_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| r.get::<Uuid, _>("invoice_id"))
            .collect())
    }

    /// The dunning read surface: every finalized, payment-overdue invoice for an
    /// org joined to its customer's dunning state (attempt count, last attempt,
    /// exclusion, applied campaign). Newest-first, paginated. Ports what the
    /// dunning dashboard shows — overdue invoices + per-customer campaign/attempt
    /// status — over the same tables the dunning sweep reads.
    pub async fn overdue_dunning_overview(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DunningOverviewRow>> {
        let rows = sqlx::query(
            "SELECT i.id AS invoice_id, i.number, i.currency, i.total_amount_cents, \
                    i.payment_due_date, i.customer_id, \
                    c.external_id, c.email, c.exclude_from_dunning_campaign, \
                    c.applied_dunning_campaign_id, c.last_dunning_campaign_attempt_at, \
                    c.dunning_currency_attempts \
             FROM invoices i \
             JOIN customers c ON c.id = i.customer_id \
             WHERE i.organization_id = $1 AND i.payment_overdue = true AND i.status = 1 \
             ORDER BY i.payment_due_date ASC NULLS LAST, i.created_at ASC \
             LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| DunningOverviewRow {
                invoice_id: r.get("invoice_id"),
                invoice_number: r.get("number"),
                currency: r.try_get("currency").unwrap_or_default(),
                total_amount_cents: r.get("total_amount_cents"),
                payment_due_date: r.try_get("payment_due_date").ok().flatten(),
                customer_id: r.get("customer_id"),
                external_customer_id: r.get("external_id"),
                customer_email: r.try_get("email").ok().flatten(),
                exclude_from_dunning_campaign: r.get("exclude_from_dunning_campaign"),
                applied_dunning_campaign_id: r
                    .try_get("applied_dunning_campaign_id")
                    .ok()
                    .flatten(),
                last_dunning_campaign_attempt_at: r
                    .try_get("last_dunning_campaign_attempt_at")
                    .ok()
                    .flatten(),
                dunning_currency_attempts: r
                    .try_get("dunning_currency_attempts")
                    .unwrap_or_else(|_| serde_json::json!({})),
            })
            .collect())
    }

}

/// One row of the dunning overview: an overdue invoice + its customer's dunning
/// campaign/attempt state.
#[derive(Debug, Clone)]
pub struct DunningOverviewRow {
    pub invoice_id: Uuid,
    pub invoice_number: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub payment_due_date: Option<chrono::NaiveDate>,
    pub customer_id: Uuid,
    pub external_customer_id: String,
    pub customer_email: Option<String>,
    pub exclude_from_dunning_campaign: bool,
    pub applied_dunning_campaign_id: Option<Uuid>,
    pub last_dunning_campaign_attempt_at: Option<NaiveDateTime>,
    pub dunning_currency_attempts: Value,
}

/// An overdue invoice row with its already-paid amount and credit-note offset.
#[derive(Debug, Clone)]
pub struct OverdueInvoiceRow {
    pub id: Uuid,
    pub total_amount_cents: i64,
    /// Amount already settled against the invoice (`SUM` of succeeded payments).
    pub total_paid_amount_cents: i64,
    pub credit_note_offset_cents: i64,
}
