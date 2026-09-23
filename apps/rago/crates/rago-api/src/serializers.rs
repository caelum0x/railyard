//! v1 JSON serializers — faithful ports of `app/serializers/v1/*`. Each
//! function produces the resource object with Lago's exact top-level keys
//! (`lago_id`, `external_id`, cents as integers, decimals/timestamps as
//! strings). Root wrapping (`{ "customer": {...} }`) is applied by the handler.

use chrono::{DateTime, Utc};
use rago_core::models::*;
use serde_json::{json, Value};

fn ts(dt: DateTime<Utc>) -> Value {
    json!(dt.to_rfc3339())
}
fn ts_opt(dt: Option<DateTime<Utc>>) -> Value {
    match dt {
        Some(d) => json!(d.to_rfc3339()),
        None => Value::Null,
    }
}
fn dec(d: rust_decimal::Decimal) -> Value {
    json!(d.to_string())
}

/// `V1::CustomerSerializer` (core keys).
pub fn customer(c: &Customer) -> Value {
    json!({
        "lago_id": c.id,
        "external_id": c.external_id,
        "name": c.name,
        "email": c.email,
        "country": c.country,
        "currency": c.currency,
        "timezone": c.timezone,
        "tax_identification_number": c.tax_identification_number,
        "created_at": ts(c.created_at),
        "updated_at": ts(c.updated_at),
    })
}

/// `V1::PlanSerializer` (core keys).
pub fn plan(p: &Plan) -> Value {
    json!({
        "lago_id": p.id,
        "name": p.name,
        "invoice_display_name": p.invoice_display_name,
        "code": p.code,
        "interval": p.interval.map(|i| i.as_str()),
        "description": p.description,
        "amount_cents": p.amount_cents,
        "amount_currency": p.amount_currency,
        "trial_period": p.trial_period,
        "pay_in_advance": p.pay_in_advance,
        "bill_charges_monthly": p.bill_charges_monthly,
        "parent_id": p.parent_id,
        "created_at": ts(p.created_at),
    })
}

/// `V1::BillableMetricSerializer` (core keys).
pub fn billable_metric(m: &BillableMetric) -> Value {
    json!({
        "lago_id": m.id,
        "name": m.name,
        "code": m.code,
        "description": m.description,
        "aggregation_type": m.aggregation_type.as_str(),
        "recurring": m.recurring,
        "rounding_function": m.rounding_function,
        "rounding_precision": m.rounding_precision,
        "field_name": m.field_name,
        "expression": m.expression,
        "created_at": ts(m.created_at),
    })
}

/// `V1::ChargeSerializer` (core keys).
pub fn charge(c: &Charge) -> Value {
    json!({
        "lago_id": c.id,
        "lago_billable_metric_id": c.billable_metric_id,
        "code": c.code,
        "invoice_display_name": c.invoice_display_name,
        "charge_model": c.charge_model.as_str(),
        "invoiceable": c.invoiceable,
        "pay_in_advance": c.pay_in_advance,
        "prorated": c.prorated,
        "min_amount_cents": c.min_amount_cents,
        "accepts_target_wallet": c.accepts_target_wallet,
        "properties": c.properties,
        "lago_parent_id": c.parent_id,
        "created_at": ts(c.created_at),
    })
}

/// `V1::SubscriptionSerializer` (core keys). `plan` carries the plan's code and
/// amount, which Lago denormalizes onto the subscription.
pub fn subscription(s: &Subscription, plan: &Plan, external_customer_id: &str) -> Value {
    json!({
        "lago_id": s.id,
        "external_id": s.external_id,
        "lago_customer_id": s.customer_id,
        "external_customer_id": external_customer_id,
        "name": s.name,
        "plan_code": plan.code,
        "plan_amount_cents": plan.amount_cents,
        "plan_amount_currency": plan.amount_currency,
        "status": s.status.as_str(),
        "billing_time": s.billing_time.as_str(),
        "subscription_at": ts_opt(s.subscription_at),
        "started_at": ts_opt(s.started_at),
        "trial_ended_at": ts_opt(s.trial_ended_at),
        "ending_at": ts_opt(s.ending_at),
        "terminated_at": ts_opt(s.terminated_at),
        "canceled_at": ts_opt(s.canceled_at),
        "created_at": ts(s.created_at),
    })
}

/// `V1::EventSerializer`.
pub fn event(e: &Event) -> Value {
    json!({
        "lago_id": e.id,
        "transaction_id": e.transaction_id,
        "lago_customer_id": e.customer_id,
        "code": e.code,
        "timestamp": e.timestamp.map(|t| t.and_utc().to_rfc3339()),
        "precise_total_amount_cents": e.precise_total_amount_cents.map(|d| d.to_string()),
        "properties": e.properties,
        "lago_subscription_id": e.subscription_id,
        "external_subscription_id": e.external_subscription_id,
        "created_at": ts(e.created_at),
    })
}

/// `V1::FeeSerializer` (core keys).
pub fn fee(f: &Fee) -> Value {
    json!({
        "lago_id": f.id,
        "lago_charge_id": f.charge_id,
        "lago_invoice_id": f.invoice_id,
        "lago_subscription_id": f.subscription_id,
        "amount_cents": f.amount_cents,
        "amount_currency": f.amount_currency,
        "taxes_amount_cents": f.taxes_amount_cents,
        "taxes_rate": f.taxes_rate,
        "total_amount_cents": f.amount_cents + f.taxes_amount_cents,
        "units": dec(f.units),
        "precise_unit_amount": dec(f.precise_unit_amount),
        "events_count": f.events_count,
        "payment_status": f.payment_status.as_str(),
        "amount_details": f.amount_details,
        "created_at": ts(f.created_at),
    })
}

