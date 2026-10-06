//! GraphQL Mutation root — faithful port of `Types::MutationType`'s core
//! fields. Each mutation delegates to the same `rago-db` + `rago-billing`
//! services the REST handlers use (no divergent logic), gated behind the
//! authenticated organization.

use async_graphql::{Context, InputObject, Json, Object, Result};
use chrono::Utc;
use rago_billing::generate::ChargeInput;
use rago_billing::wallets::{build_transaction, increase_balance, WalletCredit};
use rago_core::enums::*;
use rago_core::models as m;
use rago_core::money::Currency;
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use rago_integrations::webhooks::WebhookKind;

use crate::context::{db, require_org};
use crate::types::*;
use crate::webhook_emit::emit;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

// ---- inputs ---------------------------------------------------------------

#[derive(InputObject)]
pub struct CreateCustomerInput {
    pub external_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    pub currency: Option<String>,
    pub timezone: Option<String>,
    pub tax_identification_number: Option<String>,
}

#[derive(InputObject)]
pub struct CreateBillableMetricInput {
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub aggregation_type: String,
    pub field_name: Option<String>,
    pub recurring: Option<bool>,
    /// Required for `custom_agg`: per-event amount expression.
    pub custom_aggregator: Option<String>,
}

#[derive(InputObject)]
pub struct ChargeInputGql {
    pub billable_metric_code: String,
    pub charge_model: String,
    pub properties: Option<Json<serde_json::Value>>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoiceable: Option<bool>,
}

#[derive(InputObject)]
pub struct CreatePlanInput {
    pub name: String,
    pub code: String,
    pub interval: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub description: Option<String>,
    pub pay_in_advance: Option<bool>,
    pub charges: Option<Vec<ChargeInputGql>>,
    /// Progressive-billing thresholds (inherited by the plan's subscriptions).
    pub usage_thresholds: Option<Vec<crate::thresholds::UsageThresholdInput>>,
}

#[derive(InputObject)]
pub struct CreateSubscriptionInput {
    pub external_customer_id: String,
    pub plan_code: String,
    pub external_id: Option<String>,
    pub name: Option<String>,
    pub billing_time: Option<String>,
}

#[derive(InputObject)]
pub struct TerminateSubscriptionInput {
    pub external_id: String,
}

#[derive(InputObject)]
pub struct CreateWalletInput {
    pub external_customer_id: String,
    pub name: Option<String>,
    pub currency: Option<String>,
    pub rate_amount: Option<String>,
    pub paid_credits: Option<String>,
    pub granted_credits: Option<String>,
}

#[derive(InputObject)]
pub struct CreateWalletTransactionInput {
    pub wallet_id: String,
    pub paid_credits: Option<String>,
    pub granted_credits: Option<String>,
}

#[derive(InputObject)]
pub struct CreateCouponInput {
    pub name: String,
    pub code: String,
    pub coupon_type: String,
    pub amount_cents: Option<i64>,
    pub amount_currency: Option<String>,
    pub percentage_rate: Option<String>,
    pub frequency: String,
    pub frequency_duration: Option<i32>,
}

#[derive(InputObject)]
pub struct CreateSubscriptionInvoiceInput {
    pub subscription_external_id: String,
}

#[derive(InputObject)]
pub struct CreateAddOnInput {
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub amount_cents: i64,
    pub amount_currency: Option<String>,
}

