//! GraphQL admin CRUD mutations — the update/destroy/finalize half of the
//! dashboard's catalog + billing management. Each resolver mirrors a Lago
//! `Mutations::<Domain>::<Action>` and delegates to the shared `rago-db`
//! operations (the same persistence the REST API uses). Every resolver is gated
//! behind the authenticated organization via [`require_org`].
//!
//! Destroy mutations return the deleted resource's `id` (Lago's destroy payloads
//! expose a single `id` field). Download mutations return the resource object
//! (PDF rendering is an async job; the object carries the reference).

use async_graphql::{Context, Object, Result, ID};
use chrono::Utc;
use rago_core::models as m;
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;
use uuid::Uuid;

use crate::admin_inputs::*;
use crate::context::{db, require_org};
use crate::types::*;
use crate::types_admin::*;
use crate::webhook_emit::emit;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}
fn parse_ts(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.naive_utc())
        .map_err(|_| async_graphql::Error::new("invalid_timestamp"))
}

fn charge_model_i32(s: &str) -> Option<i32> {
    use rago_core::enums::ChargeModel::*;
    Some(match s {
        "standard" => Standard,
        "graduated" => Graduated,
        "package" => Package,
        "percentage" => Percentage,
        "volume" => Volume,
        "graduated_percentage" => GraduatedPercentage,
        "custom" => Custom,
        "dynamic" => Dynamic,
        _ => return None,
    }
    .as_i32())
}
fn aggregation_i32(s: &str) -> Option<i32> {
    use rago_core::enums::AggregationType::*;
    Some(match s {
        "count_agg" => CountAgg,
        "sum_agg" => SumAgg,
        "max_agg" => MaxAgg,
        "unique_count_agg" => UniqueCountAgg,
        // weighted_sum_agg / custom_agg are not accepted: the billing engine
        // cannot compute fees for them, so a metric using one would fail every
        // invoice it appears on.
        "latest_agg" => LatestAgg,
        _ => return None,
    }
    .as_i32())
}
fn interval_i32(s: &str) -> Option<i32> {
    use rago_core::enums::PlanInterval::*;
    Some(match s {
        "weekly" => Weekly,
        "monthly" => Monthly,
        "yearly" => Yearly,
        "quarterly" => Quarterly,
        "semiannual" => Semiannual,
        _ => return None,
    }
    .as_i32())
}
fn coupon_frequency_i32(s: &str) -> Option<i32> {
    use rago_core::enums::CouponFrequency::*;
    Some(match s {
        "once" => Once,
        "recurring" => Recurring,
        "forever" => Forever,
        _ => return None,
    }
    .as_i32())
}
fn payment_status_i32(s: &str) -> Option<i32> {
    use rago_core::enums::PaymentStatus::*;
    Some(match s {
        "pending" => Pending,
        "succeeded" => Succeeded,
        "failed" => Failed,
        "refunded" => Refunded,
        _ => return None,
    }
    .as_i32())
}
fn credit_note_reason_i32(s: &str) -> Option<i32> {
    Some(match s {
        "duplicated_charge" => 0,
        "product_unsatisfactory" => 1,
        "order_change" => 2,
        "order_cancellation" => 3,
        "fraudulent_charge" => 4,
        "other" => 5,
        _ => return None,
    })
}
fn refund_status_i32(s: &str) -> Option<i32> {
    Some(match s {
        "pending" => 0,
        "succeeded" => 1,
        "failed" => 2,
        _ => return None,
    })
}

async fn load_plan(ctx: &Context<'_>, plan: m::Plan) -> Result<Plan> {
    let charges = db(ctx).charges_for_plan(plan.id).await?;
    Ok(Plan { plan, charges })
}

async fn load_invoice(ctx: &Context<'_>, invoice: m::Invoice) -> Result<Invoice> {
    let fees = db(ctx).fees_for_invoice(invoice.id).await?;
    Ok(Invoice { invoice, fees })
}

async fn wallet_out(ctx: &Context<'_>, wallet: m::Wallet) -> Result<Wallet> {
    let ext = db(ctx).customer_external_id(wallet.customer_id).await?.unwrap_or_default();
    Ok(Wallet { wallet, external_customer_id: ext })
}

fn dunning_out(row: rago_db::DunningCampaignRow) -> DunningCampaign {
    let (id, name, code, _description, max, days, applied) = row;
    DunningCampaign {
        id: id.into(),
        name,
        code,
        max_attempts: max as i64,
        days_between_attempts: days as i64,
        applied_to_organization: applied,
    }
}

