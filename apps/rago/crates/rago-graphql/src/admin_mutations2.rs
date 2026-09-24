//! GraphQL admin mutations — slice 2: the invoice tail (one-off create + batch
//! ops + regenerate/resend/payment-url/xml), usage-monitoring alerts, billing
//! entities, invoice custom sections, adjusted fees, fixed charges, and
//! subscription entitlement attach/detach. Each mirrors a Lago
//! `Mutations::<Domain>::<Action>` and delegates to `rago-db`.

use async_graphql::{Context, Object, Result, ID};
use chrono::Utc;
use rago_core::enums::{ChargeModel, InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models as m;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::admin_inputs2::*;
use crate::context::{db, require_org};
use crate::types::*;
use crate::types_admin2::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}
fn dec(s: &str) -> Result<Decimal> {
    s.parse::<Decimal>().map_err(|_| async_graphql::Error::new("invalid_decimal"))
}
fn charge_model_i32(s: &str) -> Option<i32> {
    Some(
        match s {
            "standard" => ChargeModel::Standard,
            "graduated" => ChargeModel::Graduated,
            "package" => ChargeModel::Package,
            "percentage" => ChargeModel::Percentage,
            "volume" => ChargeModel::Volume,
            "graduated_percentage" => ChargeModel::GraduatedPercentage,
            "custom" => ChargeModel::Custom,
            "dynamic" => ChargeModel::Dynamic,
            _ => return None,
        }
        .as_i32(),
    )
}

async fn load_invoice(ctx: &Context<'_>, invoice: m::Invoice) -> Result<Invoice> {
    let fees = db(ctx).fees_for_invoice(invoice.id).await?;
    Ok(Invoice { invoice, fees })
}

async fn invoice_collection(ctx: &Context<'_>, invoices: Vec<m::Invoice>) -> Result<InvoiceCollection> {
    let total = invoices.len() as i64;
    let mut collection = Vec::with_capacity(invoices.len());
    for invoice in invoices {
        collection.push(load_invoice(ctx, invoice).await?);
    }
    Ok(InvoiceCollection { collection, metadata: CollectionMetadata::build(1, 100, total) })
}

async fn alert_out(ctx: &Context<'_>, id: Uuid) -> Result<Alert> {
    let row = db(ctx).find_alert(id).await?.ok_or_else(|| async_graphql::Error::new("alert_not_found"))?;
    let thresholds = db(ctx).alert_thresholds(id).await?;
    Ok(Alert::build(row, thresholds))
}

/// Admin CRUD mutation root — slice 2 (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation2;

#[Object]
impl AdminMutation2 {
    // ---- invoice tail ----------------------------------------------------

