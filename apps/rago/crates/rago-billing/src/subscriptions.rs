//! Subscription lifecycle — termination and upgrade with proration.
//!
//! Ports `Fees::SubscriptionService`'s `terminated_amount` / `upgraded_amount`
//! (billing the used vs remaining days of a period) and the shape of
//! `Subscriptions::{Terminate,PlanUpgrade}Service`:
//!
//! * **Terminate** (pay-in-arrears): bill `days(period_start → termination)`
//!   days of the plan, plus usage fees for that window.
//! * **Upgrade**: terminate the old plan, credit the unused days of the old
//!   plan, and bill the new plan for `days(upgrade → period_end)`.
//!
//! `date_diff` is Lago's `Utils::Datetime.date_diff_with_timezone`: the
//! inclusive day count `(to_date − from_date) + 1`.

use chrono::{NaiveDate, Utc};
use rago_core::enums::SubscriptionStatus;
use rago_core::models::{BillableMetric, Charge, Event, Fee, Invoice, Plan, Subscription, Tax};
use rust_decimal::prelude::ToPrimitive;

use crate::dates::MonthlyDates;
use crate::error::Result;
use crate::generate::{generate_subscription_invoice, ChargeInput};
use crate::invoices::CouponApplication;
use crate::subscription_fee::Proration;

/// `Utils::Datetime.date_diff_with_timezone` at day granularity — the inclusive
/// number of days from `from` to `to` (both ends counted).
pub fn date_diff(from: NaiveDate, to: NaiveDate) -> i64 {
    (to - from).num_days() + 1
}

/// Result of terminating a subscription.
pub struct TerminationResult {
    pub invoice: Invoice,
    pub fees: Vec<Fee>,
    pub subscription: Subscription,
    pub days_billed: i64,
}

/// `Subscriptions::TerminateService` + `Fees::SubscriptionService#terminated_amount`.
///
/// Bills the used days of the current period (subscription fee prorated to
/// `days(period_start → termination)`), plus usage charge fees, and marks the
/// subscription terminated.
#[allow(clippy::too_many_arguments)]
pub fn terminate_subscription(
    mut invoice: Invoice,
    plan: &Plan,
    subscription: &Subscription,
    dates: &MonthlyDates,
    termination_date: NaiveDate,
    charges: &[ChargeInput],
    taxes: &[Tax],
) -> Result<TerminationResult> {
    let period = dates.boundaries();
    let days_billed = date_diff(period.from_date, termination_date);

    let fees = generate_subscription_invoice(
        &mut invoice,
        plan,
        subscription,
        dates,
        Proration::Prorated {
            days_to_bill: days_billed,
        },
        charges,
        taxes,
        &[],
    )?;

    let mut terminated = subscription.clone();
    terminated.status = SubscriptionStatus::Terminated;
    let now = Utc::now();
    terminated.terminated_at = Some(now);
    terminated.canceled_at = Some(now);
    terminated.updated_at = now;

    Ok(TerminationResult {
        invoice,
        fees,
        subscription: terminated,
        days_billed,
    })
}

/// Result of an upgrade.
pub struct UpgradeResult {
    /// The new plan's (prorated) invoice.
    pub invoice: Invoice,
    pub fees: Vec<Fee>,
    pub old_subscription: Subscription,
    pub new_subscription: Subscription,
    /// Credit (cents) for the unused days of the already-billed old plan.
    pub old_plan_credit_cents: i64,
    pub new_plan_days_billed: i64,
}

/// `Subscriptions::PlanUpgradeService` + `Fees::SubscriptionService#upgraded_amount`.
///
/// Terminates the old subscription, credits the unused days of the old plan
/// (`days(upgrade → period_end) × old_single_day_price`, the amount Lago issues
/// as an automatic credit note), and bills the new plan for the remaining days
/// of the period (`days(upgrade → period_end) × new_single_day_price`).
#[allow(clippy::too_many_arguments)]
pub fn upgrade_subscription(
    mut new_invoice: Invoice,
    old_plan: &Plan,
    new_plan: &Plan,
    old_subscription: &Subscription,
    dates: &MonthlyDates,
    upgrade_date: NaiveDate,
    charges: &[ChargeInput],
    taxes: &[Tax],
    coupons: &[CouponApplication],
) -> Result<UpgradeResult> {
    let period = dates.boundaries();
    // Days from the upgrade to the end of the period (inclusive).
    let remaining_days = date_diff(upgrade_date, period.to_date);

    // Old plan: credit the unused days that were billed in advance.
    let old_plan_credit_cents = (old_plan.single_day_price_over(dates)
        * rust_decimal::Decimal::from(remaining_days))
    .round()
    .to_i64()
    .unwrap_or(0);

    // New subscription created from the upgrade.
    let now = Utc::now();
    let mut new_subscription = old_subscription.clone();
    new_subscription.id = uuid::Uuid::new_v4();
    new_subscription.plan_id = new_plan.id;
    new_subscription.previous_subscription_id = Some(old_subscription.id);
    new_subscription.status = SubscriptionStatus::Active;
    new_subscription.started_at = Some(now);
    new_subscription.created_at = now;
    new_subscription.updated_at = now;

    // Old subscription terminated by the upgrade.
    let mut old_subscription = old_subscription.clone();
    old_subscription.status = SubscriptionStatus::Terminated;
    old_subscription.terminated_at = Some(now);
    old_subscription.updated_at = now;

    // New plan billed for the remaining days of the period.
    let fees = generate_subscription_invoice(
        &mut new_invoice,
        new_plan,
        &new_subscription,
        dates,
        Proration::Prorated {
            days_to_bill: remaining_days,
        },
        charges,
        taxes,
        coupons,
    )?;

    Ok(UpgradeResult {
        invoice: new_invoice,
        fees,
        old_subscription,
        new_subscription,
        old_plan_credit_cents,
        new_plan_days_billed: remaining_days,
    })
}

/// Ingest-to-bill helper: given the already-scoped period events for one charge,
/// build its charge fee (proving events → aggregation → charge → fee).
pub fn bill_charge_from_events(
    metric: &BillableMetric,
    charge: &Charge,
    subscription: &Subscription,
    events: &[Event],
    currency: rago_core::money::Currency,
) -> Result<Fee> {
    crate::charge_to_fee(metric, charge, subscription, events, currency, None)
}

// Small extension so the upgrade credit can price a day of the old plan.
trait SingleDayPrice {
    fn single_day_price_over(&self, dates: &MonthlyDates) -> rust_decimal::Decimal;
}
impl SingleDayPrice for Plan {
    fn single_day_price_over(&self, dates: &MonthlyDates) -> rust_decimal::Decimal {
        dates.single_day_price(self.amount_cents.unwrap_or(0))
    }
}
