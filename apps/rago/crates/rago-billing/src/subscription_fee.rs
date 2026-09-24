//! `Fees::SubscriptionService` — the recurring flat plan fee, with proration
//! on the first period, on termination, and on upgrade.
//!
//! The full-period amount is simply `plan.amount_cents`. A prorated period bills
//! `days_to_bill * single_day_price`, where
//! `single_day_price = plan.amount_cents / period_duration_days`.

use chrono::Utc;
use rago_core::enums::{FeeType, PaymentStatus};
use rago_core::models::{Fee, Plan, Subscription};
use rago_core::money::round_to_integer;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::proration::FeeDates;

/// How to prorate the subscription fee.
#[derive(Debug, Clone, Copy)]
pub enum Proration {
    /// Bill the full plan amount.
    Full,
    /// Bill `days_to_bill` days at the single-day price (first period,
    /// termination, upgrade).
    Prorated { days_to_bill: i64 },
    /// Bill nothing — the free trial covers the whole period
    /// (`Fees::SubscriptionService` returns `0` when
    /// `trial_end_datetime >= to_datetime`).
    Zero,
}

/// Compute the precise subscription fee amount in cents.
pub fn compute_precise_amount_cents(
    plan_amount_cents: i64,
    dates: &dyn FeeDates,
    proration: Proration,
) -> Decimal {
    match proration {
        Proration::Full => Decimal::from(plan_amount_cents),
        Proration::Prorated { days_to_bill } => {
            Decimal::from(days_to_bill) * dates.single_day_price(plan_amount_cents)
        }
        Proration::Zero => Decimal::ZERO,
    }
}

/// `Fees::SubscriptionService#call` — build the subscription fee.
pub fn build_subscription_fee(
    plan: &Plan,
    subscription: &Subscription,
    invoice_id: Option<Uuid>,
    billing_entity_id: Uuid,
    dates: &dyn FeeDates,
    proration: Proration,
) -> Fee {
    let plan_amount_cents = plan.amount_cents.unwrap_or(0);
    let precise_amount_cents = compute_precise_amount_cents(plan_amount_cents, dates, proration);
    let amount_cents = round_to_integer(precise_amount_cents).to_i64().unwrap_or(0);

    Fee {
        id: Uuid::new_v4(),
        organization_id: subscription.organization_id,
        billing_entity_id,
        invoice_id,
        subscription_id: Some(subscription.id),
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(FeeType::Subscription),
        invoiceable_type: Some("Subscription".to_string()),
        invoiceable_id: Some(subscription.id),
        amount_cents,
        amount_currency: plan.amount_currency.clone(),
        precise_amount_cents,
        unit_amount_cents: amount_cents,
        precise_unit_amount: Decimal::from(amount_cents)
            / plan.currency().subunit_to_unit_decimal(),
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: Decimal::ZERO,
        taxes_rate: 0.0,
        units: Decimal::ONE,
        total_aggregated_units: Some(Decimal::ONE),
        events_count: None,
        precise_coupons_amount_cents: Decimal::ZERO,
        precise_credit_notes_amount_cents: Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({ "plan_amount_cents": plan_amount_cents }),
        grouped_by: json!({}),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        applied_taxes: Vec::new(),
    }
}