#[derive(InputObject)]
pub struct CreateUsageAlertInput {
    pub external_subscription_id: String,
    pub code: Option<String>,
    pub billable_metric_code: Option<String>,
    pub threshold_cents: i64,
    pub recurring: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateOrganizationInput {
    pub name: Option<String>,
    pub default_currency: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    pub timezone: Option<String>,
}

#[derive(InputObject)]
pub struct CreateApiKeyInput {
    pub name: Option<String>,
}

#[derive(InputObject)]
pub struct CreateWebhookEndpointInput {
    pub webhook_url: String,
}

#[derive(InputObject)]
pub struct CreateDunningCampaignInput {
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub max_attempts: Option<i64>,
    pub days_between_attempts: Option<i64>,
    pub applied_to_organization: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateCustomerPortalCustomerInput {
    pub external_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

// ---- mutation root --------------------------------------------------------

#[derive(Default)]
pub struct Mutation;

#[Object(name = "CoreMutation")]
impl Mutation {
    async fn create_customer(&self, ctx: &Context<'_>, input: CreateCustomerInput) -> Result<Customer> {
        let org = require_org(ctx)?;
        if let Some(existing) = db(ctx).find_customer_by_external_id(org.id, &input.external_id).await? {
            return Ok(Customer(existing));
        }
        let c = m::Customer {
            id: Uuid::new_v4(),
            organization_id: org.id,
            billing_entity_id: org.id,
            external_id: input.external_id,
            name: input.name,
            email: input.email,
            country: input.country,
            currency: input.currency,
            timezone: input.timezone,
            tax_identification_number: input.tax_identification_number,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        };
        db(ctx).insert_customer(&c).await?;
        // EU customers with a VAT number are validated against VIES
        // (`customer.vies_check`).
        rago_jobs::vies::enqueue_vies_check_logged(db(ctx), c.id).await;
        emit(db(ctx), org.id, WebhookKind::CustomerCreated, customer_object(&c)).await;
        Ok(Customer(c))
    }

    async fn create_billable_metric(
        &self,
        ctx: &Context<'_>,
        input: CreateBillableMetricInput,
    ) -> Result<BillableMetric> {
        let org = require_org(ctx)?;
        let aggregation_type = aggregation_from_str(&input.aggregation_type)
            .ok_or_else(|| async_graphql::Error::new("invalid aggregation_type"))?;
        let custom_aggregator = input.custom_aggregator.filter(|c| !c.trim().is_empty());
        match (&aggregation_type, custom_aggregator.as_deref()) {
            (AggregationType::CustomAgg, None) => {
                return Err(async_graphql::Error::new("custom_aggregator is required for custom_agg"))
            }
            (_, Some(src)) => rago_billing::aggregation::validate_custom_aggregator(src)
                .map_err(|e| async_graphql::Error::new(format!("invalid custom_aggregator: {e}")))?,
            _ => {}
        }
        let metric = m::BillableMetric {
            id: Uuid::new_v4(),
            organization_id: org.id,
            name: input.name,
            code: input.code,
            description: input.description,
            aggregation_type,
            field_name: input.field_name,
            recurring: input.recurring.unwrap_or(false),
            expression: None,
            rounding_function: None,
            rounding_precision: None,
            custom_aggregator,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        };
        db(ctx).insert_billable_metric(&metric).await?;
        // `BillableMetrics::CreateService` emits `billable_metric.created`.
        emit(db(ctx), org.id, WebhookKind::BillableMetricCreated, json!({ "lago_id": metric.id, "code": metric.code, "name": metric.name })).await;
        Ok(BillableMetric(metric))
    }

    async fn create_plan(&self, ctx: &Context<'_>, input: CreatePlanInput) -> Result<Plan> {
        let org = require_org(ctx)?;
        // Validate thresholds before anything is written.
        if let Some(items) = &input.usage_thresholds {
            crate::thresholds::parse_threshold_inputs(items)?;
        }
        let interval = interval_from_str(&input.interval);
        let plan = m::Plan {
            id: Uuid::new_v4(),
            organization_id: org.id,
            name: input.name,
            code: input.code,
            interval,
            description: input.description,
            amount_cents: Some(input.amount_cents),
            amount_currency: input.amount_currency.clone(),
            trial_period: None,
            pay_in_advance: input.pay_in_advance.unwrap_or(false),
            bill_charges_monthly: None,
            parent_id: None,
            invoice_display_name: None,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        };
        db(ctx).insert_plan(&plan).await?;

        let mut charges = Vec::new();
        for cp in input.charges.unwrap_or_default() {
            let metric = db(ctx)
                .find_metric_by_code(org.id, &cp.billable_metric_code)
                .await?
                .ok_or_else(|| async_graphql::Error::new("billable_metric_not_found"))?;
            let charge_model = charge_model_from_str(&cp.charge_model)
                .ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?;
            let charge = m::Charge {
                id: Uuid::new_v4(),
                organization_id: org.id,
                billable_metric_id: Some(metric.id),
                plan_id: Some(plan.id),
                code: metric.code.clone(),
                charge_model,
                properties: cp.properties.map(|j| j.0).unwrap_or_else(|| json!({})),
                pay_in_advance: cp.pay_in_advance.unwrap_or(false),
                prorated: cp.prorated.unwrap_or(false),
                invoiceable: cp.invoiceable.unwrap_or(true),
                min_amount_cents: 0,
                amount_currency: Some(input.amount_currency.clone()),
                accepts_target_wallet: false,
                invoice_display_name: None,
                parent_id: None,
                created_at: now(),
                updated_at: now(),
                deleted_at: None,
            };
            db(ctx).insert_charge(&charge).await?;
            charges.push(charge);
        }
        if let Some(items) = input.usage_thresholds.as_deref().filter(|t| !t.is_empty()) {
            crate::thresholds::replace_plan_thresholds(ctx, org.id, plan.id, items).await?;
        }
        // `Plans::CreateService` emits `plan.created`.
        emit(db(ctx), org.id, WebhookKind::PlanCreated, json!({ "lago_id": plan.id, "code": plan.code, "name": plan.name })).await;
        Ok(Plan { plan, charges })
    }

    async fn create_subscription(
        &self,
        ctx: &Context<'_>,
        input: CreateSubscriptionInput,
    ) -> Result<Subscription> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &input.external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let plan = db(ctx)
            .find_plan_by_code(org.id, &input.plan_code)
            .await?
            .ok_or_else(|| async_graphql::Error::new("plan_not_found"))?;

        let billing_time = input
            .billing_time
            .as_deref()
            .and_then(billing_time_from_str)
            .unwrap_or(BillingTime::Calendar);
        let sub = m::Subscription {
            id: Uuid::new_v4(),
            organization_id: org.id,
            customer_id: customer.id,
            plan_id: plan.id,
            billing_entity_id: Some(org.id),
            external_id: input.external_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: input.name,
            status: SubscriptionStatus::Active,
            billing_time,
            subscription_at: Some(now()),
            started_at: Some(now()),
            ending_at: None,
            trial_ended_at: None,
            canceled_at: None,
            terminated_at: None,
            previous_subscription_id: None,
            created_at: now(),
            updated_at: now(),
        };
        db(ctx).insert_subscription(&sub).await?;
        Ok(Subscription {
            plan_code: plan.code.clone(),
            plan_amount_cents: plan.amount_cents,
            plan_amount_currency: plan.amount_currency.clone(),
            external_customer_id: input.external_customer_id,
            sub,
        })
    }

    async fn terminate_subscription(
        &self,
        ctx: &Context<'_>,
        input: TerminateSubscriptionInput,
    ) -> Result<Subscription> {
        let org = require_org(ctx)?;
        let mut sub = db(ctx)
            .find_subscription_by_external_id(org.id, &input.external_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("subscription_not_found"))?;
        let plan = db(ctx).find_plan(sub.plan_id).await?;
        // A subscription that never started is canceled; an active one is
        // terminated (`Subscriptions::TerminateService`).
        let was_pending = matches!(sub.status, SubscriptionStatus::Pending);
        let terminated_at = now();
        if matches!(sub.status, SubscriptionStatus::Active) {
            // The prorated final invoice (and the unused-days credit note of a
            // pay-in-advance fee) is issued before the subscription stops.
            let transport = rago_integrations::default_transport();
            rago_jobs::termination::bill_mid_period_termination(
                db(ctx),
                &transport,
                &sub,
                &plan,
                terminated_at.naive_utc(),
                true,
            )
            .await
            .map_err(|e| {
                tracing::error!(subscription_id = %sub.id, error = %e, "final invoice on termination failed");
                async_graphql::Error::new("final_invoice_failed")
            })?;
        }
        if was_pending {
            sub.status = SubscriptionStatus::Canceled;
        } else {
            sub.status = SubscriptionStatus::Terminated;
            sub.terminated_at = Some(terminated_at);
        }
        sub.canceled_at = Some(terminated_at);
        sub.updated_at = terminated_at;
        db(ctx).update_subscription(&sub).await?;
        let ext = db(ctx).customer_external_id(sub.customer_id).await?.unwrap_or_default();
        let kind = if was_pending { WebhookKind::SubscriptionCanceled } else { WebhookKind::SubscriptionTerminated };
        emit(
            db(ctx),
            org.id,
            kind,
            json!({ "lago_id": sub.id, "external_id": sub.external_id, "external_customer_id": ext, "plan_code": plan.code, "status": sub.status.as_str() }),
        )
        .await;
        Ok(Subscription {
            plan_code: plan.code.clone(),
            plan_amount_cents: plan.amount_cents,
            plan_amount_currency: plan.amount_currency.clone(),
            external_customer_id: ext,
            sub,
        })
    }

    async fn create_customer_wallet(&self, ctx: &Context<'_>, input: CreateWalletInput) -> Result<Wallet> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &input.external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let currency = input
            .currency
            .clone()
            .or_else(|| customer.currency.clone())
            .unwrap_or_else(|| "USD".into());
        let rate_amount = input
            .rate_amount
            .as_deref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ONE);

        let mut wallet = m::Wallet {
            id: Uuid::new_v4(),
            organization_id: org.id,
            customer_id: customer.id,
            status: WalletStatus::Active,
            name: input.name,
            rate_amount,
            credits_balance: Decimal::ZERO,
            consumed_credits: Decimal::ZERO,
            balance_cents: 0,
            balance_currency: currency.clone(),
            consumed_amount_cents: 0,
            ongoing_balance_cents: 0,
            ongoing_usage_balance_cents: 0,
            credits_ongoing_balance: Decimal::ZERO,
            credits_ongoing_usage_balance: Decimal::ZERO,
            expiration_at: None,
            terminated_at: None,
            lock_version: 0,
            priority: 50,
            allowed_fee_types: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        db(ctx).insert_wallet(&wallet).await?;
        emit(
            db(ctx),
            org.id,
            WebhookKind::WalletCreated,
            json!({
                "lago_id": wallet.id,
                "lago_customer_id": customer.id,
                "external_customer_id": customer.external_id,
                "status": wallet.status.as_str(),
                "currency": wallet.balance_currency,
                "name": wallet.name,
                "rate_amount": wallet.rate_amount.to_string(),
                "created_at": wallet.created_at,
            }),
        )
        .await;

        let granted = parse_dec(&input.granted_credits);
        let paid = parse_dec(&input.paid_credits);
        if granted < Decimal::ZERO || paid < Decimal::ZERO {
            return Err(async_graphql::Error::new("credits must be positive"));
        }
        if granted > Decimal::ZERO {
            wallet = grant_wallet_credits(db(ctx), &wallet, granted).await?.0;
        }
        if paid > Decimal::ZERO {
            // A purchase: pending until its credit invoice is paid.
            rago_jobs::wallet_credits::purchase_paid_credits(db(ctx), &wallet, paid)
                .await
                .map_err(|e| async_graphql::Error::new(format!("paid_credit_purchase_failed: {e}")))?;
        }
        Ok(Wallet {
            wallet,
            external_customer_id: input.external_customer_id,
        })
    }

    async fn create_customer_wallet_transaction(
        &self,
        ctx: &Context<'_>,
        input: CreateWalletTransactionInput,
    ) -> Result<Vec<WalletTransaction>> {
        let org = require_org(ctx)?;
        let wallet_id = Uuid::parse_str(&input.wallet_id).map_err(|_| async_graphql::Error::new("invalid wallet_id"))?;
        let wallet = db(ctx)
            .find_wallet(wallet_id)
            .await?
            .filter(|w| w.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("wallet_not_found"))?;
        let granted = parse_dec(&input.granted_credits);
        let paid = parse_dec(&input.paid_credits);
        if granted < Decimal::ZERO || paid < Decimal::ZERO {
            return Err(async_graphql::Error::new("credits must be positive"));
        }
        if granted + paid <= Decimal::ZERO {
            return Err(async_graphql::Error::new("paid_credits or granted_credits required"));
        }
        let mut out = Vec::new();
        let mut wallet = wallet;
        if granted > Decimal::ZERO {
            let (updated, tx) = grant_wallet_credits(db(ctx), &wallet, granted).await?;
            wallet = updated;
            out.push(WalletTransaction(tx));
        }
        if paid > Decimal::ZERO {
            let (tx, _invoice) = rago_jobs::wallet_credits::purchase_paid_credits(db(ctx), &wallet, paid)
                .await
                .map_err(|e| async_graphql::Error::new(format!("paid_credit_purchase_failed: {e}")))?;
            out.push(WalletTransaction(tx));
        }
        Ok(out)
    }

    async fn create_coupon(&self, ctx: &Context<'_>, input: CreateCouponInput) -> Result<Coupon> {
        let org = require_org(ctx)?;
        let coupon_type = coupon_type_from_str(&input.coupon_type).unwrap_or(CouponType::FixedAmount);
        let frequency = coupon_frequency_from_str(&input.frequency).unwrap_or(CouponFrequency::Once);
        let c = m::Coupon {
            id: Uuid::new_v4(),
            organization_id: org.id,
            name: input.name,
            code: input.code,
            status: CouponStatus::Active,
            coupon_type,
            amount_cents: input.amount_cents,
            amount_currency: input.amount_currency,
            percentage_rate: input.percentage_rate.and_then(|s| s.parse().ok()),
            frequency,
            frequency_duration: input.frequency_duration,
            expiration: CouponExpiration::NoExpiration,
            expiration_at: None,
            reusable: true,
            limited_plans: false,
            limited_billable_metrics: false,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        };
        db(ctx).insert_coupon(&c).await?;
        Ok(Coupon(c))
    }

    /// `voidInvoice(id:)` — void a finalized invoice.
    async fn void_invoice(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        // Org-scoped: another organization's invoice id answers not-found.
        db(ctx)
            .find_invoice(uuid)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        db(ctx).void_invoice(uuid).await?;
        // Real-time wallets: release reserved holds and reverse captured money.
        let transport = rago_integrations::default_transport();
        if let Err(e) = rago_jobs::wallet_realtime::settle_void(db(ctx), &transport, uuid).await {
            tracing::warn!(error = %e, invoice_id = %uuid, "real-time wallet settlement failed on void");
        }
        let invoice = db(ctx)
            .find_invoice(uuid)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let fees = db(ctx).fees_for_invoice(invoice.id).await?;
        Ok(Invoice { invoice, fees })
    }

    /// `retryInvoicePayment(id:)` — reset the invoice to pending so the payment
    /// runtime re-attempts the charge (Lago enqueues `Invoices::Payments::
    /// RetryService`; the actual charge is performed by the payments job).
    async fn retry_invoice_payment(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        db(ctx)
            .find_invoice(uuid)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        db(ctx)
            .update_invoice_payment_status(uuid, PaymentStatus::Pending.as_i32())
            .await?;
        // The worker collects it through the org's provider on its next tick.
        db(ctx)
            .enqueue_job(
                rago_jobs::JOB_COLLECT_INVOICE,
                &serde_json::json!({ "invoice_id": uuid }),
                chrono::Utc::now().naive_utc(),
                3,
            )
            .await?;
        let invoice = db(ctx)
            .find_invoice(uuid)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let fees = db(ctx).fees_for_invoice(invoice.id).await?;
        Ok(Invoice { invoice, fees })
    }

    /// `createSubscriptionInvoice` — bill a subscription's current period into an
    /// invoice (delegates to the same `generate_subscription_invoice` the REST +
    /// job paths use). Distinct from Lago's canonical one-off `createInvoice`
    /// (see `admin_mutations2::AdminMutation2::create_invoice`).
    async fn create_subscription_invoice(&self, ctx: &Context<'_>, input: CreateSubscriptionInvoiceInput) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let sub = db(ctx)
            .find_subscription_by_external_id(org.id, &input.subscription_external_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("subscription_not_found"))?;
        let plan = db(ctx).find_plan(sub.plan_id).await?;
        let currency = plan.currency();
        let charges = db(ctx).charges_for_plan(plan.id).await?;

        // The in-progress billing period for the plan's interval and billing
        // time, prorated from the subscription start (or trial end).
        let today = now().date_naive();
        let dates = rago_jobs::scheduler::span_of(&sub, &plan, today).current(today);
        let from = dates.charges_window.from.and_hms_opt(0, 0, 0).unwrap_or_default();
        let to = dates.charges_window.to.and_hms_opt(23, 59, 59).unwrap_or_default();

        let mut loaded: Vec<rago_db::LoadedCharge> = Vec::new();
        for charge in &charges {
            // Pay-in-advance charges were billed when their events were ingested.
            if charge.pay_in_advance {
                continue;
            }
            if let Some(c) = db(ctx).load_charge_usage(org.id, &sub.external_id, charge, from, to, to).await? {
                loaded.push(c);
            }
        }
        let charge_inputs: Vec<ChargeInput> = loaded.iter().map(rago_db::LoadedCharge::input).collect();

        let taxes = db(ctx).organization_taxes(org.id).await?;
        let mut invoice = blank_invoice(&sub, &currency);
        let fixed = db(ctx).fixed_charge_specs(plan.id).await?;
        let fixed_fees = rago_billing::fixed_charges::fixed_charge_fees(
            &fixed,
            plan.pay_in_advance,
            &dates,
            true,
            &sub,
            currency,
            None,
        )
        .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let (fees, _) = rago_billing::generate::generate_subscription_invoice_with_progressive(
            &mut invoice,
            &plan,
            &sub,
            &dates,
            dates.fee_proration(),
            &charge_inputs,
            &taxes,
            &[],
            &[],
            fixed_fees,
        )
        .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        db(ctx).insert_numbered_invoice_with_fees(&mut invoice, &fees).await?;
        Ok(Invoice { invoice, fees })
    }

    /// `createAddOn` — create a reusable one-off charge template.
    async fn create_add_on(&self, ctx: &Context<'_>, input: CreateAddOnInput) -> Result<AddOn> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let currency = input.amount_currency.unwrap_or_else(|| "USD".into());
        db(ctx)
            .insert_add_on(id, org.id, &input.name, &input.code, input.description.as_deref(), input.amount_cents, &currency)
            .await?;
        Ok(AddOn {
            id: id.into(),
            name: input.name,
            code: input.code,
            amount_cents: input.amount_cents,
            amount_currency: currency,
        })
    }

    /// `createUsageAlert` — attach a usage-monitoring threshold alert to a
    /// subscription.
    async fn create_usage_alert(&self, ctx: &Context<'_>, input: CreateUsageAlertInput) -> Result<UsageAlert> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let alert_type = input
            .billable_metric_code
            .as_ref()
            .map(|_| "billable_metric_current_usage_amount")
            .unwrap_or("current_usage_amount")
            .to_string();
        db(ctx)
            .insert_usage_alert(
                id,
                org.id,
                &input.external_subscription_id,
                &alert_type,
                input.billable_metric_code.as_deref(),
                input.code.as_deref(),
                input.threshold_cents,
                input.recurring.unwrap_or(false),
            )
            .await?;
        Ok(UsageAlert {
            id: id.into(),
            alert_type,
            billable_metric_code: input.billable_metric_code,
            threshold_cents: input.threshold_cents,
            recurring: input.recurring.unwrap_or(false),
            last_triggered_at: None,
        })
    }

    // ---- dashboard admin mutations ---------------------------------------

    /// `updateOrganization` — update mutable org settings.
    async fn update_organization(&self, ctx: &Context<'_>, input: UpdateOrganizationInput) -> Result<Organization> {
        let org = require_org(ctx)?;
        let updated = db(ctx)
            .update_organization(
                org.id,
                input.name.as_deref(),
                input.default_currency.as_deref(),
                input.email.as_deref(),
                input.country.as_deref(),
                input.timezone.as_deref(),
            )
            .await?;
        Ok(Organization(updated))
    }

    /// `createApiKey` — mint a new API key for the org.
    async fn create_api_key(&self, ctx: &Context<'_>, input: CreateApiKeyInput) -> Result<ApiKey> {
        let org = require_org(ctx)?;
        let key = m::ApiKey {
            id: Uuid::new_v4(),
            organization_id: org.id,
            value: format!("rago_{}", Uuid::new_v4().simple()),
            name: input.name.clone(),
            permissions: json!({}),
            expires_at: None,
            last_used_at: None,
            created_at: now(),
            updated_at: now(),
        };
        db(ctx).insert_api_key(&key).await?;
        Ok(ApiKey {
            id: key.id.into(),
            name: key.name,
            value: key.value,
            created_at: now().to_rfc3339(),
        })
    }

    /// `createWebhookEndpoint` — register an outbound webhook endpoint.
    async fn create_webhook_endpoint(&self, ctx: &Context<'_>, input: CreateWebhookEndpointInput) -> Result<WebhookEndpoint> {
        let org = require_org(ctx)?;
        let id = db(ctx).insert_webhook_endpoint_returning(org.id, &input.webhook_url).await?;
        Ok(WebhookEndpoint {
            id: id.into(),
            webhook_url: input.webhook_url,
            signature_algo: "hmac".into(),
        })
    }

    /// `terminateCoupon` — mark a coupon terminated.
    async fn terminate_coupon(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<Coupon> {
        let _org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        db(ctx).terminate_coupon(uuid).await?;
        let coupon = db(ctx).find_coupon(uuid).await?.ok_or_else(|| async_graphql::Error::new("coupon_not_found"))?;
        Ok(Coupon(coupon))
    }

    /// `terminateCustomerWallet` — terminate a wallet.
    async fn terminate_customer_wallet(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<Wallet> {
        let _org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        db(ctx).terminate_wallet(uuid).await?;
        let wallet = db(ctx).find_wallet(uuid).await?.ok_or_else(|| async_graphql::Error::new("wallet_not_found"))?;
        let ext = db(ctx).customer_external_id(wallet.customer_id).await?.unwrap_or_default();
        Ok(Wallet { wallet, external_customer_id: ext })
    }

    /// `createDunningCampaign` — configure a dunning campaign.
    async fn create_dunning_campaign(&self, ctx: &Context<'_>, input: CreateDunningCampaignInput) -> Result<DunningCampaign> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let max = input.max_attempts.unwrap_or(3);
        let days = input.days_between_attempts.unwrap_or(7);
        let applied = input.applied_to_organization.unwrap_or(false);
        db(ctx)
            .insert_dunning_campaign(id, org.id, &input.name, &input.code, input.description.as_deref(), max as i32, days as i32, &json!([]), applied)
            .await?;
        Ok(DunningCampaign {
            id: id.into(),
            name: input.name,
            code: input.code,
            max_attempts: max,
            days_between_attempts: days,
            applied_to_organization: applied,
        })
    }

    /// `updateCustomerPortalCustomer` — customer-portal self-service profile
    /// update (name / email).
    async fn update_customer_portal_customer(&self, ctx: &Context<'_>, input: UpdateCustomerPortalCustomerInput) -> Result<Customer> {
        let org = require_org(ctx)?;
        let mut customer = db(ctx)
            .find_customer_by_external_id(org.id, &input.external_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        customer.name = input.name.or(customer.name);
        customer.email = input.email.or(customer.email);
        customer.updated_at = now();
        db(ctx).update_customer(&customer).await?;
        Ok(Customer(customer))
    }
}

fn blank_invoice(sub: &m::Subscription, currency: &Currency) -> m::Invoice {
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: sub.organization_id,
        billing_entity_id: sub.billing_entity_id.unwrap_or(sub.organization_id),
        customer_id: Some(sub.customer_id),
        // Assigned from the per-organization sequence at insert time.
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(now().date_naive()),
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

fn parse_dec(s: &Option<String>) -> Decimal {
    s.as_deref().and_then(|v| v.parse::<Decimal>().ok()).unwrap_or(Decimal::ZERO)
}

fn aggregation_from_str(s: &str) -> Option<AggregationType> {
    Some(match s {
        "count_agg" => AggregationType::CountAgg,
        "sum_agg" => AggregationType::SumAgg,
        "max_agg" => AggregationType::MaxAgg,
        "unique_count_agg" => AggregationType::UniqueCountAgg,
        "latest_agg" => AggregationType::LatestAgg,
        "weighted_sum_agg" => AggregationType::WeightedSumAgg,
        "custom_agg" => AggregationType::CustomAgg,
        _ => return None,
    })
}
fn charge_model_from_str(s: &str) -> Option<ChargeModel> {
    Some(match s {
        "standard" => ChargeModel::Standard,
        "graduated" => ChargeModel::Graduated,
        "package" => ChargeModel::Package,
        "percentage" => ChargeModel::Percentage,
        "volume" => ChargeModel::Volume,
        "graduated_percentage" => ChargeModel::GraduatedPercentage,
        "custom" => ChargeModel::Custom,
        "dynamic" => ChargeModel::Dynamic,
        _ => return None,
    })
}
fn interval_from_str(s: &str) -> Option<PlanInterval> {
    Some(match s {
        "weekly" => PlanInterval::Weekly,
        "monthly" => PlanInterval::Monthly,
        "yearly" => PlanInterval::Yearly,
        "quarterly" => PlanInterval::Quarterly,
        "semiannual" => PlanInterval::Semiannual,
        _ => return None,
    })
}
fn billing_time_from_str(s: &str) -> Option<BillingTime> {
    Some(match s {
        "calendar" => BillingTime::Calendar,
        "anniversary" => BillingTime::Anniversary,
        _ => return None,
    })
}
fn coupon_type_from_str(s: &str) -> Option<CouponType> {
    Some(match s {
        "fixed_amount" => CouponType::FixedAmount,
        "percentage" => CouponType::Percentage,
        _ => return None,
    })
}
fn coupon_frequency_from_str(s: &str) -> Option<CouponFrequency> {
    Some(match s {
        "once" => CouponFrequency::Once,
        "recurring" => CouponFrequency::Recurring,
        "forever" => CouponFrequency::Forever,
        _ => return None,
    })
}


/// Grant free wallet credits: a settled inbound transaction applied to the
/// balance at once, announced with `wallet_transaction.created`.
async fn grant_wallet_credits(
    db: &rago_db::Db,
    wallet: &m::Wallet,
    credits: Decimal,
) -> Result<(m::Wallet, m::WalletTransaction)> {
    let credit = WalletCredit::from_credit_amount(
        Currency::new(&wallet.balance_currency),
        wallet.rate_amount,
        credits,
        true,
    );
    let tx = build_transaction(wallet, &credit, WalletTransactionType::Inbound, WalletTransactionStatus::Settled, None);
    db.insert_wallet_transaction(&tx).await?;
    let updated = increase_balance(wallet, &tx, false);
    db.update_wallet_balance(&updated).await?;
    emit(
        db,
        wallet.organization_id,
        WebhookKind::WalletTransactionCreated,
        rago_jobs::wallet_credits::wallet_transaction_object(&tx),
    )
    .await;
    Ok((updated, tx))
}

/// The `customer` object of `customer.created` / `customer.updated`.
pub(crate) fn customer_object(c: &m::Customer) -> serde_json::Value {
    json!({
        "lago_id": c.id,
        "external_id": c.external_id,
        "name": c.name,
        "email": c.email,
        "country": c.country,
        "currency": c.currency,
        "timezone": c.timezone,
        "tax_identification_number": c.tax_identification_number,
        "created_at": c.created_at,
        "updated_at": c.updated_at,
    })
}
