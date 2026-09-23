//! Row → domain model mapping. Lago stores timestamps as `timestamp without
//! time zone` (UTC by convention), so we read them as `NaiveDateTime` and wrap
//! them in `Utc`. ActiveRecord integer enums are converted via `TryFrom`.

use chrono::{DateTime, NaiveDateTime, Utc};
use rago_core::enums::*;
use rago_core::models::*;
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::error::{DbError, Result};

fn utc(ndt: NaiveDateTime) -> DateTime<Utc> {
    ndt.and_utc()
}
fn opt_utc(ndt: Option<NaiveDateTime>) -> Option<DateTime<Utc>> {
    ndt.map(utc)
}

pub fn charge(row: &PgRow) -> Result<Charge> {
    Ok(Charge {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        billable_metric_id: row.try_get("billable_metric_id")?,
        plan_id: row.try_get("plan_id")?,
        code: row.try_get("code")?,
        charge_model: ChargeModel::try_from(row.try_get::<i32, _>("charge_model")?)
            .map_err(DbError::enum_err)?,
        properties: row.try_get("properties")?,
        pay_in_advance: row.try_get("pay_in_advance")?,
        prorated: row.try_get("prorated")?,
        invoiceable: row.try_get("invoiceable")?,
        min_amount_cents: row.try_get("min_amount_cents")?,
        amount_currency: row.try_get("amount_currency")?,
        accepts_target_wallet: row.try_get("accepts_target_wallet")?,
        invoice_display_name: row.try_get("invoice_display_name")?,
        parent_id: row.try_get("parent_id")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
    })
}

pub fn billable_metric(row: &PgRow) -> Result<BillableMetric> {
    Ok(BillableMetric {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        name: row.try_get("name")?,
        code: row.try_get("code")?,
        description: row.try_get("description")?,
        aggregation_type: AggregationType::try_from(row.try_get::<i32, _>("aggregation_type")?)
            .map_err(DbError::enum_err)?,
        field_name: row.try_get("field_name")?,
        recurring: row.try_get("recurring")?,
        expression: row.try_get("expression")?,
        rounding_function: row.try_get("rounding_function")?,
        rounding_precision: row.try_get("rounding_precision")?,
        custom_aggregator: row.try_get("custom_aggregator").ok().flatten(),
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
    })
}

pub fn plan(row: &PgRow) -> Result<Plan> {
    let interval = match row.try_get::<Option<i32>, _>("interval")? {
        Some(v) => Some(PlanInterval::try_from(v).map_err(DbError::enum_err)?),
        None => None,
    };
    Ok(Plan {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        name: row.try_get("name")?,
        code: row.try_get("code")?,
        interval,
        description: row.try_get("description")?,
        amount_cents: row.try_get("amount_cents")?,
        amount_currency: row.try_get("amount_currency")?,
        trial_period: row.try_get("trial_period")?,
        pay_in_advance: row
            .try_get::<Option<bool>, _>("pay_in_advance")?
            .unwrap_or(false),
        bill_charges_monthly: row.try_get("bill_charges_monthly")?,
        parent_id: row.try_get("parent_id")?,
        invoice_display_name: row.try_get("invoice_display_name")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
    })
}

