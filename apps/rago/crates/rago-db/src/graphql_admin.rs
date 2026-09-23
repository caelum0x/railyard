//! Repository extensions for the GraphQL dashboard admin CRUD surface
//! (`rago-graphql`). These are the update/destroy/finalize operations the
//! dashboard needs that the REST-focused [`crate::repo`] did not yet expose.
//! Kept in a separate file (rather than growing `repo.rs`) so the admin surface
//! is easy to review in isolation; every function is a thin, parameterized
//! `sqlx` query mirroring the Lago service it stands in for.
//!
//! Faithfulness notes:
//! * Soft-deletable resources (customers, plans, charges, charge filters,
//!   billable metrics, add-ons, coupons) set `deleted_at` — matching Lago's
//!   `paranoia`/discard semantics — rather than hard-deleting.
//! * Taxes, dunning campaigns and webhook endpoints have no `deleted_at` column
//!   (as in Lago's schema), so their destroy is a hard `DELETE`.

use chrono::{DateTime, NaiveDateTime, Utc};
use rago_core::enums::*;
use rago_core::models::*;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, Result};
use crate::mapping;
use crate::repo::Db;

fn utc(ndt: NaiveDateTime) -> DateTime<Utc> {
    ndt.and_utc()
}
fn opt_utc(ndt: Option<NaiveDateTime>) -> Option<DateTime<Utc>> {
    ndt.map(|t| t.and_utc())
}

/// An outbound webhook delivery record, as surfaced by `Types::Webhooks::Object`.
#[derive(Debug, Clone)]
pub struct WebhookRow {
    pub id: Uuid,
    pub webhook_type: String,
    pub object_type: String,
    pub status: String,
    pub http_status: Option<i32>,
    pub endpoint: String,
    pub retries: i32,
}

fn webhook_row(r: &sqlx::postgres::PgRow) -> WebhookRow {
    WebhookRow {
        id: r.get("id"),
        webhook_type: r.get("webhook_type"),
        object_type: r.get("object_type"),
        status: r.get("status"),
        http_status: r.get("http_status"),
        endpoint: r.get("endpoint"),
        retries: r.get("retries"),
    }
}

/// A dunning-campaign row `(id, name, code, description, max_attempts,
/// days_between_attempts, applied_to_organization)`.
pub type DunningCampaignRow = (Uuid, String, String, Option<String>, i32, i32, bool);

/// An add-on row `(id, name, code, description, amount_cents, amount_currency)`.
pub type AddOnRow = (Uuid, String, String, Option<String>, i64, String);

/// A charge filter (Lago `app/models/charge_filter.rb`).
fn charge_filter(r: &sqlx::postgres::PgRow) -> Result<ChargeFilter> {
    Ok(ChargeFilter {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        charge_id: r.try_get("charge_id")?,
        properties: r.try_get("properties")?,
        invoice_display_name: r.try_get("invoice_display_name")?,
        created_at: utc(r.try_get("created_at")?),
        updated_at: utc(r.try_get("updated_at")?),
        deleted_at: opt_utc(r.try_get("deleted_at")?),
    })
}

fn applied_coupon(r: &sqlx::postgres::PgRow) -> Result<AppliedCoupon> {
    Ok(AppliedCoupon {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        coupon_id: r.try_get("coupon_id")?,
        customer_id: r.try_get("customer_id")?,
        status: CouponStatus::try_from(r.try_get::<i32, _>("status")?).map_err(DbError::enum_err)?,
        amount_cents: r.try_get("amount_cents")?,
        amount_currency: r.try_get("amount_currency")?,
        percentage_rate: r.try_get("percentage_rate")?,
        frequency: CouponFrequency::try_from(r.try_get::<i32, _>("frequency")?)
            .map_err(DbError::enum_err)?,
        frequency_duration: r.try_get("frequency_duration")?,
        frequency_duration_remaining: r.try_get("frequency_duration_remaining")?,
        created_at: utc(r.try_get("created_at")?),
        updated_at: utc(r.try_get("updated_at")?),
        terminated_at: opt_utc(r.try_get("terminated_at")?),
    })
}

impl Db {
    // ---- customers -------------------------------------------------------

