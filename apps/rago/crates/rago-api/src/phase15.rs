//! Phase-15 handlers — v2 REST surface (batch events ingestion, fee estimate,
//! data-API analytics), dunning-campaign configuration, and lifetime-usage
//! thresholds (long-tail domain).

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_billing::events::{build_event, validate_creation, EventParams};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{dec_of, i64_of, now, req_s, s, wrapped};
use crate::serializers as ser;
use crate::state::AppState;

// ---- v2 events ingestion (batch) -------------------------------------------

/// `POST /api/v2/events/batch` — ingest a batch of events atomically-ish
/// (validate + dedup each; returns per-event results). Reuses the same
/// validation/build path as v1 single ingestion.
pub async fn v2_events_batch(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ApiError::validation("events", "must_be_an_array"))?;
    if events.is_empty() {
        return Err(ApiError::validation("events", "must_not_be_empty"));
    }

    let mut created = Vec::new();
    let mut errors = Vec::new();
    for (idx, ev) in events.iter().enumerate() {
        let params = EventParams {
            code: req_s(ev, "code")?,
            transaction_id: req_s(ev, "transaction_id")?,
            external_subscription_id: req_s(ev, "external_subscription_id")?,
            properties: ev.get("properties").cloned().unwrap_or_else(|| json!({})),
            timestamp: ev.get("timestamp").and_then(|t| t.as_f64().or_else(|| t.as_str().and_then(|s| s.parse().ok()))),
            precise_total_amount_cents: dec_of(ev, "precise_total_amount_cents"),
        };
        let sub = st.db.find_subscription_by_external_id(org, &params.external_subscription_id).await?;
        let sub_ext_ids: Vec<String> = sub.as_ref().map(|s| vec![s.external_id.clone()]).unwrap_or_default();
        let metric = st.db.find_metric_by_code(org, &params.code).await?;
        let exists = st
            .db
            .event_exists(org, &params.external_subscription_id, &params.transaction_id)
            .await?;
        match validate_creation(&params, &sub_ext_ids, metric.as_ref(), exists) {
            Ok(()) => {
                let mut event = build_event(org, &params, now());
                event.customer_id = sub.as_ref().map(|s| s.customer_id);
                event.subscription_id = sub.as_ref().map(|s| s.id);
                // DB unique index is the authoritative guard; a lost race yields a
                // no-op insert, reported as a duplicate rather than double-metered.
                if st.db.insert_event(&event).await? {
                    created.push(ser::event(&event));
                } else {
                    errors.push(json!({
                        "index": idx,
                        "transaction_id": params.transaction_id,
                        "error": "value_is_missing_or_already_exists",
                    }));
                }
            }
            Err(e) => {
                errors.push(json!({ "index": idx, "transaction_id": params.transaction_id, "error": format!("{e:?}") }));
            }
        }
    }

    Ok(Json(json!({
        "events": created,
        "errors": errors,
        "created_count": created.len(),
        "error_count": errors.len(),
    })))
}

// ---- v2 data-API analytics -------------------------------------------------

fn analytics_points_json(points: Vec<rago_db::AnalyticsPoint>) -> Vec<Value> {
    points
        .into_iter()
        .map(|p| json!({ "month": p.month, "currency": p.currency, "amount_cents": p.amount_cents, "count": p.count }))
        .collect()
}

/// `GET /api/v2/analytics/gross_revenue`.
pub async fn v2_gross_revenue(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let points = st.db.gross_revenues(auth.organization.id).await?;
    Ok(Json(json!({ "gross_revenues": analytics_points_json(points) })))
}

/// `GET /api/v2/analytics/mrr`.
pub async fn v2_mrr(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let points = st.db.mrrs(auth.organization.id).await?;
    Ok(Json(json!({ "mrrs": analytics_points_json(points) })))
}

/// `GET /api/v2/analytics/overdue_balance`.
pub async fn v2_overdue_balance(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let points = st.db.overdue_balances(auth.organization.id).await?;
    Ok(Json(json!({ "overdue_balances": analytics_points_json(points) })))
}

/// `GET /api/v2/analytics/invoice_collection`.
pub async fn v2_invoice_collection(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let points = st.db.invoice_collections(auth.organization.id).await?;
    let collection: Vec<Value> = points
        .into_iter()
        .map(|p| json!({
            "month": p.month, "currency": p.currency, "payment_status": p.payment_status,
            "invoices_count": p.invoices_count, "amount_cents": p.amount_cents,
        }))
        .collect();
    Ok(Json(json!({ "invoice_collections": collection })))
}

// ---- dunning-campaign configuration ----------------------------------------

/// `POST /dunning_campaigns` — configure a dunning campaign.
pub async fn create_dunning_campaign(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "dunning_campaign");
    let name = req_s(p, "name")?;
    let code = req_s(p, "code")?;
    let max_attempts = i64_of(p, "max_attempts").unwrap_or(3) as i32;
    let days_between = i64_of(p, "days_between_attempts").unwrap_or(7) as i32;
    let applied = p.get("applied_to_organization").and_then(Value::as_bool).unwrap_or(false);
    let bcc = p.get("bcc_emails").cloned().unwrap_or_else(|| json!([]));
    let id = Uuid::new_v4();
    st.db
        .insert_dunning_campaign(id, org, &name, &code, s(p, "description").as_deref(), max_attempts, days_between, &bcc, applied)
        .await?;
    Ok(Json(json!({
        "dunning_campaign": {
            "lago_id": id, "name": name, "code": code,
            "max_attempts": max_attempts, "days_between_attempts": days_between,
            "applied_to_organization": applied, "bcc_emails": bcc,
        }
    })))
}

