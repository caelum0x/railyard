//! Customer usage — real-time (`current`), projected (`projected`) and
//! historical (`past`) usage, all without generating a fresh invoice.
//!
//! * `current_usage` ports `Invoices::CustomerUsageService` (read path):
//!   aggregate the current period's events per charge, apply the charge model,
//!   return per-charge units + amount. Reuses the exact same engine
//!   (`charge_to_fee`) the billing run uses — no invoice is persisted.
//! * `projected_usage` extends that with Lago's `calculate_projected_usage`
//!   run-rate projection (`Fees::ProjectionService`): scale the elapsed-period
//!   usage up to the full period by the elapsed/total day ratio (recurring
//!   metrics are not projected, matching Lago).
//! * `past_usage` ports `PastUsageQuery`: the finalized invoice periods for the
//!   subscription, newest first, with per-charge usage from each period's fees.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Utc};
use rago_billing::charge_to_fee;
use rago_core::enums::BillingTime;
use rago_core::models::{Fee, Subscription};
use rago_billing::dates::MonthlyDates;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub external_subscription_id: String,
}

/// One charge's live usage, priced through the billing engine but not persisted.
struct ChargeUsage {
    charge_json: Value,
    metric_json: Value,
    metric_code: String,
    recurring: bool,
    units: Decimal,
    events_count: i64,
    amount_cents: i64,
}