pub fn subscription(row: &PgRow) -> Result<Subscription> {
    Ok(Subscription {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        customer_id: row.try_get("customer_id")?,
        plan_id: row.try_get("plan_id")?,
        billing_entity_id: row.try_get("billing_entity_id")?,
        external_id: row.try_get("external_id")?,
        name: row.try_get("name")?,
        status: SubscriptionStatus::try_from(row.try_get::<i32, _>("status")?)
            .map_err(DbError::enum_err)?,
        billing_time: BillingTime::try_from(row.try_get::<i32, _>("billing_time")?)
            .map_err(DbError::enum_err)?,
        subscription_at: opt_utc(row.try_get("subscription_at")?),
        started_at: opt_utc(row.try_get("started_at")?),
        ending_at: opt_utc(row.try_get("ending_at")?),
        trial_ended_at: opt_utc(row.try_get("trial_ended_at")?),
        canceled_at: opt_utc(row.try_get("canceled_at")?),
        terminated_at: opt_utc(row.try_get("terminated_at")?),
        previous_subscription_id: row.try_get("previous_subscription_id")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn tax(row: &PgRow) -> Result<Tax> {
    Ok(Tax {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        name: row.try_get("name")?,
        code: row.try_get("code")?,
        description: row.try_get("description")?,
        rate: row.try_get("rate")?,
        applied_to_organization: row.try_get("applied_to_organization")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn event(row: &PgRow) -> Result<Event> {
    Ok(Event {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        customer_id: row.try_get("customer_id")?,
        subscription_id: row.try_get("subscription_id")?,
        transaction_id: row.try_get("transaction_id")?,
        code: row.try_get("code")?,
        properties: row.try_get("properties")?,
        external_customer_id: row.try_get("external_customer_id")?,
        external_subscription_id: row.try_get("external_subscription_id")?,
        timestamp: row.try_get("timestamp")?,
        precise_total_amount_cents: row.try_get("precise_total_amount_cents")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
        persisted: true,
    })
}

pub fn organization(row: &PgRow) -> Result<Organization> {
    Ok(Organization {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        slug: row.try_get("slug")?,
        default_currency: row.try_get("default_currency")?,
        email: row.try_get("email")?,
        country: row.try_get("country")?,
        tax_identification_number: row.try_get("tax_identification_number")?,
        timezone: row.try_get("timezone")?,
        document_locale: row.try_get("document_locale")?,
        finalize_zero_amount_invoice: row.try_get("finalize_zero_amount_invoice")?,
        hmac_key: row.try_get("hmac_key").ok().flatten(),
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn api_key(row: &PgRow) -> Result<ApiKey> {
    Ok(ApiKey {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        value: row.try_get("value")?,
        name: row.try_get("name")?,
        permissions: row.try_get("permissions")?,
        expires_at: opt_utc(row.try_get("expires_at")?),
        last_used_at: opt_utc(row.try_get("last_used_at")?),
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn customer(row: &PgRow) -> Result<Customer> {
    Ok(Customer {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        billing_entity_id: row.try_get("billing_entity_id")?,
        external_id: row.try_get("external_id")?,
        name: row.try_get("name")?,
        email: row.try_get("email")?,
        country: row.try_get("country")?,
        currency: row.try_get("currency")?,
        timezone: row.try_get("timezone")?,
        tax_identification_number: row.try_get("tax_identification_number")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
    })
}

pub fn wallet(row: &PgRow) -> Result<Wallet> {
    Ok(Wallet {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        customer_id: row.try_get("customer_id")?,
        status: WalletStatus::try_from(row.try_get::<i32, _>("status")?)
            .map_err(DbError::enum_err)?,
        name: row.try_get("name")?,
        rate_amount: row.try_get("rate_amount")?,
        credits_balance: row.try_get("credits_balance")?,
        consumed_credits: row.try_get("consumed_credits")?,
        balance_cents: row.try_get("balance_cents")?,
        balance_currency: row.try_get("balance_currency")?,
        consumed_amount_cents: row.try_get("consumed_amount_cents")?,
        ongoing_balance_cents: row.try_get("ongoing_balance_cents")?,
        ongoing_usage_balance_cents: row.try_get("ongoing_usage_balance_cents")?,
        credits_ongoing_balance: row.try_get("credits_ongoing_balance")?,
        credits_ongoing_usage_balance: row.try_get("credits_ongoing_usage_balance")?,
        expiration_at: opt_utc(row.try_get("expiration_at")?),
        terminated_at: opt_utc(row.try_get("terminated_at")?),
        lock_version: row.try_get("lock_version")?,
        priority: row.try_get("priority")?,
        allowed_fee_types: {
            // Stored as integer[]; an empty array (the column default) means
            // the wallet is unrestricted. Unknown values are silently dropped.
            let raw: Vec<i32> = row.try_get("allowed_fee_types").unwrap_or_default();
            raw.into_iter()
                .filter_map(|v| FeeType::try_from(v).ok())
                .collect()
        },
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn wallet_transaction(row: &PgRow) -> Result<WalletTransaction> {
    Ok(WalletTransaction {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        wallet_id: row.try_get("wallet_id")?,
        transaction_type: WalletTransactionType::try_from(
            row.try_get::<i32, _>("transaction_type")?,
        )
        .map_err(DbError::enum_err)?,
        status: WalletTransactionStatus::try_from(row.try_get::<i32, _>("status")?)
            .map_err(DbError::enum_err)?,
        amount: row.try_get("amount")?,
        credit_amount: row.try_get("credit_amount")?,
        invoice_id: row.try_get("invoice_id")?,
        settled_at: opt_utc(row.try_get("settled_at")?),
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
    })
}

pub fn invoice(row: &PgRow) -> Result<Invoice> {
    Ok(Invoice {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        billing_entity_id: row.try_get("billing_entity_id")?,
        customer_id: row.try_get("customer_id")?,
        number: row.try_get("number")?,
        sequential_id: row.try_get("sequential_id")?,
        invoice_type: InvoiceType::try_from(row.try_get::<i32, _>("invoice_type")?)
            .map_err(DbError::enum_err)?,
        status: InvoiceStatus::try_from(row.try_get::<i32, _>("status")?)
            .map_err(DbError::enum_err)?,
        payment_status: PaymentStatus::try_from(row.try_get::<i32, _>("payment_status")?)
            .map_err(DbError::enum_err)?,
        currency: row.try_get("currency")?,
        issuing_date: row.try_get("issuing_date")?,
        fees_amount_cents: row.try_get("fees_amount_cents")?,
        coupons_amount_cents: row.try_get("coupons_amount_cents")?,
        credit_notes_amount_cents: row.try_get("credit_notes_amount_cents")?,
        prepaid_credit_amount_cents: row.try_get("prepaid_credit_amount_cents")?,
        progressive_billing_credit_amount_cents: row
            .try_get("progressive_billing_credit_amount_cents")?,
        sub_total_excluding_taxes_amount_cents: row
            .try_get("sub_total_excluding_taxes_amount_cents")?,
        sub_total_including_taxes_amount_cents: row
            .try_get("sub_total_including_taxes_amount_cents")?,
        taxes_amount_cents: row.try_get("taxes_amount_cents")?,
        taxes_rate: row.try_get("taxes_rate")?,
        total_amount_cents: row.try_get("total_amount_cents")?,
        version_number: row.try_get("version_number")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        applied_taxes: Vec::new(),
    })
}

pub fn fee(row: &PgRow) -> Result<Fee> {
    Ok(Fee {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        billing_entity_id: row.try_get("billing_entity_id")?,
        invoice_id: row.try_get("invoice_id")?,
        subscription_id: row.try_get("subscription_id")?,
        charge_id: row.try_get("charge_id")?,
        charge_filter_id: row.try_get("charge_filter_id")?,
        fee_type: match row.try_get::<Option<i32>, _>("fee_type")? {
            Some(v) => Some(FeeType::try_from(v).map_err(DbError::enum_err)?),
            None => None,
        },
        invoiceable_type: row.try_get("invoiceable_type")?,
        invoiceable_id: row.try_get("invoiceable_id")?,
        amount_cents: row.try_get("amount_cents")?,
        amount_currency: row.try_get("amount_currency")?,
        precise_amount_cents: row.try_get("precise_amount_cents")?,
        unit_amount_cents: row.try_get("unit_amount_cents")?,
        precise_unit_amount: row.try_get("precise_unit_amount")?,
        taxes_amount_cents: row.try_get("taxes_amount_cents")?,
        taxes_precise_amount_cents: row.try_get("taxes_precise_amount_cents")?,
        taxes_rate: row.try_get("taxes_rate")?,
        units: row.try_get("units")?,
        total_aggregated_units: row.try_get("total_aggregated_units")?,
        events_count: row.try_get("events_count")?,
        precise_coupons_amount_cents: row.try_get("precise_coupons_amount_cents")?,
        precise_credit_notes_amount_cents: row.try_get("precise_credit_notes_amount_cents")?,
        payment_status: PaymentStatus::try_from(row.try_get::<i32, _>("payment_status")?)
            .map_err(DbError::enum_err)?,
        properties: row.try_get("properties")?,
        amount_details: row.try_get("amount_details")?,
        grouped_by: row.try_get("grouped_by")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        applied_taxes: Vec::new(),
    })
}

pub fn coupon(row: &PgRow) -> Result<Coupon> {
    Ok(Coupon {
        id: row.try_get("id")?,
        organization_id: row.try_get("organization_id")?,
        name: row.try_get("name")?,
        code: row.try_get("code")?,
        status: CouponStatus::try_from(row.try_get::<i32, _>("status")?)
            .map_err(DbError::enum_err)?,
        coupon_type: CouponType::try_from(row.try_get::<i32, _>("coupon_type")?)
            .map_err(DbError::enum_err)?,
        amount_cents: row.try_get("amount_cents")?,
        amount_currency: row.try_get("amount_currency")?,
        percentage_rate: row.try_get("percentage_rate")?,
        frequency: CouponFrequency::try_from(row.try_get::<i32, _>("frequency")?)
            .map_err(DbError::enum_err)?,
        frequency_duration: row.try_get("frequency_duration")?,
        expiration: CouponExpiration::try_from(row.try_get::<i32, _>("expiration")?)
            .map_err(DbError::enum_err)?,
        expiration_at: opt_utc(row.try_get("expiration_at")?),
        reusable: row.try_get("reusable")?,
        limited_plans: row.try_get("limited_plans")?,
        limited_billable_metrics: row.try_get("limited_billable_metrics")?,
        created_at: utc(row.try_get("created_at")?),
        updated_at: utc(row.try_get("updated_at")?),
        deleted_at: opt_utc(row.try_get("deleted_at")?),
    })
}