    /// `CreateInvoice` (`Mutations::Invoices::Create`) — a one-off invoice with
    /// add-on/manual fee lines (`Invoices::CreateOneOffService`).
    async fn create_invoice(&self, ctx: &Context<'_>, input: CreateInvoiceInput) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer(uuid(&input.customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let currency = input
            .currency
            .clone()
            .or_else(|| customer.currency.clone())
            .unwrap_or_else(|| org.default_currency.clone());
        let now = Utc::now();
        let mut invoice = m::Invoice {
            id: Uuid::new_v4(),
            organization_id: org.id,
            billing_entity_id: org.id,
            customer_id: Some(customer.id),
            // Assigned from the per-organization sequence at insert time.
            number: String::new(),
            sequential_id: None,
            invoice_type: InvoiceType::OneOff,
            status: InvoiceStatus::Finalized,
            payment_status: PaymentStatus::Pending,
            currency: Some(currency.clone()),
            issuing_date: Some(now.date_naive()),
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
            created_at: now,
            updated_at: now,
            applied_taxes: Vec::new(),
        };
        db(ctx).insert_numbered_invoice(&mut invoice).await?;
        for fee in &input.fees {
            let units = match fee.units.as_deref() {
                Some(s) => dec(s)?,
                None => Decimal::ONE,
            };
            let amount_cents = (Decimal::from(fee.unit_amount_cents) * units).round().try_into().unwrap_or(0);
            let add_on_id = match &fee.add_on_id {
                Some(a) => Some(uuid(a)?),
                None => None,
            };
            db(ctx)
                .insert_one_off_fee(org.id, invoice.id, amount_cents, &currency, units, fee.unit_amount_cents, add_on_id)
                .await?;
        }
        invoice = db(ctx)
            .refresh_invoice_totals(invoice.id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `FinalizeAllInvoices` (`Mutations::Invoices::FinalizeAll`) — finalize
    /// every draft of the organization (each numbered, taxed, announced with
    /// `invoice.created` + `invoice.generated`, and queued for collection) and
    /// return exactly the invoices this call finalized.
    async fn finalize_all_invoices(&self, ctx: &Context<'_>) -> Result<InvoiceCollection> {
        let org = require_org(ctx)?;
        let invoices = rago_jobs::finalize::finalize_all_invoices(db(ctx), org.id)
            .await
            .map_err(|e| async_graphql::Error::new(format!("finalize_failed: {e}")))?;
        invoice_collection(ctx, invoices).await
    }

    /// `RetryAllInvoices` (`Mutations::Invoices::RetryAll`) — re-finalize
    /// failed-generation invoices.
    async fn retry_all_invoices(&self, ctx: &Context<'_>) -> Result<InvoiceCollection> {
        let org = require_org(ctx)?;
        db(ctx).refinalize_failed_invoices(org.id).await?;
        let invoices = db(ctx)
            .list_invoices(org.id, Some(InvoiceStatus::Finalized.as_i32()), None, None, None, 100, 0)
            .await?;
        invoice_collection(ctx, invoices).await
    }

    /// `RetryAllInvoicePayments` (`Mutations::Invoices::RetryAllPayments`) —
    /// reset failed payments on finalized invoices back to pending.
    async fn retry_all_invoice_payments(&self, ctx: &Context<'_>) -> Result<InvoiceCollection> {
        let org = require_org(ctx)?;
        // Queue a collection attempt for every finalized invoice whose payment
        // failed; the worker charges each through the org's provider.
        let failed = db(ctx)
            .list_invoices(org.id, Some(InvoiceStatus::Finalized.as_i32()), Some(PaymentStatus::Failed.as_i32()), None, None, 500, 0)
            .await?;
        for inv in &failed {
            db(ctx)
                .enqueue_job(rago_jobs::JOB_COLLECT_INVOICE, &serde_json::json!({ "invoice_id": inv.id }), chrono::Utc::now().naive_utc(), 3)
                .await?;
        }
        db(ctx).reset_failed_invoice_payments(org.id).await?;
        let invoices = db(ctx)
            .list_invoices(org.id, Some(InvoiceStatus::Finalized.as_i32()), Some(PaymentStatus::Pending.as_i32()), None, None, 100, 0)
            .await?;
        invoice_collection(ctx, invoices).await
    }

    /// `RegenerateInvoice` (`Mutations::Invoices::RegenerateFromVoided`) —
    /// re-issue a VOIDED invoice as a fresh finalized invoice carrying a copy
    /// of every fee, so the new invoice's totals are backed by its line items.
    /// Emits `invoice.created` + `invoice.generated`.
    async fn regenerate_invoice(&self, ctx: &Context<'_>, voided_invoice_id: ID) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let src = db(ctx)
            .find_invoice(uuid(&voided_invoice_id)?)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        if !matches!(src.status, InvoiceStatus::Voided) {
            return Err(async_graphql::Error::new("invoice_not_voided"));
        }
        let src_fees = db(ctx).fees_for_invoice(src.id).await?;
        if src_fees.is_empty() {
            return Err(async_graphql::Error::new("invoice_has_no_fees_to_regenerate"));
        }
        let now = Utc::now();
        let mut clone = src.clone();
        clone.id = Uuid::new_v4();
        // A re-issued invoice takes the next number in the organization sequence.
        clone.number = String::new();
        clone.sequential_id = None;
        clone.status = InvoiceStatus::Finalized;
        clone.payment_status = PaymentStatus::Pending;
        clone.issuing_date = Some(now.date_naive());
        clone.created_at = now;
        clone.updated_at = now;
        let copies: Vec<m::Fee> = src_fees
            .iter()
            .map(|fee| {
                let mut copy = fee.clone();
                copy.id = Uuid::new_v4();
                copy.invoice_id = Some(clone.id);
                copy
            })
            .collect();
        db(ctx).insert_numbered_invoice_with_fees(&mut clone, &copies).await?;
        let object = serde_json::json!({
            "lago_id": clone.id,
            "number": clone.number,
            "status": clone.status.as_str(),
            "payment_status": clone.payment_status.as_str(),
            "currency": clone.currency,
            "total_amount_cents": clone.total_amount_cents,
            "regenerated_from_invoice_id": src.id,
        });
        crate::webhook_emit::emit(db(ctx), org.id, rago_integrations::webhooks::WebhookKind::InvoiceCreated, object.clone()).await;
        let _ = rago_jobs::webhook_emit::record_invoice_generated(db(ctx), org.id, clone.id, object).await;
        load_invoice(ctx, clone).await
    }

    /// `ResendInvoiceEmail` (`Mutations::Invoices::ResendEmail`) — renders the
    /// invoice email and sends it to the customer through the API's mailer.
    /// Errors when the customer has no email address or delivery fails.
    async fn resend_invoice_email(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&id)?)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let mailer = ctx
            .data_opt::<crate::GraphMailer>()
            .ok_or_else(|| async_graphql::Error::new("mailer_not_configured"))?;
        let to = match invoice.customer_id {
            Some(cid) => db(ctx).find_customer(cid).await?.and_then(|c| c.email),
            None => None,
        }
        .filter(|e| e.contains('@'))
        .ok_or_else(|| async_graphql::Error::new("customer_email_missing"))?;
        let fees = db(ctx).fees_for_invoice(invoice.id).await?;
        let email = rago_mail::builders::invoice_finalized_email(&invoice, &fees, &to);
        mailer
            .0
            .send(&email)
            .await
            .map_err(|e| async_graphql::Error::new(format!("email_delivery_failed: {e}")))?;
        load_invoice(ctx, invoice).await
    }