/// Aggregate + price every charge on the subscription's plan for the window
/// `[from, to]`. Shared by `current`, `projected`, and per-period `past` usage.
async fn compute_charges_usage(
    st: &AppState,
    org: uuid::Uuid,
    sub: &Subscription,
    currency: rago_core::money::Currency,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> ApiResult<Vec<ChargeUsage>> {
    let plan = st.db.find_plan(sub.plan_id).await?;
    let charges = st.db.charges_for_plan(plan.id).await?;
    let mut out = Vec::new();
    for charge in &charges {
        let Some(mid) = charge.billable_metric_id else { continue };
        let metric = st.db.find_billable_metric(mid).await?;
        let events = st
            .db
            .events_for_external_subscription(org, &sub.external_id, &metric.code, from, to)
            .await?;
        let fee: Fee = charge_to_fee(&metric, charge, sub, &events, currency, None)?;
        out.push(ChargeUsage {
            charge_json: json!({ "lago_id": charge.id, "charge_model": charge.charge_model.as_str() }),
            metric_json: json!({ "lago_id": metric.id, "code": metric.code, "name": metric.name, "aggregation_type": metric.aggregation_type.as_str() }),
            metric_code: metric.code.clone(),
            recurring: metric.recurring,
            units: fee.units,
            events_count: fee.events_count.unwrap_or(0) as i64,
            amount_cents: fee.amount_cents,
        });
    }
    Ok(out)
}

/// Load the customer + subscription (scoped to the org) and the plan currency.
async fn load_scope(
    st: &AppState,
    org: uuid::Uuid,
    external_customer_id: &str,
    external_subscription_id: &str,
) -> ApiResult<(Subscription, rago_core::money::Currency, MonthlyDates)> {
    let customer = st
        .db
        .find_customer_by_external_id(org, external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let sub = st
        .db
        .find_subscription_by_external_id(org, external_subscription_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    if sub.customer_id != customer.id {
        return Err(ApiError::NotFound("subscription".into()));
    }
    let plan = st.db.find_plan(sub.plan_id).await?;
    let currency = plan.currency();
    let today = now_date();
    let dates = MonthlyDates {
        subscription_at: sub.subscription_at.map(|t| t.date_naive()).unwrap_or(today),
        billing_at: first_of_next_month(today),
        anniversary: matches!(sub.billing_time, BillingTime::Anniversary),
        pay_in_advance: plan.pay_in_advance,
        current_usage: false,
    };
    Ok((sub, currency, dates))
}

/// `GET /customers/:external_customer_id/current_usage?external_subscription_id=`.
pub async fn current_usage(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_customer_id): Path<String>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (sub, currency, dates) = load_scope(&st, org, &external_customer_id, &q.external_subscription_id).await?;
    let b = dates.boundaries();
    let from = b.from_date.and_hms_opt(0, 0, 0).unwrap();
    let to = Utc::now().naive_utc();
    let usages = compute_charges_usage(&st, org, &sub, currency, from, to).await?;

    let mut charges_usage: Vec<Value> = Vec::new();
    let mut amount_cents: i64 = 0;
    let mut per_metric: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for u in &usages {
        amount_cents += u.amount_cents;
        *per_metric.entry(u.metric_code.clone()).or_insert(0) += u.amount_cents;
        charges_usage.push(json!({
            "units": u.units.to_string(),
            "events_count": u.events_count,
            "amount_cents": u.amount_cents,
            "amount_currency": currency.code(),
            "charge": u.charge_json,
            "billable_metric": u.metric_json,
        }));
    }

    let alerts_fired = crate::phase14::evaluate_usage_alerts(&st, org, &sub.external_id, amount_cents, &per_metric).await?;

    Ok(Json(json!({
        "customer_usage": {
            "from_datetime": from.and_utc().to_rfc3339(),
            "to_datetime": to.and_utc().to_rfc3339(),
            "issuing_date": b.to_date.format("%Y-%m-%d").to_string(),
            "currency": currency.code(),
            "amount_cents": amount_cents,
            "total_amount_cents": amount_cents,
            "charges_usage": charges_usage,
            "alerts_fired": alerts_fired,
        }
    })))
}

/// `GET /customers/:external_customer_id/projected_usage?external_subscription_id=`
/// — `Customers::ProjectedUsageController#current`. Same live usage as
/// `current_usage`, plus a run-rate projection to the end of the period.
pub async fn projected_usage(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_customer_id): Path<String>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (sub, currency, dates) = load_scope(&st, org, &external_customer_id, &q.external_subscription_id).await?;
    let b = dates.boundaries();
    let from = b.from_date.and_hms_opt(0, 0, 0).unwrap();
    let to_period = b.to_date.and_hms_opt(23, 59, 59).unwrap();
    let now = Utc::now().naive_utc();
    let usages = compute_charges_usage(&st, org, &sub, currency, from, now).await?;

    // `Fees::ProjectionService#period_ratio`: elapsed / total period, clamped to
    // (0, 1]. Projected usage = current / ratio for non-recurring metrics
    // (recurring metrics are billed at their full quantity, never projected).
    let ratio = period_ratio(from, to_period, now);

    let mut charges_usage: Vec<Value> = Vec::new();
    let mut amount_cents: i64 = 0;
    let mut projected_amount_cents: i64 = 0;
    for u in &usages {
        amount_cents += u.amount_cents;
        let (proj_units, proj_amount) = if u.recurring || ratio <= 0.0 {
            (u.units, u.amount_cents)
        } else {
            let pu = u.units / Decimal::from_f64_retain(ratio).unwrap_or(Decimal::ONE);
            let pa = ((u.amount_cents as f64) / ratio).round() as i64;
            (pu, pa)
        };
        projected_amount_cents += proj_amount;
        charges_usage.push(json!({
            "units": u.units.to_string(),
            "events_count": u.events_count,
            "amount_cents": u.amount_cents,
            "amount_currency": currency.code(),
            "projected_units": proj_units.to_string(),
            "projected_amount_cents": proj_amount,
            "charge": u.charge_json,
            "billable_metric": u.metric_json,
        }));
    }

    Ok(Json(json!({
        "customer_projected_usage": {
            "from_datetime": from.and_utc().to_rfc3339(),
            "to_datetime": to_period.and_utc().to_rfc3339(),
            "issuing_date": b.to_date.format("%Y-%m-%d").to_string(),
            "currency": currency.code(),
            "amount_cents": amount_cents,
            "total_amount_cents": amount_cents,
            "projected_amount_cents": projected_amount_cents,
            "projected_total_amount_cents": projected_amount_cents,
            "charges_usage": charges_usage,
        }
    })))
}

/// `GET /customers/:external_customer_id/past_usage?external_subscription_id=`
/// — `Customers::UsageController#past` / `PastUsageQuery`. The finalized invoice
/// periods for the subscription, newest first, each with per-charge usage.
pub async fn past_usage(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_customer_id): Path<String>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (sub, _currency, _dates) = load_scope(&st, org, &external_customer_id, &q.external_subscription_id).await?;
    let periods = st.db.subscription_past_usage_periods(sub.id).await?;

    let mut usage_periods: Vec<Value> = Vec::new();
    for (invoice_id, charges_from, charges_to, issuing_date, currency, fees_amount_cents, taxes_amount_cents) in periods {
        let fees = st.db.fees_for_invoice(invoice_id).await?;
        let charges_usage: Vec<Value> = fees
            .iter()
            .filter(|f| f.charge_id.is_some())
            .map(|f| json!({
                "units": f.units.to_string(),
                "events_count": f.events_count.unwrap_or(0),
                "amount_cents": f.amount_cents,
                "amount_currency": f.amount_currency,
                "charge": { "lago_id": f.charge_id },
            }))
            .collect();
        usage_periods.push(json!({
            "lago_invoice_id": invoice_id,
            "from_datetime": charges_from.and_utc().to_rfc3339(),
            "to_datetime": charges_to.and_utc().to_rfc3339(),
            "issuing_date": issuing_date.format("%Y-%m-%d").to_string(),
            "currency": currency,
            "amount_cents": fees_amount_cents,
            "total_amount_cents": fees_amount_cents + taxes_amount_cents,
            "taxes_amount_cents": taxes_amount_cents,
            "charges_usage": charges_usage,
        }));
    }

    Ok(Json(json!({ "usage_periods": usage_periods })))
}

/// Elapsed / total ratio of `[from, to]` at instant `now`, clamped to `[0, 1]`.
fn period_ratio(from: NaiveDateTime, to: NaiveDateTime, now: NaiveDateTime) -> f64 {
    if now >= to {
        return 1.0;
    }
    if now <= from {
        return 0.0;
    }
    let total = (to - from).num_seconds() as f64;
    if total <= 0.0 {
        return 1.0;
    }
    let elapsed = (now - from).num_seconds() as f64;
    (elapsed / total).clamp(0.0, 1.0)
}

fn now_date() -> NaiveDate {
    Utc::now().date_naive()
}

fn first_of_next_month(date: NaiveDate) -> NaiveDate {
    if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1).unwrap()
    }
}