    /// `Customers::DestroyService` — soft-delete a customer.
    pub async fn soft_delete_customer(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE customers SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `Customers::UpdateService` (invoice_grace_period only) — persist the
    /// per-customer grace period and return the refreshed customer.
    pub async fn update_customer_invoice_grace_period(
        &self,
        id: Uuid,
        invoice_grace_period: Option<i32>,
    ) -> Result<Option<Customer>> {
        sqlx::query(
            "UPDATE customers SET invoice_grace_period = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(invoice_grace_period)
        .execute(&self.pool)
        .await?;
        self.find_customer(id).await
    }

    // ---- plans -----------------------------------------------------------

    /// `Plans::UpdateService` — update a plan's mutable fields (only provided
    /// values change; `NULL` args are ignored via `COALESCE`).
    #[allow(clippy::too_many_arguments)]
    pub async fn update_plan(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        amount_cents: Option<i64>,
        amount_currency: Option<&str>,
        pay_in_advance: Option<bool>,
        interval: Option<i32>,
        invoice_display_name: Option<&str>,
    ) -> Result<Option<Plan>> {
        sqlx::query(
            "UPDATE plans SET \
             name = COALESCE($2, name), \
             description = COALESCE($3, description), \
             amount_cents = COALESCE($4, amount_cents), \
             amount_currency = COALESCE($5, amount_currency), \
             pay_in_advance = COALESCE($6, pay_in_advance), \
             interval = COALESCE($7, interval), \
             invoice_display_name = COALESCE($8, invoice_display_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(amount_cents)
        .bind(amount_currency)
        .bind(pay_in_advance)
        .bind(interval)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        self.find_plan_opt(id).await
    }

    /// `Plans::PrepareDestroyService` — soft-delete a plan and its charges.
    pub async fn soft_delete_plan(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE charges SET deleted_at = now(), updated_at = now() WHERE plan_id = $1 AND deleted_at IS NULL")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE plans SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// A plan by id, or `None` (the fallible-but-optional counterpart to
    /// [`Db::find_plan`]).
    pub async fn find_plan_opt(&self, id: Uuid) -> Result<Option<Plan>> {
        let row = sqlx::query("SELECT * FROM plans WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::plan).transpose()
    }

    // ---- charges ---------------------------------------------------------

    /// `Charges::UpdateService` — update a charge's pricing config.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_charge(
        &self,
        id: Uuid,
        charge_model: Option<i32>,
        properties: Option<&Value>,
        pay_in_advance: Option<bool>,
        prorated: Option<bool>,
        invoiceable: Option<bool>,
        min_amount_cents: Option<i64>,
        invoice_display_name: Option<&str>,
    ) -> Result<Option<Charge>> {
        sqlx::query(
            "UPDATE charges SET \
             charge_model = COALESCE($2, charge_model), \
             properties = COALESCE($3, properties), \
             pay_in_advance = COALESCE($4, pay_in_advance), \
             prorated = COALESCE($5, prorated), \
             invoiceable = COALESCE($6, invoiceable), \
             min_amount_cents = COALESCE($7, min_amount_cents), \
             invoice_display_name = COALESCE($8, invoice_display_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(charge_model)
        .bind(properties)
        .bind(pay_in_advance)
        .bind(prorated)
        .bind(invoiceable)
        .bind(min_amount_cents)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        self.find_charge_opt(id).await
    }

    /// A charge by id, or `None`.
    pub async fn find_charge_opt(&self, id: Uuid) -> Result<Option<Charge>> {
        let row = sqlx::query("SELECT * FROM charges WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::charge).transpose()
    }

    /// `Charges::DestroyService` — soft-delete a charge (and its filters).
    pub async fn soft_delete_charge(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE charge_filters SET deleted_at = now(), updated_at = now() WHERE charge_id = $1 AND deleted_at IS NULL")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE charges SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- charge filters --------------------------------------------------

    /// `ChargeFilters::CreateService` — attach a named filter to a charge.
    pub async fn insert_charge_filter(
        &self,
        id: Uuid,
        org: Uuid,
        charge_id: Uuid,
        properties: &Value,
        invoice_display_name: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO charge_filters (id, organization_id, charge_id, properties, invoice_display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(charge_id)
        .bind(properties)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `ChargeFilters::UpdateService`.
    pub async fn update_charge_filter(
        &self,
        id: Uuid,
        properties: Option<&Value>,
        invoice_display_name: Option<&str>,
    ) -> Result<Option<ChargeFilter>> {
        sqlx::query(
            "UPDATE charge_filters SET \
             properties = COALESCE($2, properties), \
             invoice_display_name = COALESCE($3, invoice_display_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(properties)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        self.find_charge_filter(id).await
    }

    /// `ChargeFilters::DestroyService` — soft-delete a charge filter.
    pub async fn soft_delete_charge_filter(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE charge_filters SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_charge_filter(&self, id: Uuid) -> Result<Option<ChargeFilter>> {
        let row = sqlx::query("SELECT * FROM charge_filters WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(charge_filter).transpose()
    }

    pub async fn charge_filters_for_charge(&self, charge_id: Uuid) -> Result<Vec<ChargeFilter>> {
        let rows = sqlx::query("SELECT * FROM charge_filters WHERE charge_id = $1 AND deleted_at IS NULL ORDER BY created_at ASC")
            .bind(charge_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(charge_filter).collect()
    }

    // ---- billable metrics ------------------------------------------------

    /// `BillableMetrics::UpdateService`.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_billable_metric(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        aggregation_type: Option<i32>,
        field_name: Option<&str>,
        recurring: Option<bool>,
        expression: Option<&str>,
        custom_aggregator: Option<&str>,
    ) -> Result<Option<BillableMetric>> {
        sqlx::query(
            "UPDATE billable_metrics SET \
             name = COALESCE($2, name), \
             description = COALESCE($3, description), \
             aggregation_type = COALESCE($4, aggregation_type), \
             field_name = COALESCE($5, field_name), \
             recurring = COALESCE($6, recurring), \
             expression = COALESCE($7, expression), \
             custom_aggregator = COALESCE($8, custom_aggregator), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(aggregation_type)
        .bind(field_name)
        .bind(recurring)
        .bind(expression)
        .bind(custom_aggregator)
        .execute(&self.pool)
        .await?;
        self.find_billable_metric_opt(id).await
    }

    pub async fn find_billable_metric_opt(&self, id: Uuid) -> Result<Option<BillableMetric>> {
        let row = sqlx::query("SELECT * FROM billable_metrics WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::billable_metric).transpose()
    }

    /// `BillableMetrics::DestroyService` — soft-delete.
    pub async fn soft_delete_billable_metric(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE billable_metrics SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- add-ons ---------------------------------------------------------

    /// `AddOns::UpdateService`.
    pub async fn update_add_on(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        amount_cents: Option<i64>,
    ) -> Result<Option<AddOnRow>> {
        sqlx::query(
            "UPDATE add_ons SET \
             name = COALESCE($2, name), \
             description = COALESCE($3, description), \
             amount_cents = COALESCE($4, amount_cents), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(amount_cents)
        .execute(&self.pool)
        .await?;
        self.find_add_on(id).await
    }

    pub async fn find_add_on(&self, id: Uuid) -> Result<Option<AddOnRow>> {
        let row = sqlx::query("SELECT id, name, code, description, amount_cents, amount_currency FROM add_ons WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("name"),
                r.get::<String, _>("code"),
                r.get::<Option<String>, _>("description"),
                r.get::<i64, _>("amount_cents"),
                r.get::<String, _>("amount_currency"),
            )
        }))
    }

    /// `AddOns::DestroyService` — soft-delete.
    pub async fn soft_delete_add_on(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE add_ons SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- coupons ---------------------------------------------------------

    /// `Coupons::UpdateService`.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_coupon(
        &self,
        id: Uuid,
        name: Option<&str>,
        code: Option<&str>,
        amount_cents: Option<i64>,
        percentage_rate: Option<rust_decimal::Decimal>,
        frequency: Option<i32>,
        frequency_duration: Option<i32>,
        reusable: Option<bool>,
    ) -> Result<Option<Coupon>> {
        sqlx::query(
            "UPDATE coupons SET \
             name = COALESCE($2, name), \
             code = COALESCE($3, code), \
             amount_cents = COALESCE($4, amount_cents), \
             percentage_rate = COALESCE($5, percentage_rate), \
             frequency = COALESCE($6, frequency), \
             frequency_duration = COALESCE($7, frequency_duration), \
             reusable = COALESCE($8, reusable), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(code)
        .bind(amount_cents)
        .bind(percentage_rate)
        .bind(frequency)
        .bind(frequency_duration)
        .bind(reusable)
        .execute(&self.pool)
        .await?;
        self.find_coupon(id).await
    }

    /// `Coupons::DestroyService` — soft-delete.
    pub async fn soft_delete_coupon(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE coupons SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- applied coupons -------------------------------------------------

    pub async fn find_applied_coupon(&self, id: Uuid) -> Result<Option<AppliedCoupon>> {
        let row = sqlx::query("SELECT * FROM applied_coupons WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(applied_coupon).transpose()
    }

    /// `AppliedCoupons::TerminateService`.
    pub async fn terminate_applied_coupon(&self, id: Uuid) -> Result<Option<AppliedCoupon>> {
        sqlx::query("UPDATE applied_coupons SET status = 1, terminated_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.find_applied_coupon(id).await
    }

    /// `AppliedCouponsQuery` — the org's applied coupons, optionally scoped to a
    /// single customer (the `/customers/:id/applied_coupons` nested index).
    pub async fn list_applied_coupons(
        &self,
        org: Uuid,
        customer_id: Option<Uuid>,
    ) -> Result<Vec<AppliedCoupon>> {
        let rows = match customer_id {
            Some(cid) => {
                sqlx::query(
                    "SELECT * FROM applied_coupons WHERE organization_id = $1 AND customer_id = $2 \
                     ORDER BY created_at DESC",
                )
                .bind(org)
                .bind(cid)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT * FROM applied_coupons WHERE organization_id = $1 ORDER BY created_at DESC",
                )
                .bind(org)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(applied_coupon).collect()
    }

    // ---- subscriptions ---------------------------------------------------

    /// `Subscriptions::UpdateService` — update the mutable, non-lifecycle fields
    /// (name / dates). Status transitions go through
    /// [`Db::update_subscription`].
    pub async fn update_subscription_fields(
        &self,
        id: Uuid,
        name: Option<&str>,
        subscription_at: Option<NaiveDateTime>,
        ending_at: Option<NaiveDateTime>,
    ) -> Result<Option<Subscription>> {
        sqlx::query(
            "UPDATE subscriptions SET \
             name = COALESCE($2, name), \
             subscription_at = COALESCE($3, subscription_at), \
             ending_at = COALESCE($4, ending_at), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(subscription_at)
        .bind(ending_at)
        .execute(&self.pool)
        .await?;
        let row = sqlx::query("SELECT * FROM subscriptions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::subscription).transpose()
    }

    /// A subscription by id, or `None` (the optional counterpart to
    /// [`Db::find_subscription`]).
    pub async fn find_subscription_opt(&self, id: Uuid) -> Result<Option<Subscription>> {
        let row = sqlx::query("SELECT * FROM subscriptions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::subscription).transpose()
    }

    // ---- invoices --------------------------------------------------------

    /// `Invoices::RefreshDraftAndFinalizeService` — finalize a draft invoice
    /// (status → finalized, stamp issuing date if unset).
    pub async fn finalize_invoice(&self, id: Uuid) -> Result<Option<Invoice>> {
        sqlx::query(
            "UPDATE invoices SET status = 1, issuing_date = COALESCE(issuing_date, now()::date), updated_at = now() \
             WHERE id = $1 AND status = 0",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.find_invoice(id).await
    }

    /// `Invoices::UpdateService` — update an invoice's `payment_status`.
    pub async fn update_invoice(
        &self,
        id: Uuid,
        payment_status: Option<i32>,
    ) -> Result<Option<Invoice>> {
        sqlx::query("UPDATE invoices SET payment_status = COALESCE($2, payment_status), updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(payment_status)
            .execute(&self.pool)
            .await?;
        self.find_invoice(id).await
    }

    /// `Invoices::RefreshDraftService` (totals only) — recompute the invoice's
    /// monetary rollups from its persisted fees. This refreshes the aggregate
    /// columns; it does not regenerate the fees themselves (fee regeneration is
    /// the billing engine's job and is a documented follow-up).
    pub async fn refresh_invoice_totals(&self, id: Uuid) -> Result<Option<Invoice>> {
        let agg = sqlx::query(
            "SELECT COALESCE(SUM(amount_cents),0)::bigint AS fees, \
             COALESCE(SUM(taxes_amount_cents),0)::bigint AS taxes \
             FROM fees WHERE invoice_id = $1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        let fees: i64 = agg.get("fees");
        let taxes: i64 = agg.get("taxes");
        sqlx::query(
            "UPDATE invoices SET \
             fees_amount_cents = $2, \
             sub_total_excluding_taxes_amount_cents = $2, \
             taxes_amount_cents = $3, \
             sub_total_including_taxes_amount_cents = $2 + $3, \
             total_amount_cents = ($2 + $3) - coupons_amount_cents - credit_notes_amount_cents - prepaid_credit_amount_cents, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(fees)
        .bind(taxes)
        .execute(&self.pool)
        .await?;
        self.find_invoice(id).await
    }

    /// `Invoices::LoseDisputeService` — stamp the payment-dispute-lost time.
    pub async fn lose_invoice_dispute(&self, id: Uuid) -> Result<Option<Invoice>> {
        sqlx::query("UPDATE invoices SET payment_dispute_lost_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.find_invoice(id).await
    }

    // ---- credit notes ----------------------------------------------------

    /// `CreditNotes::UpdateService` — update the refund status.
    pub async fn update_credit_note_refund_status(
        &self,
        id: Uuid,
        refund_status: i32,
    ) -> Result<Option<CreditNote>> {
        sqlx::query("UPDATE credit_notes SET refund_status = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(refund_status)
            .execute(&self.pool)
            .await?;
        self.find_credit_note(id).await
    }

    /// `CreditNotes::VoidService` — void a credit note (credit_status → voided,
    /// zero the remaining balance, stamp `voided_at`).
    pub async fn void_credit_note(&self, id: Uuid) -> Result<Option<CreditNote>> {
        sqlx::query("UPDATE credit_notes SET credit_status = 2, balance_amount_cents = 0, voided_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.find_credit_note(id).await
    }

    /// Credit notes for an invoice.
    pub async fn credit_notes_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<CreditNote>> {
        let rows = sqlx::query("SELECT id FROM credit_notes WHERE invoice_id = $1 ORDER BY created_at DESC")
            .bind(invoice_id)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Some(cn) = self.find_credit_note(r.get::<Uuid, _>("id")).await? {
                out.push(cn);
            }
        }
        Ok(out)
    }

    /// Credit notes for a customer (the `/customers/:id/credit_notes` nested
    /// index). Ordered newest-first like `CreditNotesQuery`.
    pub async fn credit_notes_for_customer(&self, customer_id: Uuid) -> Result<Vec<CreditNote>> {
        let rows = sqlx::query("SELECT id FROM credit_notes WHERE customer_id = $1 ORDER BY created_at DESC")
            .bind(customer_id)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Some(cn) = self.find_credit_note(r.get::<Uuid, _>("id")).await? {
                out.push(cn);
            }
        }
        Ok(out)
    }

    /// A page of an organization's credit notes, newest-first — the unfiltered
    /// `/credit_notes` index (Lago `CreditNotesQuery` with no scope).
    pub async fn credit_notes_for_org(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CreditNote>> {
        let rows = sqlx::query(
            "SELECT id FROM credit_notes WHERE organization_id = $1 \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Some(cn) = self.find_credit_note(r.get::<Uuid, _>("id")).await? {
                out.push(cn);
            }
        }
        Ok(out)
    }

    /// Total credit notes for an organization (for pagination metadata).
    pub async fn count_credit_notes_for_org(&self, org: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS n FROM credit_notes WHERE organization_id = $1",
        )
        .bind(org)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("n"))
    }

    // ---- wallets ---------------------------------------------------------

    /// `Wallets::UpdateService` — update a wallet's name / expiration / priority.
    pub async fn update_wallet_details(
        &self,
        id: Uuid,
        name: Option<&str>,
        expiration_at: Option<NaiveDateTime>,
        priority: Option<i32>,
    ) -> Result<Option<Wallet>> {
        sqlx::query(
            "UPDATE wallets SET \
             name = COALESCE($2, name), \
             expiration_at = COALESCE($3, expiration_at), \
             priority = COALESCE($4, priority), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(expiration_at)
        .bind(priority)
        .execute(&self.pool)
        .await?;
        self.find_wallet(id).await
    }

    // ---- taxes -----------------------------------------------------------

    /// `Taxes::CreateService`.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_tax(
        &self,
        id: Uuid,
        org: Uuid,
        name: &str,
        code: &str,
        description: Option<&str>,
        rate: f64,
        applied_to_organization: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO taxes (id, organization_id, name, code, description, rate, applied_to_organization, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(name)
        .bind(code)
        .bind(description)
        .bind(rate)
        .bind(applied_to_organization)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `Taxes::UpdateService`.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_tax(
        &self,
        id: Uuid,
        name: Option<&str>,
        code: Option<&str>,
        description: Option<&str>,
        rate: Option<f64>,
        applied_to_organization: Option<bool>,
    ) -> Result<Option<Tax>> {
        sqlx::query(
            "UPDATE taxes SET \
             name = COALESCE($2, name), \
             code = COALESCE($3, code), \
             description = COALESCE($4, description), \
             rate = COALESCE($5, rate), \
             applied_to_organization = COALESCE($6, applied_to_organization), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(code)
        .bind(description)
        .bind(rate)
        .bind(applied_to_organization)
        .execute(&self.pool)
        .await?;
        self.find_tax(id).await
    }

    /// `Taxes::DestroyService` — hard delete (taxes have no `deleted_at`).
    pub async fn delete_tax(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM taxes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_tax(&self, id: Uuid) -> Result<Option<Tax>> {
        let row = sqlx::query("SELECT * FROM taxes WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::tax).transpose()
    }

    pub async fn list_taxes(&self, org: Uuid) -> Result<Vec<Tax>> {
        let rows = sqlx::query("SELECT * FROM taxes WHERE organization_id = $1 ORDER BY created_at DESC")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(mapping::tax).collect()
    }

    // ---- dunning campaigns ----------------------------------------------

    /// `DunningCampaigns::UpdateService`.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_dunning_campaign(
        &self,
        id: Uuid,
        name: Option<&str>,
        code: Option<&str>,
        description: Option<&str>,
        max_attempts: Option<i32>,
        days_between_attempts: Option<i32>,
        applied_to_organization: Option<bool>,
    ) -> Result<Option<DunningCampaignRow>> {
        sqlx::query(
            "UPDATE dunning_campaigns SET \
             name = COALESCE($2, name), \
             code = COALESCE($3, code), \
             description = COALESCE($4, description), \
             max_attempts = COALESCE($5, max_attempts), \
             days_between_attempts = COALESCE($6, days_between_attempts), \
             applied_to_organization = COALESCE($7, applied_to_organization), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(code)
        .bind(description)
        .bind(max_attempts)
        .bind(days_between_attempts)
        .bind(applied_to_organization)
        .execute(&self.pool)
        .await?;
        self.find_dunning_campaign(id).await
    }

    /// `DunningCampaigns::DestroyService` — hard delete.
    pub async fn delete_dunning_campaign(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM dunning_campaigns WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_dunning_campaign(&self, id: Uuid) -> Result<Option<DunningCampaignRow>> {
        let row = sqlx::query("SELECT id, name, code, description, max_attempts, days_between_attempts, applied_to_organization FROM dunning_campaigns WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("name"),
                r.get::<String, _>("code"),
                r.get::<Option<String>, _>("description"),
                r.get::<i32, _>("max_attempts"),
                r.get::<i32, _>("days_between_attempts"),
                r.get::<bool, _>("applied_to_organization"),
            )
        }))
    }

    // ---- webhook endpoints ----------------------------------------------

    /// `WebhookEndpoints::UpdateService`.
    pub async fn update_webhook_endpoint(
        &self,
        id: Uuid,
        webhook_url: Option<&str>,
        signature_algo: Option<&str>,
    ) -> Result<Option<(Uuid, String, String)>> {
        sqlx::query(
            "UPDATE webhook_endpoints SET \
             webhook_url = COALESCE($2, webhook_url), \
             signature_algo = COALESCE($3, signature_algo), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(webhook_url)
        .bind(signature_algo)
        .execute(&self.pool)
        .await?;
        self.find_webhook_endpoint(id).await
    }

    /// `WebhookEndpoints::DestroyService`.
    pub async fn delete_webhook_endpoint(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM webhook_endpoints WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_webhook_endpoint(&self, id: Uuid) -> Result<Option<(Uuid, String, String)>> {
        let row = sqlx::query("SELECT id, webhook_url, signature_algo FROM webhook_endpoints WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("webhook_url"),
                r.get::<String, _>("signature_algo"),
            )
        }))
    }

    // ---- webhooks --------------------------------------------------------

    pub async fn find_webhook(&self, id: Uuid) -> Result<Option<WebhookRow>> {
        let row = sqlx::query("SELECT id, webhook_type, object_type, status, http_status, endpoint, retries FROM webhooks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(webhook_row))
    }

    /// `Webhooks::RetryService` — re-enqueue a webhook for delivery.
    pub async fn retry_webhook(&self, id: Uuid) -> Result<Option<WebhookRow>> {
        sqlx::query("UPDATE webhooks SET status = 'retrying', next_retry_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.find_webhook(id).await
    }
}