/// `V1::InvoiceSerializer` (core/root keys).
pub fn invoice(i: &Invoice, fees: &[Fee]) -> Value {
    json!({
        "lago_id": i.id,
        "lago_customer_id": i.customer_id,
        "sequential_id": i.sequential_id,
        "number": i.number,
        "issuing_date": i.issuing_date.map(|d| d.format("%Y-%m-%d").to_string()),
        "invoice_type": i.invoice_type.as_str(),
        "status": i.status.as_str(),
        "payment_status": i.payment_status.as_str(),
        "currency": i.currency,
        "fees_amount_cents": i.fees_amount_cents,
        "taxes_amount_cents": i.taxes_amount_cents,
        "coupons_amount_cents": i.coupons_amount_cents,
        "credit_notes_amount_cents": i.credit_notes_amount_cents,
        "sub_total_excluding_taxes_amount_cents": i.sub_total_excluding_taxes_amount_cents,
        "sub_total_including_taxes_amount_cents": i.sub_total_including_taxes_amount_cents,
        "total_amount_cents": i.total_amount_cents,
        "prepaid_credit_amount_cents": i.prepaid_credit_amount_cents,
        "progressive_billing_credit_amount_cents": i.progressive_billing_credit_amount_cents,
        "version_number": i.version_number,
        "created_at": ts(i.created_at),
        "updated_at": ts(i.updated_at),
        "fees": fees.iter().map(fee).collect::<Vec<_>>(),
    })
}

/// `V1::WalletSerializer` (core keys).
pub fn wallet(w: &Wallet, external_customer_id: &str) -> Value {
    json!({
        "lago_id": w.id,
        "lago_customer_id": w.customer_id,
        "external_customer_id": external_customer_id,
        "status": w.status.as_str(),
        "currency": w.balance_currency,
        "name": w.name,
        "rate_amount": dec(w.rate_amount),
        "credits_balance": dec(w.credits_balance),
        "credits_ongoing_balance": dec(w.credits_ongoing_balance),
        "credits_ongoing_usage_balance": dec(w.credits_ongoing_usage_balance),
        "balance_cents": w.balance_cents,
        "ongoing_balance_cents": w.ongoing_balance_cents,
        "ongoing_usage_balance_cents": w.ongoing_usage_balance_cents,
        "consumed_credits": dec(w.consumed_credits),
        "created_at": ts(w.created_at),
        "expiration_at": ts_opt(w.expiration_at),
        "terminated_at": ts_opt(w.terminated_at),
        "priority": w.priority,
    })
}

/// `V1::WalletTransactionSerializer` (core keys).
pub fn wallet_transaction(t: &WalletTransaction) -> Value {
    json!({
        "lago_id": t.id,
        "lago_wallet_id": t.wallet_id,
        "lago_invoice_id": t.invoice_id,
        "status": t.status.as_str(),
        "transaction_type": t.transaction_type.as_str(),
        "amount": dec(t.amount),
        "credit_amount": dec(t.credit_amount),
        "settled_at": ts_opt(t.settled_at),
        "created_at": ts(t.created_at),
    })
}

/// `V1::CouponSerializer`.
pub fn coupon(c: &Coupon) -> Value {
    json!({
        "lago_id": c.id,
        "name": c.name,
        "code": c.code,
        "coupon_type": c.coupon_type.as_str(),
        "amount_cents": c.amount_cents,
        "amount_currency": c.amount_currency,
        "percentage_rate": c.percentage_rate.map(|d| d.to_string()),
        "frequency": c.frequency.as_str(),
        "frequency_duration": c.frequency_duration,
        "reusable": c.reusable,
        "limited_plans": c.limited_plans,
        "limited_billable_metrics": c.limited_billable_metrics,
        "created_at": ts(c.created_at),
        "expiration": c.expiration.as_str(),
        "expiration_at": ts_opt(c.expiration_at),
        "terminated_at": ts_opt(c.deleted_at),
    })
}

/// `V1::CreditNoteSerializer` (core keys).
pub fn credit_note(cn: &CreditNote) -> Value {
    json!({
        "lago_id": cn.id,
        "sequential_id": cn.sequential_id,
        "number": cn.number,
        "lago_invoice_id": cn.invoice_id,
        "issuing_date": cn.issuing_date.format("%Y-%m-%d").to_string(),
        "reason": cn.reason,
        "currency": cn.credit_amount_currency,
        "total_amount_cents": cn.total_amount_cents,
        "taxes_amount_cents": cn.taxes_amount_cents,
        "balance_amount_cents": cn.balance_amount_cents,
        "credit_amount_cents": cn.credit_amount_cents,
        "refund_amount_cents": cn.refund_amount_cents,
        "coupons_adjustment_amount_cents": cn.coupons_adjustment_amount_cents,
        "taxes_rate": cn.taxes_rate,
        "created_at": ts(cn.created_at),
        "updated_at": ts(cn.updated_at),
    })
}
