//! Lago REST parity for events:
//!
//! * `POST /events/estimate_instant_fees` and `/events/batch_estimate_instant_fees`
//!   (`Fees::EstimateInstant::*`): what an event would cost on the
//!   subscription's pay-in-advance standard and percentage charges, priced at
//!   the event's charge filter, without storing anything;
//! * `GET /events_enriched`: the subscription's events as each charge reads
//!   them (value, charge, filter, pricing group), the Postgres twin of Lago's
//!   enriched-events store.

use axum::extract::{Query, State};
use axum::Json;
use chrono::{NaiveDateTime, Utc};
use rago_billing::charge_fees::{buckets, FilterSpec};
use rago_core::models::{BillableMetric, Charge, Event, Subscription};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{req_s, s};
use crate::state::AppState;

async fn subscription(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Subscription> {
    st.db.find_subscription_by_external_id(org, external_id).await?.ok_or_else(|| ApiError::NotFound("subscription".into()))
}

/// The units one event carries for a metric (Lago: the field value, or 1
/// when the metric has no field), rounded like the metric.
fn event_units(metric: &BillableMetric, event: &Event) -> Decimal {
    let units = match metric.field_name.as_deref().filter(|f| !f.is_empty()) {
        Some(f) => event.field_value(f),
        None => Decimal::ONE,
    };
    let precision = metric.rounding_precision.unwrap_or(0).max(0) as u32;
    match metric.rounding_function.as_deref() {
        Some("ceil") => rago_billing::aggregation::ceil_dp(units, precision),
        Some("floor") => rago_billing::aggregation::floor_dp(units, precision),
        Some("round") => units.round_dp_with_strategy(precision, rust_decimal::RoundingStrategy::MidpointAwayFromZero),
        _ => units,
    }
}

/// The filter an event falls in for a charge (`None`: the default bucket),
/// with the properties it is priced at and its pricing group values.
fn bucket_of(charge: &Charge, filters: &[FilterSpec], event: &Event) -> (Option<Uuid>, Value, Value) {
    let all = buckets(charge, filters);
    let bucket = all.iter().find(|b| b.filter_id.is_some() && b.contains(event)).or_else(|| all.iter().find(|b| b.filter_id.is_none()));
    match bucket {
        Some(b) => {
            let grouped: serde_json::Map<String, Value> = b
                .group_keys
                .iter()
                .map(|k| (k.clone(), event.properties.get(k).cloned().unwrap_or(Value::Null)))
                .collect();
            (b.filter_id, b.properties.clone(), Value::Object(grouped))
        }
        None => (None, charge.properties.clone(), json!({})),
    }
}

fn event_from(org: Uuid, sub: &Subscription, raw: &Value) -> ApiResult<Event> {
    let now = Utc::now();
    Ok(Event {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Some(sub.customer_id),
        subscription_id: Some(sub.id),
        transaction_id: s(raw, "transaction_id").unwrap_or_else(|| Uuid::new_v4().to_string()),
        code: req_s(raw, "code")?,
        properties: match raw.get("properties") {
            Some(p @ Value::Object(_)) => p.clone(),
            None | Some(Value::Null) => json!({}),
            Some(_) => return Err(ApiError::validation("properties", "value_is_invalid")),
        },
        external_customer_id: None,
        external_subscription_id: Some(sub.external_id.clone()),
        timestamp: Some(now.naive_utc()),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: false,
    })
}

/// One estimated fee (Lago's instant-fee payload).
async fn estimate(st: &AppState, sub: &Subscription, plan_currency: &rago_core::money::Currency, charge: &Charge, metric: &BillableMetric, event: Event) -> ApiResult<Value> {
    let event = rago_billing::events::with_expression(Some(metric), event).map_err(|e| ApiError::Validation(json!({ "event": [format!("expression_evaluation_failed: {e}")] })))?;
    let filters = st.db.charge_filter_specs(charge.id).await?;
    let (filter_id, properties, _) = bucket_of(charge, &filters, &event);
    let units = event_units(metric, &event);
    let amount = rago_billing::instant::instant_amount(charge.charge_model, &properties, units).unwrap_or_default();
    let exponent = plan_currency.exponent();
    let rounded = amount.round_dp_with_strategy(exponent, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
    let subunit = plan_currency.subunit_to_unit_decimal();
    let amount_cents: i64 = (rounded * subunit).trunc().try_into().unwrap_or(0);
    let unit_amount = if rounded.is_zero() || units.is_zero() { Decimal::ZERO } else { rounded / units };
    let external_customer_id = st.db.customer_external_id(sub.customer_id).await?;
    Ok(json!({
        "lago_id": null,
        "lago_charge_id": charge.id,
        "lago_charge_filter_id": filter_id,
        "lago_invoice_id": null,
        "lago_subscription_id": sub.id,
        "external_subscription_id": sub.external_id,
        "lago_customer_id": sub.customer_id,
        "external_customer_id": external_customer_id,
        "item": {
            "type": "charge",
            "code": metric.code,
            "name": metric.name,
            "invoice_display_name": charge.invoice_display_name.clone().unwrap_or_else(|| metric.name.clone()),
            "lago_item_id": metric.id,
            "item_type": "BillableMetric",
            "grouped_by": {},
        },
        "pay_in_advance": true,
        "invoiceable": charge.invoiceable,
        "amount_cents": amount_cents,
        "amount_currency": plan_currency.code(),
        "precise_amount": amount.normalize().to_string(),
        "total_amount_cents": amount_cents,
        "taxes_amount_cents": 0,
        "units": units.normalize().to_string(),
        "precise_unit_amount": (unit_amount * subunit).normalize().to_string(),
        "events_count": 1,
        "payment_status": "pending",
        "event_transaction_id": event.transaction_id,
    }))
}

/// The subscription's instant charges with their metrics.
async fn instant_charges(st: &AppState, sub: &Subscription) -> ApiResult<Vec<(Charge, BillableMetric)>> {
    let mut out = Vec::new();
    for c in st.db.charges_for_plan(sub.plan_id).await? {
        if !c.pay_in_advance || !rago_billing::instant::is_instant(c.charge_model) {
            continue;
        }
        let Some(mid) = c.billable_metric_id else { continue };
        let m = st.db.find_billable_metric(mid).await?;
        out.push((c, m));
    }
    Ok(out)
}

/// `POST /events/estimate_instant_fees` — `{ event: { code, external_subscription_id, properties } }`.
pub async fn estimate_instant_fees(State(st): State<AppState>, auth: Authed, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let e = body.get("event").unwrap_or(&body);
    let sub = subscription(&st, org, &req_s(e, "external_subscription_id")?).await?;
    let plan = st.db.find_plan(sub.plan_id).await?;
    let event = event_from(org, &sub, e)?;
    let matched: Vec<(Charge, BillableMetric)> = instant_charges(&st, &sub).await?.into_iter().filter(|(_, m)| m.code == event.code).collect();
    if matched.is_empty() {
        return Err(ApiError::validation("code", "does_not_match_an_instant_charge"));
    }
    let mut fees = Vec::new();
    for (c, m) in &matched {
        fees.push(estimate(&st, &sub, &plan.currency(), c, m, event.clone()).await?);
    }
    Ok(Json(json!({ "fees": fees })))
}

/// `POST /events/batch_estimate_instant_fees` — `{ events: [...] }` on one subscription.
pub async fn batch_estimate_instant_fees(State(st): State<AppState>, auth: Authed, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let events = body.get("events").and_then(Value::as_array).filter(|e| !e.is_empty()).ok_or_else(|| ApiError::validation("events", "value_is_mandatory"))?;
    if events.len() > 100 {
        return Err(ApiError::validation("events", "too_many_events"));
    }
    let sub_ext = req_s(&events[0], "external_subscription_id")?;
    if events.iter().any(|e| s(e, "external_subscription_id").as_deref() != Some(sub_ext.as_str())) {
        return Err(ApiError::validation("external_subscription_id", "all_events_must_share_one_subscription"));
    }
    let sub = subscription(&st, org, &sub_ext).await?;
    let plan = st.db.find_plan(sub.plan_id).await?;
    let charges = instant_charges(&st, &sub).await?;
    if charges.is_empty() {
        return Err(ApiError::validation("code", "does_not_match_an_instant_charge"));
    }
    let mut fees = Vec::new();
    for raw in events {
        let event = event_from(org, &sub, raw)?;
        for (c, m) in charges.iter().filter(|(_, m)| m.code == event.code) {
            fees.push(estimate(&st, &sub, &plan.currency(), c, m, event.clone()).await?);
        }
    }
    Ok(Json(json!({ "fees": fees })))
}

#[derive(Debug, Deserialize)]
pub struct EnrichedQuery {
    pub external_subscription_id: Option<String>,
    pub code: Option<String>,
    pub timestamp_from: Option<String>,
    pub timestamp_to: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

fn at(raw: Option<&str>) -> ApiResult<Option<NaiveDateTime>> {
    match raw {
        None | Some("") => Ok(None),
        Some(v) => chrono::DateTime::parse_from_rfc3339(v)
            .map(|d| Some(d.naive_utc()))
            .map_err(|_| ApiError::validation("timestamp", "invalid_format")),
    }
}

/// `GET /events_enriched?external_subscription_id=&code=&timestamp_from=&timestamp_to=`.
pub async fn events_enriched(State(st): State<AppState>, auth: Authed, Query(q): Query<EnrichedQuery>) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let ext = q.external_subscription_id.as_deref().filter(|s| !s.is_empty()).ok_or_else(|| ApiError::validation("external_subscription_id", "value_is_mandatory"))?;
    let sub = subscription(&st, org, ext).await?;
    let from = at(q.timestamp_from.as_deref())?.unwrap_or_default();
    let to = at(q.timestamp_to.as_deref())?.unwrap_or_else(|| Utc::now().naive_utc());
    let per_page = q.per_page.unwrap_or(100).clamp(1, 1000);
    let page = q.page.unwrap_or(1).max(1);
    let mut rows = Vec::new();
    for c in st.db.charges_for_plan(sub.plan_id).await? {
        let Some(mid) = c.billable_metric_id else { continue };
        let metric = st.db.find_billable_metric(mid).await?;
        if q.code.as_deref().is_some_and(|code| code != metric.code) {
            continue;
        }
        let filters = st.db.charge_filter_specs(c.id).await?;
        for e in st.db.events_for_external_subscription(org, &sub.external_id, &metric.code, from, to).await? {
            let (filter_id, _, grouped) = bucket_of(&c, &filters, &e);
            let value = event_units(&metric, &e);
            rows.push((
                e.timestamp,
                json!({
                    "transaction_id": e.transaction_id,
                    "lago_subscription_id": sub.id,
                    "external_subscription_id": sub.external_id,
                    "code": e.code,
                    "timestamp": e.timestamp.map(|t| t.and_utc().to_rfc3339()),
                    "properties": e.properties,
                    "value": value.normalize().to_string(),
                    "decimal_value": value.normalize().to_string(),
                    "lago_charge_id": c.id,
                    "lago_charge_filter_id": filter_id,
                    "grouped_by": grouped,
                    "enriched_at": e.created_at.to_rfc3339(),
                }),
            ));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let total = rows.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    let events: Vec<Value> = rows.into_iter().skip(start).take(per_page as usize).map(|(_, v)| v).collect();
    Ok(Json(json!({ "events": events, "meta": crate::pagination::meta(page, per_page, total) })))
}