    /// `DownloadXmlInvoice` (`Mutations::Invoices::DownloadXml`) — returns the
    /// invoice; its `xmlUrl` serves the EN 16931 XML rendered on request.
    async fn download_xml_invoice(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        load_invoice(ctx, invoice).await
    }

    /// `GeneratePaymentUrl` (`Mutations::Invoices::GeneratePaymentUrl`) — a
    /// one-time Checkout link minted by the org's configured payment provider.
    /// Configuration problems surface as a GraphQL error naming what is missing.
    async fn generate_payment_url(&self, ctx: &Context<'_>, invoice_id: ID) -> Result<PaymentUrl> {
        let org = require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&invoice_id)?)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let transport = rago_integrations::default_transport();
        match rago_jobs::collection::invoice_payment_url(db(ctx), &transport, &invoice).await {
            Ok(Ok(url)) => Ok(PaymentUrl { payment_url: Some(url) }),
            Ok(Err(code)) => Err(async_graphql::Error::new(code)),
            Err(e) => Err(async_graphql::Error::new(e.to_string())),
        }
    }

    // ---- usage-monitoring alerts ----------------------------------------

    /// `CreateSubscriptionAlert` (`Mutations::Subscriptions::Alerts::Create`).
    async fn create_subscription_alert(&self, ctx: &Context<'_>, input: CreateSubscriptionAlertInput) -> Result<Alert> {
        let org = require_org(ctx)?;
        let sub = uuid(&input.subscription_id)?;
        let bm = match &input.billable_metric_id {
            Some(b) => Some(uuid(b)?),
            None => None,
        };
        let id = Uuid::new_v4();
        db(ctx).insert_alert(id, org.id, "Subscription", sub, &input.alert_type, input.name.as_deref(), &input.code, bm).await?;
        for t in &input.thresholds {
            db(ctx).insert_alert_threshold(Uuid::new_v4(), org.id, id, t.code.as_deref(), dec(&t.value)?, t.recurring.unwrap_or(false)).await?;
        }
        alert_out(ctx, id).await
    }

    /// `CreateCustomerWalletAlert` (`Mutations::Wallets::Alerts::Create`).
    async fn create_customer_wallet_alert(&self, ctx: &Context<'_>, input: CreateWalletAlertInput) -> Result<Alert> {
        let org = require_org(ctx)?;
        let wallet = uuid(&input.wallet_id)?;
        let bm = match &input.billable_metric_id {
            Some(b) => Some(uuid(b)?),
            None => None,
        };
        let id = Uuid::new_v4();
        db(ctx).insert_alert(id, org.id, "Wallet", wallet, &input.alert_type, input.name.as_deref(), &input.code, bm).await?;
        for t in &input.thresholds {
            db(ctx).insert_alert_threshold(Uuid::new_v4(), org.id, id, t.code.as_deref(), dec(&t.value)?, t.recurring.unwrap_or(false)).await?;
        }
        alert_out(ctx, id).await
    }

    /// `UpdateSubscriptionAlert` (`Mutations::Subscriptions::Alerts::Update`).
    async fn update_subscription_alert(&self, ctx: &Context<'_>, input: UpdateAlertInput) -> Result<Alert> {
        self.update_alert(ctx, input).await
    }

    /// `UpdateCustomerWalletAlert` (`Mutations::Wallets::Alerts::Update`).
    async fn update_customer_wallet_alert(&self, ctx: &Context<'_>, input: UpdateAlertInput) -> Result<Alert> {
        self.update_alert(ctx, input).await
    }

    /// `DestroySubscriptionAlert` (`Mutations::Subscriptions::Alerts::Destroy`).
    async fn destroy_subscription_alert(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_alert(uid).await?;
        Ok(uid.into())
    }

    /// `DestroyCustomerWalletAlert` (`Mutations::Wallets::Alerts::Destroy`).
    async fn destroy_customer_wallet_alert(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_alert(uid).await?;
        Ok(uid.into())
    }

    // ---- billing entities -----------------------------------------------

    /// `CreateBillingEntity` (`Mutations::BillingEntities::Create`).
    async fn create_billing_entity(&self, ctx: &Context<'_>, input: CreateBillingEntityInput) -> Result<BillingEntity> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let currency = input.default_currency.as_deref().unwrap_or("USD");
        let timezone = input.timezone.as_deref().unwrap_or("UTC");
        db(ctx)
            .insert_billing_entity(id, org.id, &input.code, &input.name, input.email.as_deref(), input.country.as_deref(), currency, timezone)
            .await?;
        let row = db(ctx).find_billing_entity(id).await?.ok_or_else(|| async_graphql::Error::new("billing_entity_not_found"))?;
        Ok(row.into())
    }

    /// `UpdateBillingEntity` (`Mutations::BillingEntities::Update`).
    async fn update_billing_entity(&self, ctx: &Context<'_>, input: UpdateBillingEntityInput) -> Result<BillingEntity> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_billing_entity(uuid(&input.id)?, input.name.as_deref(), input.email.as_deref(), input.country.as_deref(), input.default_currency.as_deref(), input.timezone.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("billing_entity_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyBillingEntity` (`Mutations::BillingEntities::Destroy`).
    async fn destroy_billing_entity(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_billing_entity(uid).await?;
        Ok(uid.into())
    }

    // ---- invoice custom sections ----------------------------------------

    /// `CreateInvoiceCustomSection` (`Mutations::InvoiceCustomSections::Create`).
    async fn create_invoice_custom_section(&self, ctx: &Context<'_>, input: CreateInvoiceCustomSectionInput) -> Result<InvoiceCustomSection> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        db(ctx)
            .insert_invoice_custom_section(id, org.id, &input.code, &input.name, input.description.as_deref(), input.details.as_deref(), input.display_name.as_deref())
            .await?;
        let row = db(ctx).find_invoice_custom_section(id).await?.ok_or_else(|| async_graphql::Error::new("invoice_custom_section_not_found"))?;
        Ok(row.into())
    }

    /// `UpdateInvoiceCustomSection` (`Mutations::InvoiceCustomSections::Update`).
    async fn update_invoice_custom_section(&self, ctx: &Context<'_>, input: UpdateInvoiceCustomSectionInput) -> Result<InvoiceCustomSection> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_invoice_custom_section(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), input.details.as_deref(), input.display_name.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("invoice_custom_section_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyInvoiceCustomSection` (`Mutations::InvoiceCustomSections::Destroy`).
    async fn destroy_invoice_custom_section(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_invoice_custom_section(uid).await?;
        Ok(uid.into())
    }

    // ---- adjusted fees ---------------------------------------------------

    /// `CreateAdjustedFee` (`Mutations::AdjustedFees::Create`) — override a
    /// draft-invoice fee's units/amount and record the adjustment.
    async fn create_adjusted_fee(&self, ctx: &Context<'_>, input: CreateAdjustedFeeInput) -> Result<Fee> {
        let org = require_org(ctx)?;
        let fee = db(ctx)
            .find_fee_opt(uuid(&input.fee_id)?)
            .await?
            .filter(|f| f.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("fee_not_found"))?;
        let units = match input.units.as_deref() {
            Some(s) => dec(s)?,
            None => fee.units,
        };
        let unit_amount_cents = input.unit_amount_cents.unwrap_or(fee.unit_amount_cents);
        let amount_cents = (Decimal::from(unit_amount_cents) * units).round().try_into().unwrap_or(0);
        // Capture the pre-adjustment values so a later destroy can revert.
        let (orig_units, orig_amount, orig_unit_amount) = (fee.units, fee.amount_cents, fee.unit_amount_cents);
        db(ctx).update_fee_amount(fee.id, units, amount_cents, unit_amount_cents).await?;
        let adjusted_fee_id = Uuid::new_v4();
        db(ctx)
            .insert_adjusted_fee(
                adjusted_fee_id,
                org.id,
                Some(fee.id),
                fee.invoice_id.ok_or_else(|| async_graphql::Error::new("fee_has_no_invoice"))?,
                input.units.is_some(),
                input.unit_amount_cents.is_some(),
                units,
                unit_amount_cents,
                input.invoice_display_name.as_deref(),
            )
            .await?;
        db(ctx).set_adjusted_fee_original(adjusted_fee_id, orig_units, orig_amount, orig_unit_amount).await?;
        let updated = db(ctx).find_fee_opt(fee.id).await?.ok_or_else(|| async_graphql::Error::new("fee_not_found"))?;
        Ok(Fee(updated))
    }

    /// `PreviewAdjustedFee` (`Mutations::AdjustedFees::Preview`) — compute the
    /// adjusted fee in memory without persisting.
    async fn preview_adjusted_fee(&self, ctx: &Context<'_>, input: CreateAdjustedFeeInput) -> Result<Fee> {
        let org = require_org(ctx)?;
        let mut fee = db(ctx)
            .find_fee_opt(uuid(&input.fee_id)?)
            .await?
            .filter(|f| f.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("fee_not_found"))?;
        let units = match input.units.as_deref() {
            Some(s) => dec(s)?,
            None => fee.units,
        };
        let unit_amount_cents = input.unit_amount_cents.unwrap_or(fee.unit_amount_cents);
        fee.units = units;
        fee.unit_amount_cents = unit_amount_cents;
        fee.amount_cents = (Decimal::from(unit_amount_cents) * units).round().try_into().unwrap_or(0);
        Ok(Fee(fee))
    }

    /// `DestroyAdjustedFee` (`Mutations::AdjustedFees::Destroy`) — remove the
    /// adjustment record and revert the fee to its captured pre-adjustment
    /// units/amount.
    async fn destroy_adjusted_fee(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        if let Some((Some(fee_id), Some(units), Some(amount), Some(unit_amount))) = db(ctx).adjusted_fee_original(uid).await? {
            db(ctx).update_fee_amount(fee_id, units, amount, unit_amount).await?;
        }
        db(ctx).delete_adjusted_fee(uid).await?;
        Ok(uid.into())
    }

    // ---- fixed charges ---------------------------------------------------

    /// `CreateFixedCharge` (`Mutations::FixedCharges::Create`).
    async fn create_fixed_charge(&self, ctx: &Context<'_>, input: CreateFixedChargeInput) -> Result<FixedCharge> {
        let org = require_org(ctx)?;
        let charge_model = charge_model_i32(&input.charge_model).ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?;
        let add_on_id = match &input.add_on_id {
            Some(a) => Some(uuid(a)?),
            None => None,
        };
        let units = match input.units.as_deref() {
            Some(s) => dec(s)?,
            None => Decimal::ZERO,
        };
        let id = Uuid::new_v4();
        db(ctx)
            .insert_fixed_charge(
                id,
                org.id,
                uuid(&input.plan_id)?,
                add_on_id,
                charge_model,
                &input.properties.map(|j| j.0).unwrap_or_else(|| serde_json::json!({})),
                units,
                input.pay_in_advance.unwrap_or(false),
                input.prorated.unwrap_or(false),
                input.invoice_display_name.as_deref(),
            )
            .await?;
        let row = db(ctx).find_fixed_charge(id).await?.ok_or_else(|| async_graphql::Error::new("fixed_charge_not_found"))?;
        Ok(row.into())
    }

    /// `UpdateFixedCharge` (`Mutations::FixedCharges::Update`).
    async fn update_fixed_charge(&self, ctx: &Context<'_>, input: UpdateFixedChargeInput) -> Result<FixedCharge> {
        require_org(ctx)?;
        let charge_model = match input.charge_model.as_deref() {
            Some(s) => Some(charge_model_i32(s).ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?),
            None => None,
        };
        let units = match input.units.as_deref() {
            Some(s) => Some(dec(s)?),
            None => None,
        };
        let props = input.properties.map(|j| j.0);
        let row = db(ctx)
            .update_fixed_charge(uuid(&input.id)?, charge_model, props.as_ref(), units, input.pay_in_advance, input.prorated, input.invoice_display_name.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("fixed_charge_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyFixedCharge` (`Mutations::FixedCharges::Destroy`).
    async fn destroy_fixed_charge(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_fixed_charge(uid).await?;
        Ok(uid.into())
    }

    // ---- subscription entitlements --------------------------------------

    /// Attach a feature entitlement to a subscription (Rago admin convenience;
    /// Lago manages these inside `UpdateSubscription`). Returns the new
    /// entitlement id.
    async fn create_subscription_entitlement(&self, ctx: &Context<'_>, subscription_id: ID, feature_code: String) -> Result<ID> {
        let org = require_org(ctx)?;
        let sub = uuid(&subscription_id)?;
        let feature_id = db(ctx)
            .find_feature_by_code(org.id, &feature_code)
            .await?
            .ok_or_else(|| async_graphql::Error::new("feature_not_found"))?;
        if let Some(existing) = db(ctx).subscription_entitlement_id(sub, feature_id).await? {
            return Ok(existing.into());
        }
        let id = Uuid::new_v4();
        db(ctx).insert_subscription_entitlement(id, org.id, feature_id, sub).await?;
        Ok(id.into())
    }

    /// Detach a feature entitlement from a subscription.
    async fn destroy_subscription_entitlement(&self, ctx: &Context<'_>, subscription_id: ID, feature_code: String) -> Result<ID> {
        let org = require_org(ctx)?;
        let sub = uuid(&subscription_id)?;
        let feature_id = db(ctx)
            .find_feature_by_code(org.id, &feature_code)
            .await?
            .ok_or_else(|| async_graphql::Error::new("feature_not_found"))?;
        let ent = db(ctx)
            .subscription_entitlement_id(sub, feature_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("entitlement_not_found"))?;
        db(ctx).soft_delete_entitlement(ent).await?;
        Ok(ent.into())
    }
}

impl AdminMutation2 {
    async fn update_alert(&self, ctx: &Context<'_>, input: UpdateAlertInput) -> Result<Alert> {
        let org = require_org(ctx)?;
        let id = uuid(&input.id)?;
        db(ctx).update_alert(id, input.name.as_deref(), input.code.as_deref()).await?;
        if let Some(thresholds) = input.thresholds {
            let mut rows = Vec::with_capacity(thresholds.len());
            for t in thresholds {
                rows.push((t.code.clone(), dec(&t.value)?, t.recurring.unwrap_or(false)));
            }
            db(ctx).replace_alert_thresholds(org.id, id, &rows).await?;
        }
        alert_out(ctx, id).await
    }
}