/// `GET /dunning_campaigns` — the org's configured dunning campaigns.
pub async fn list_dunning_campaigns(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_dunning_campaigns(auth.organization.id).await?;
    let collection: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, code, max, days, applied)| json!({
            "lago_id": id, "name": name, "code": code,
            "max_attempts": max, "days_between_attempts": days, "applied_to_organization": applied,
        }))
        .collect();
    Ok(Json(json!({ "dunning_campaigns": collection })))
}

// ---- lifetime usage + lifetime-usage threshold alerts ----------------------

#[derive(Debug, Deserialize)]
pub struct LifetimeUsageQuery {
    pub external_subscription_id: String,
}

/// `GET /customers/:external_customer_id/lifetime_usage?external_subscription_id=`
/// — the subscription's cumulative usage (invoiced charge fees + live current
/// usage), persisted and evaluated against `lifetime_usage_amount` alerts.
pub async fn lifetime_usage(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_customer_id): Path<String>,
    Query(q): Query<LifetimeUsageQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let sub = st
        .db
        .find_subscription_by_external_id(org, &q.external_subscription_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    if sub.customer_id != customer.id {
        return Err(ApiError::NotFound("subscription".into()));
    }

    // Invoiced usage (finalized charge fees) + live current-period usage.
    let invoiced = st.db.subscription_invoiced_usage(org, &sub.external_id).await?;
    let current = current_usage_amount(&st, org, &sub).await?;
    let total = invoiced + current;
    st.db.upsert_lifetime_usage(org, &sub.external_id, invoiced, current).await?;

    let alerts_fired = evaluate_lifetime_alerts(&st, org, &sub.external_id, total).await?;

    Ok(Json(json!({
        "lifetime_usage": {
            "external_subscription_id": sub.external_id,
            "invoiced_usage_amount_cents": invoiced,
            "current_usage_amount_cents": current,
            "total_usage_amount_cents": total,
            "alerts_fired": alerts_fired,
        }
    })))
}

/// Compute the subscription's live current-period usage amount (charge fees),
/// reusing the billing engine (no persistence).
pub(crate) async fn current_usage_amount(st: &AppState, org: Uuid, sub: &rago_core::models::Subscription) -> ApiResult<i64> {
    use chrono::{Datelike, NaiveDate, Utc};
    use rago_billing::dates::MonthlyDates;
    let plan = st.db.find_plan(sub.plan_id).await?;
    let currency = plan.currency();
    let charges = st.db.charges_for_plan(plan.id).await?;
    let today = Utc::now().date_naive();
    let billing_at = if today.month() == 12 {
        NaiveDate::from_ymd_opt(today.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1).unwrap()
    };
    let dates = MonthlyDates {
        subscription_at: sub.subscription_at.map(|t| t.date_naive()).unwrap_or(today),
        billing_at,
        anniversary: matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary),
        pay_in_advance: plan.pay_in_advance,
        current_usage: false,
    };
    let from = dates.boundaries().from_date.and_hms_opt(0, 0, 0).unwrap();
    let to = Utc::now().naive_utc();
    let mut amount = 0i64;
    for charge in &charges {
        let Some(mid) = charge.billable_metric_id else { continue };
        let metric = st.db.find_billable_metric(mid).await?;
        let events = st.db.events_for_external_subscription(org, &sub.external_id, &metric.code, from, to).await?;
        let fee = rago_billing::charge_to_fee(&metric, charge, sub, &events, currency, None)?;
        amount += fee.amount_cents;
    }
    Ok(amount)
}

/// Evaluate `lifetime_usage_amount` alerts against the cumulative total (fires a
/// trigger + `alert.triggered` webhook per crossing). Returns the number fired.
pub async fn evaluate_lifetime_alerts(st: &AppState, org: Uuid, sub_ext: &str, lifetime_total_cents: i64) -> ApiResult<u32> {
    let alerts = st.db.list_usage_alerts(org, sub_ext).await?;
    let mut fired = 0u32;
    for (id, alert_type, _metric, threshold, recurring, previous_value, last_triggered) in alerts {
        if alert_type != "lifetime_usage_amount" {
            continue;
        }
        let current = lifetime_total_cents;
        let crossed = if recurring {
            threshold > 0 && (current / threshold) > (previous_value / threshold)
        } else {
            current >= threshold && last_triggered.is_none()
        };
        if !crossed {
            continue;
        }
        st.db.insert_alert_trigger(Uuid::new_v4(), org, id, sub_ext, threshold, current).await?;
        st.db.mark_alert_triggered(id, current).await?;
        let endpoint = st.db.webhook_endpoint(org).await?.unwrap_or_else(|| "https://webhook.example/rago".into());
        let payload = json!({
            "webhook_type": "alert.triggered",
            "alert": { "lago_id": id, "external_subscription_id": sub_ext, "alert_type": alert_type, "threshold_cents": threshold, "current_value_cents": current }
        });
        let _ = st.db.upsert_webhook(Uuid::new_v4(), org, "alert.triggered", "Alert", &endpoint, &payload, "pending", None, 0).await;
        fired += 1;
    }
    Ok(fired)
}