/// Admin CRUD mutation root (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation;

#[Object]
impl AdminMutation {
    // ---- customers -------------------------------------------------------

    /// `UpdateCustomer` (`Mutations::Customers::Update`).
    async fn update_customer(&self, ctx: &Context<'_>, input: UpdateCustomerInput) -> Result<Customer> {
        let org = require_org(ctx)?;
        let mut customer = db(ctx)
            .find_customer(uuid(&input.id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        customer.name = input.name.or(customer.name);
        customer.email = input.email.or(customer.email);
        customer.country = input.country.or(customer.country);
        customer.currency = input.currency.or(customer.currency);
        customer.timezone = input.timezone.or(customer.timezone);
        customer.tax_identification_number =
            input.tax_identification_number.or(customer.tax_identification_number);
        customer.updated_at = Utc::now();
        db(ctx).update_customer(&customer).await?;
        if let Some(gp) = input.invoice_grace_period {
            db(ctx).update_customer_invoice_grace_period(customer.id, Some(gp)).await?;
        }
        Ok(Customer(customer))
    }

    /// `DestroyCustomer` (`Mutations::Customers::Destroy`).
    async fn destroy_customer(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_customer(uid).await?;
        Ok(uid.into())
    }

    /// `UpdateCustomerInvoiceGracePeriod`
    /// (`Mutations::Customers::UpdateInvoiceGracePeriod`).
    async fn update_customer_invoice_grace_period(
        &self,
        ctx: &Context<'_>,
        id: ID,
        invoice_grace_period: Option<i32>,
    ) -> Result<Customer> {
        require_org(ctx)?;
        let customer = db(ctx)
            .update_customer_invoice_grace_period(uuid(&id)?, invoice_grace_period)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        Ok(Customer(customer))
    }

    // ---- plans -----------------------------------------------------------

    /// `UpdatePlan` (`Mutations::Plans::Update`).
    async fn update_plan(&self, ctx: &Context<'_>, input: UpdatePlanInput) -> Result<Plan> {
        let org = require_org(ctx)?;
        let org_id = org.id;
        let interval = match input.interval.as_deref() {
            Some(s) => Some(interval_i32(s).ok_or_else(|| async_graphql::Error::new("invalid interval"))?),
            None => None,
        };
        let plan = db(ctx)
            .update_plan(
                uuid(&input.id)?,
                input.name.as_deref(),
                input.description.as_deref(),
                input.amount_cents,
                input.amount_currency.as_deref(),
                input.pay_in_advance,
                interval,
                input.invoice_display_name.as_deref(),
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("plan_not_found"))?;
        // `Plans::UpdateService` emits `plan.updated`.
        emit(db(ctx), org_id, WebhookKind::PlanUpdated, json!({ "lago_id": plan.id, "code": plan.code, "name": plan.name })).await;
        load_plan(ctx, plan).await
    }

    /// `DestroyPlan` (`Mutations::Plans::Destroy`).
    async fn destroy_plan(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        let org = require_org(ctx)?;
        let org_id = org.id;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_plan(uid).await?;
        // `Plans::DestroyService` emits `plan.deleted`.
        emit(db(ctx), org_id, WebhookKind::PlanDeleted, json!({ "lago_id": uid })).await;
        Ok(uid.into())
    }

    // ---- charges + charge filters ---------------------------------------

    /// `CreateCharge` (`Mutations::Charges::Create`).
    async fn create_charge(&self, ctx: &Context<'_>, input: CreateChargeInput) -> Result<Charge> {
        let org = require_org(ctx)?;
        let metric = db(ctx)
            .find_billable_metric_opt(uuid(&input.billable_metric_id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("billable_metric_not_found"))?;
        let charge_model = charge_model_i32(&input.charge_model)
            .ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?;
        let charge = m::Charge {
            id: Uuid::new_v4(),
            organization_id: org.id,
            billable_metric_id: Some(metric.id),
            plan_id: Some(uuid(&input.plan_id)?),
            code: metric.code.clone(),
            charge_model: rago_core::enums::ChargeModel::try_from(charge_model).unwrap(),
            properties: input.properties.map(|j| j.0).unwrap_or_else(|| serde_json::json!({})),
            pay_in_advance: input.pay_in_advance.unwrap_or(false),
            prorated: input.prorated.unwrap_or(false),
            invoiceable: input.invoiceable.unwrap_or(true),
            min_amount_cents: input.min_amount_cents.unwrap_or(0),
            amount_currency: None,
            accepts_target_wallet: false,
            invoice_display_name: input.invoice_display_name,
            parent_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        };
        db(ctx).insert_charge(&charge).await?;
        Ok(Charge(charge))
    }

    /// `UpdateCharge` (`Mutations::Charges::Update`).
    async fn update_charge(&self, ctx: &Context<'_>, input: UpdateChargeInput) -> Result<Charge> {
        require_org(ctx)?;
        let charge_model = match input.charge_model.as_deref() {
            Some(s) => Some(charge_model_i32(s).ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?),
            None => None,
        };
        let props = input.properties.map(|j| j.0);
        let charge = db(ctx)
            .update_charge(
                uuid(&input.id)?,
                charge_model,
                props.as_ref(),
                input.pay_in_advance,
                input.prorated,
                input.invoiceable,
                input.min_amount_cents,
                input.invoice_display_name.as_deref(),
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("charge_not_found"))?;
        Ok(Charge(charge))
    }

    /// `DestroyCharge` (`Mutations::Charges::Destroy`).
    async fn destroy_charge(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_charge(uid).await?;
        Ok(uid.into())
    }

    /// `CreateChargeFilter` (`Mutations::ChargeFilters::Create`).
    async fn create_charge_filter(
        &self,
        ctx: &Context<'_>,
        input: CreateChargeFilterInput,
    ) -> Result<ChargeFilter> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let charge_id = uuid(&input.charge_id)?;
        db(ctx)
            .insert_charge_filter(id, org.id, charge_id, &input.properties.0, input.invoice_display_name.as_deref())
            .await?;
        let cf = db(ctx)
            .find_charge_filter(id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("charge_filter_not_found"))?;
        Ok(ChargeFilter(cf))
    }

    /// `UpdateChargeFilter` (`Mutations::ChargeFilters::Update`).
    async fn update_charge_filter(
        &self,
        ctx: &Context<'_>,
        input: UpdateChargeFilterInput,
    ) -> Result<ChargeFilter> {
        require_org(ctx)?;
        let props = input.properties.map(|j| j.0);
        let cf = db(ctx)
            .update_charge_filter(uuid(&input.id)?, props.as_ref(), input.invoice_display_name.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("charge_filter_not_found"))?;
        Ok(ChargeFilter(cf))
    }

    /// `DestroyChargeFilter` (`Mutations::ChargeFilters::Destroy`).
    async fn destroy_charge_filter(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_charge_filter(uid).await?;
        Ok(uid.into())
    }

    // ---- billable metrics -----------------------------------------------

    /// `UpdateBillableMetric` (`Mutations::BillableMetrics::Update`).
    async fn update_billable_metric(
        &self,
        ctx: &Context<'_>,
        input: UpdateBillableMetricInput,
    ) -> Result<BillableMetric> {
        let org = require_org(ctx)?;
        let org_id = org.id;
        let aggregation = match input.aggregation_type.as_deref() {
            Some(s) => Some(aggregation_i32(s).ok_or_else(|| async_graphql::Error::new("invalid aggregation_type"))?),
            None => None,
        };
        let metric = db(ctx)
            .update_billable_metric(
                uuid(&input.id)?,
                input.name.as_deref(),
                input.description.as_deref(),
                aggregation,
                input.field_name.as_deref(),
                input.recurring,
                input.expression.as_deref(),
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("billable_metric_not_found"))?;
        // `BillableMetrics::UpdateService` emits `billable_metric.updated`.
        emit(db(ctx), org_id, WebhookKind::BillableMetricUpdated, json!({ "lago_id": metric.id, "code": metric.code, "name": metric.name })).await;
        Ok(BillableMetric(metric))
    }

    /// `DestroyBillableMetric` (`Mutations::BillableMetrics::Destroy`).
    async fn destroy_billable_metric(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        let org = require_org(ctx)?;
        let org_id = org.id;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_billable_metric(uid).await?;
        // `BillableMetrics::DestroyService` emits `billable_metric.deleted`.
        emit(db(ctx), org_id, WebhookKind::BillableMetricDeleted, json!({ "lago_id": uid })).await;
        Ok(uid.into())
    }

    // ---- add-ons ---------------------------------------------------------

    /// `UpdateAddOn` (`Mutations::AddOns::Update`).
    async fn update_add_on(&self, ctx: &Context<'_>, input: UpdateAddOnInput) -> Result<AddOn> {
        require_org(ctx)?;
        let (id, name, code, _desc, amount_cents, currency) = db(ctx)
            .update_add_on(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), input.amount_cents)
            .await?
            .ok_or_else(|| async_graphql::Error::new("add_on_not_found"))?;
        Ok(AddOn { id: id.into(), name, code, amount_cents, amount_currency: currency })
    }

    /// `DestroyAddOn` (`Mutations::AddOns::Destroy`).
    async fn destroy_add_on(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_add_on(uid).await?;
        Ok(uid.into())
    }

    // ---- coupons + applied coupons --------------------------------------

    /// `UpdateCoupon` (`Mutations::Coupons::Update`).
    async fn update_coupon(&self, ctx: &Context<'_>, input: UpdateCouponInput) -> Result<Coupon> {
        require_org(ctx)?;
        let frequency = match input.frequency.as_deref() {
            Some(s) => Some(coupon_frequency_i32(s).ok_or_else(|| async_graphql::Error::new("invalid frequency"))?),
            None => None,
        };
        let percentage_rate = match input.percentage_rate.as_deref() {
            Some(s) => Some(s.parse().map_err(|_| async_graphql::Error::new("invalid percentage_rate"))?),
            None => None,
        };
        let coupon = db(ctx)
            .update_coupon(
                uuid(&input.id)?,
                input.name.as_deref(),
                input.code.as_deref(),
                input.amount_cents,
                percentage_rate,
                frequency,
                input.frequency_duration,
                input.reusable,
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("coupon_not_found"))?;
        Ok(Coupon(coupon))
    }

    /// `DestroyCoupon` (`Mutations::Coupons::Destroy`).
    async fn destroy_coupon(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_coupon(uid).await?;
        Ok(uid.into())
    }

    /// `CreateAppliedCoupon` (`Mutations::AppliedCoupons::Create`).
    async fn create_applied_coupon(
        &self,
        ctx: &Context<'_>,
        coupon_id: ID,
        customer_id: ID,
        amount_cents: Option<i64>,
        amount_currency: Option<String>,
        frequency: Option<String>,
        frequency_duration: Option<i32>,
        percentage_rate: Option<String>,
    ) -> Result<AppliedCoupon> {
        let org = require_org(ctx)?;
        let coupon = db(ctx)
            .find_coupon(uuid(&coupon_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("coupon_not_found"))?;
        let customer = db(ctx)
            .find_customer(uuid(&customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let frequency = match frequency.as_deref() {
            Some(s) => rago_core::enums::CouponFrequency::try_from(
                coupon_frequency_i32(s).ok_or_else(|| async_graphql::Error::new("invalid frequency"))?,
            )
            .unwrap(),
            None => coupon.frequency,
        };
        let percentage_rate = match percentage_rate.as_deref() {
            Some(s) => Some(s.parse().map_err(|_| async_graphql::Error::new("invalid percentage_rate"))?),
            None => coupon.percentage_rate,
        };
        let applied = m::AppliedCoupon {
            id: Uuid::new_v4(),
            organization_id: org.id,
            coupon_id: coupon.id,
            customer_id: customer.id,
            status: rago_core::enums::CouponStatus::Active,
            amount_cents: amount_cents.or(coupon.amount_cents),
            amount_currency: amount_currency.or(coupon.amount_currency.clone()),
            percentage_rate,
            frequency,
            frequency_duration: frequency_duration.or(coupon.frequency_duration),
            frequency_duration_remaining: frequency_duration.or(coupon.frequency_duration),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminated_at: None,
        };
        db(ctx).insert_applied_coupon(&applied).await?;
        Ok(AppliedCoupon(applied))
    }

    /// `TerminateAppliedCoupon` (`Mutations::AppliedCoupons::Terminate`).
    async fn terminate_applied_coupon(&self, ctx: &Context<'_>, id: ID) -> Result<AppliedCoupon> {
        require_org(ctx)?;
        let applied = db(ctx)
            .terminate_applied_coupon(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("applied_coupon_not_found"))?;
        Ok(AppliedCoupon(applied))
    }

    // ---- subscriptions ---------------------------------------------------

    /// `UpdateSubscription` (`Mutations::Subscriptions::Update`).
    async fn update_subscription(
        &self,
        ctx: &Context<'_>,
        input: UpdateSubscriptionInput,
    ) -> Result<Subscription> {
        let org = require_org(ctx)?;
        let org_id = org.id;
        let subscription_at = match input.subscription_at.as_deref() {
            Some(s) => Some(parse_ts(s)?),
            None => None,
        };
        let ending_at = match input.ending_at.as_deref() {
            Some(s) => Some(parse_ts(s)?),
            None => None,
        };
        let sub = db(ctx)
            .update_subscription_fields(uuid(&input.id)?, input.name.as_deref(), subscription_at, ending_at)
            .await?
            .ok_or_else(|| async_graphql::Error::new("subscription_not_found"))?;
        // `Subscriptions::UpdateService` emits `subscription.updated`.
        emit(db(ctx), org_id, WebhookKind::SubscriptionUpdated, json!({ "lago_id": sub.id, "external_id": sub.external_id })).await;
        let plan = db(ctx).find_plan(sub.plan_id).await?;
        let ext = db(ctx).customer_external_id(sub.customer_id).await?.unwrap_or_default();
        Ok(Subscription {
            plan_code: plan.code.clone(),
            plan_amount_cents: plan.amount_cents,
            plan_amount_currency: plan.amount_currency.clone(),
            external_customer_id: ext,
            sub,
        })
    }

    // ---- invoices --------------------------------------------------------

    /// `FinalizeInvoice` (`Mutations::Invoices::Finalize`).
    async fn finalize_invoice(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        require_org(ctx)?;
        let invoice = db(ctx)
            .finalize_invoice(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `RefreshInvoice` (`Mutations::Invoices::Refresh`) — recomputes the
    /// invoice's monetary rollups from its persisted fees.
    async fn refresh_invoice(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        require_org(ctx)?;
        let invoice = db(ctx)
            .refresh_invoice_totals(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `UpdateInvoice` (`Mutations::Invoices::Update`).
    async fn update_invoice(&self, ctx: &Context<'_>, input: UpdateInvoiceInput) -> Result<Invoice> {
        require_org(ctx)?;
        let payment_status = match input.payment_status.as_deref() {
            Some(s) => Some(payment_status_i32(s).ok_or_else(|| async_graphql::Error::new("invalid payment_status"))?),
            None => None,
        };
        let invoice = db(ctx)
            .update_invoice(uuid(&input.id)?, payment_status)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `LoseInvoiceDispute` (`Mutations::Invoices::LoseDispute`).
    async fn lose_invoice_dispute(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        require_org(ctx)?;
        let invoice = db(ctx)
            .lose_invoice_dispute(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `DownloadInvoice` (`Mutations::Invoices::Download`) — returns the invoice
    /// with its `fileUrl`; the PDF is rendered on request at that URL.
    async fn download_invoice(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    // ---- credit notes ----------------------------------------------------

    /// `CreateCreditNote` (`Mutations::CreditNotes::Create`).
    ///
    /// Persists the credit-note header (amounts, reason, refund/credit status),
    /// the per-item `credit_note_items` rows, and taxes recomputed from the
    /// source invoice's effective rate. When `credit_amount_cents` is omitted the
    /// item amounts are summed to derive the credited amount.
    async fn create_credit_note(
        &self,
        ctx: &Context<'_>,
        input: CreateCreditNoteInput,
    ) -> Result<CreditNote> {
        let org = require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&input.invoice_id)?)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let reason = credit_note_reason_i32(&input.reason)
            .ok_or_else(|| async_graphql::Error::new("invalid reason"))?;
        let items_total: i64 = input.items.iter().map(|i| i.amount_cents).sum();
        let credit = input.credit_amount_cents.unwrap_or(items_total);
        let refund = input.refund_amount_cents.unwrap_or(0);
        let total = credit + refund;
        let currency = invoice.currency.clone().unwrap_or_else(|| "USD".into());
        let customer_id = invoice
            .customer_id
            .ok_or_else(|| async_graphql::Error::new("invoice_customer_missing"))?;
        // Recompute taxes from the source invoice's effective tax rate.
        let taxes_rate = invoice.taxes_rate;
        let taxes_amount_cents = ((total as f64) * taxes_rate / 100.0).round() as i64;
        let seq = db(ctx).next_credit_note_sequential_id(invoice.id).await?;
        let cn = m::CreditNote {
            id: Uuid::new_v4(),
            organization_id: org.id,
            customer_id,
            invoice_id: invoice.id,
            sequential_id: seq,
            number: format!("{}-CN{seq}", invoice.number),
            issuing_date: Utc::now().date_naive(),
            reason,
            credit_status: (credit > 0).then_some(0),
            refund_status: (refund > 0).then_some(0),
            status: 1,
            credit_amount_cents: credit,
            credit_amount_currency: currency.clone(),
            balance_amount_cents: credit,
            refund_amount_cents: refund,
            total_amount_cents: total,
            total_amount_currency: currency.clone(),
            taxes_amount_cents,
            taxes_rate,
            coupons_adjustment_amount_cents: 0,
            precise_coupons_adjustment_amount_cents: rust_decimal::Decimal::ZERO,
            precise_taxes_amount_cents: rust_decimal::Decimal::from(taxes_amount_cents),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            voided_at: None,
            items: Vec::new(),
            applied_taxes: Vec::new(),
        };
        db(ctx).insert_credit_note(&cn).await?;
        // Persist per-item credit lines against their fees.
        for item in &input.items {
            db(ctx)
                .insert_credit_note_item(
                    Uuid::new_v4(),
                    org.id,
                    cn.id,
                    uuid(&item.fee_id)?,
                    item.amount_cents,
                    &currency,
                )
                .await?;
        }
        // `CreditNotes::CreateService` emits `credit_note.created`.
        emit(db(ctx), org.id, WebhookKind::CreditNoteCreated, json!({ "lago_id": cn.id, "number": cn.number, "lago_invoice_id": cn.invoice_id, "total_amount_cents": cn.total_amount_cents })).await;
        let _ = rago_jobs::webhook_emit::record_credit_note_generated(
            db(ctx),
            org.id,
            cn.id,
            json!({ "number": cn.number, "lago_invoice_id": cn.invoice_id, "total_amount_cents": cn.total_amount_cents, "currency": cn.total_amount_currency }),
        )
        .await;
        // Pay the refund portion back through the provider that captured the
        // invoice's payment; refund_status reflects the provider's answer.
        if cn.refund_amount_cents > 0 {
            let transport = rago_integrations::default_transport();
            match rago_jobs::refunds::refund_invoice(db(ctx), &transport, &invoice, Some(cn.refund_amount_cents), Some(&cn)).await {
                Ok(Ok(_)) => {}
                Ok(Err(rejection)) => {
                    db(ctx).update_credit_note_refund_status(cn.id, 2).await?;
                    return Err(async_graphql::Error::new(format!("credit_note_created_refund_rejected: {}", rejection.code())));
                }
                Err(e) => return Err(async_graphql::Error::new(format!("credit_note_created_refund_error: {e}"))),
            }
            if let Some(updated) = db(ctx).find_credit_note(cn.id).await? {
                return Ok(CreditNote(updated));
            }
        }
        Ok(CreditNote(cn))
    }

    /// `UpdateCreditNote` (`Mutations::CreditNotes::Update`).
    async fn update_credit_note(
        &self,
        ctx: &Context<'_>,
        input: UpdateCreditNoteInput,
    ) -> Result<CreditNote> {
        require_org(ctx)?;
        let refund_status = refund_status_i32(&input.refund_status)
            .ok_or_else(|| async_graphql::Error::new("invalid refund_status"))?;
        let cn = db(ctx)
            .update_credit_note_refund_status(uuid(&input.id)?, refund_status)
            .await?
            .ok_or_else(|| async_graphql::Error::new("credit_note_not_found"))?;
        Ok(CreditNote(cn))
    }

    /// `VoidCreditNote` (`Mutations::CreditNotes::Void`).
    async fn void_credit_note(&self, ctx: &Context<'_>, id: ID) -> Result<CreditNote> {
        require_org(ctx)?;
        let cn = db(ctx)
            .void_credit_note(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("credit_note_not_found"))?;
        Ok(CreditNote(cn))
    }

    /// `DownloadCreditNote` (`Mutations::CreditNotes::Download`) — returns the
    /// credit note with its `fileUrl`; the PDF is rendered on request there.
    async fn download_credit_note(&self, ctx: &Context<'_>, id: ID) -> Result<CreditNote> {
        require_org(ctx)?;
        let cn = db(ctx)
            .find_credit_note(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("credit_note_not_found"))?;
        Ok(CreditNote(cn))
    }

    // ---- wallets ---------------------------------------------------------

    /// `UpdateCustomerWallet` (`Mutations::Wallets::Update`).
    async fn update_customer_wallet(
        &self,
        ctx: &Context<'_>,
        input: UpdateCustomerWalletInput,
    ) -> Result<Wallet> {
        require_org(ctx)?;
        let expiration_at = match input.expiration_at.as_deref() {
            Some(s) => Some(parse_ts(s)?),
            None => None,
        };
        let wallet = db(ctx)
            .update_wallet_details(uuid(&input.id)?, input.name.as_deref(), expiration_at, input.priority)
            .await?
            .ok_or_else(|| async_graphql::Error::new("wallet_not_found"))?;
        wallet_out(ctx, wallet).await
    }

    // ---- taxes -----------------------------------------------------------

    /// `CreateTax` (`Mutations::Taxes::Create`).
    async fn create_tax(&self, ctx: &Context<'_>, input: CreateTaxInput) -> Result<Tax> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        db(ctx)
            .insert_tax(id, org.id, &input.name, &input.code, input.description.as_deref(), input.rate, false)
            .await?;
        let tax = db(ctx).find_tax(id).await?.ok_or_else(|| async_graphql::Error::new("tax_not_found"))?;
        Ok(Tax(tax))
    }

    /// `UpdateTax` (`Mutations::Taxes::Update`).
    async fn update_tax(&self, ctx: &Context<'_>, input: UpdateTaxInput) -> Result<Tax> {
        require_org(ctx)?;
        let tax = db(ctx)
            .update_tax(
                uuid(&input.id)?,
                input.name.as_deref(),
                input.code.as_deref(),
                input.description.as_deref(),
                input.rate,
                input.applied_to_organization,
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("tax_not_found"))?;
        Ok(Tax(tax))
    }

    /// `DestroyTax` (`Mutations::Taxes::Destroy`).
    async fn destroy_tax(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).delete_tax(uid).await?;
        Ok(uid.into())
    }

    // ---- dunning campaigns ----------------------------------------------

    /// `UpdateDunningCampaign` (`Mutations::DunningCampaigns::Update`).
    async fn update_dunning_campaign(
        &self,
        ctx: &Context<'_>,
        input: UpdateDunningCampaignInput,
    ) -> Result<DunningCampaign> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_dunning_campaign(
                uuid(&input.id)?,
                input.name.as_deref(),
                input.code.as_deref(),
                input.description.as_deref(),
                input.max_attempts,
                input.days_between_attempts,
                input.applied_to_organization,
            )
            .await?
            .ok_or_else(|| async_graphql::Error::new("dunning_campaign_not_found"))?;
        Ok(dunning_out(row))
    }

    /// `DestroyDunningCampaign` (`Mutations::DunningCampaigns::Destroy`).
    async fn destroy_dunning_campaign(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).delete_dunning_campaign(uid).await?;
        Ok(uid.into())
    }

    // ---- webhook endpoints + webhooks -----------------------------------

    /// `UpdateWebhookEndpoint` (`Mutations::WebhookEndpoints::Update`).
    async fn update_webhook_endpoint(
        &self,
        ctx: &Context<'_>,
        input: UpdateWebhookEndpointInput,
    ) -> Result<WebhookEndpoint> {
        require_org(ctx)?;
        let (id, webhook_url, signature_algo) = db(ctx)
            .update_webhook_endpoint(uuid(&input.id)?, input.webhook_url.as_deref(), input.signature_algo.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("webhook_endpoint_not_found"))?;
        Ok(WebhookEndpoint { id: id.into(), webhook_url, signature_algo })
    }

    /// `DestroyWebhookEndpoint` (`Mutations::WebhookEndpoints::Destroy`).
    async fn destroy_webhook_endpoint(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).delete_webhook_endpoint(uid).await?;
        Ok(uid.into())
    }

    /// `RetryWebhook` (`Mutations::Webhooks::Retry`).
    async fn retry_webhook(&self, ctx: &Context<'_>, id: ID) -> Result<Webhook> {
        require_org(ctx)?;
        let row = db(ctx)
            .retry_webhook(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("webhook_not_found"))?;
        Ok(row.into())
    }
}
