//! Progressive billing: usage thresholds on plans (inherited by their
//! subscriptions) and per-subscription overrides, plus an on-demand check.
//!
//! * `GET|PUT /plans/:code/usage_thresholds`
//! * `GET|PUT /subscriptions/:external_id/usage_thresholds`
//! * `POST /subscriptions/:external_id/usage_thresholds/check`
//!
//! `PUT` replaces the whole set: `{ "usage_thresholds": [{ "amount_cents",
//! "recurring", "threshold_display_name" }] }` (an empty list clears it; for a
//! subscription that falls back to the plan's thresholds).

use axum::extract::{Path, State};
use axum::Json;
use chrono::Utc;
use rago_db::progressive::{validate_thresholds, NewUsageThreshold, ThresholdOwner, UsageThresholdRow};
use serde_json::{json, Value};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{bool_of, i64_of, s};
use crate::state::AppState;

/// Longest accepted display name.
const MAX_DISPLAY_NAME: usize = 255;

fn threshold_json(t: &UsageThresholdRow) -> Value {
    json!({
        "lago_id": t.id,
        "threshold_display_name": t.threshold_display_name,
        "amount_cents": t.amount_cents,
        "recurring": t.recurring,
        "created_at": t.created_at.to_rfc3339(),
    })
}

/// Parse and validate the `usage_thresholds` array of a request body.
pub(crate) fn parse_thresholds(body: &Value) -> ApiResult<Vec<NewUsageThreshold>> {
    let items = body
        .get("usage_thresholds")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::validation("usage_thresholds", "value_is_mandatory"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let amount_cents = i64_of(item, "amount_cents")
            .ok_or_else(|| ApiError::validation("amount_cents", "value_is_mandatory"))?;
        let name = s(item, "threshold_display_name").filter(|n| !n.trim().is_empty());
        if name.as_deref().is_some_and(|n| n.len() > MAX_DISPLAY_NAME) {
            return Err(ApiError::validation("threshold_display_name", "value_is_too_long"));
        }
        out.push(NewUsageThreshold {
            threshold_display_name: name,
            amount_cents,
            recurring: bool_of(item, "recurring").unwrap_or(false),
        });
    }
    validate_thresholds(&out).map_err(|code| ApiError::validation("usage_thresholds", code))?;
    Ok(out)
}

fn list_json(rows: &[UsageThresholdRow], inherited: bool) -> Json<Value> {
    Json(json!({
        "usage_thresholds": rows.iter().map(threshold_json).collect::<Vec<_>>(),
        "inherited_from_plan": inherited,
    }))
}

/// `GET /plans/:code/usage_thresholds`.
pub async fn list_plan_thresholds(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let rows = st.db.usage_thresholds_for(ThresholdOwner::Plan(plan.id)).await?;
    Ok(list_json(&rows, false))
}

/// `PUT /plans/:code/usage_thresholds`.
pub async fn replace_plan_thresholds(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let plan = st
        .db
        .find_plan_by_code(org, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let items = parse_thresholds(&body)?;
    let rows = st
        .db
        .replace_usage_thresholds(org, ThresholdOwner::Plan(plan.id), &items)
        .await?;
    Ok(list_json(&rows, false))
}

/// `GET /subscriptions/:external_id/usage_thresholds` — the thresholds in
/// force (the subscription's own, or its plan's when it has none).
pub async fn list_subscription_thresholds(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let own = st.db.usage_thresholds_for(ThresholdOwner::Subscription(sub.id)).await?;
    if !own.is_empty() {
        return Ok(list_json(&own, false));
    }
    let inherited = st.db.usage_thresholds_for(ThresholdOwner::Plan(sub.plan_id)).await?;
    Ok(list_json(&inherited, true))
}

/// `PUT /subscriptions/:external_id/usage_thresholds`.
pub async fn replace_subscription_thresholds(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st
        .db
        .find_subscription_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let items = parse_thresholds(&body)?;
    let rows = st
        .db
        .replace_usage_thresholds(org, ThresholdOwner::Subscription(sub.id), &items)
        .await?;
    if rows.is_empty() {
        let inherited = st.db.usage_thresholds_for(ThresholdOwner::Plan(sub.plan_id)).await?;
        return Ok(list_json(&inherited, true));
    }
    Ok(list_json(&rows, false))
}

/// `POST /subscriptions/:external_id/usage_thresholds/check` — evaluate the
/// thresholds now (the clock does this every few minutes) and issue a
/// progressive invoice when one was crossed.
pub async fn check_subscription_thresholds(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let issued = rago_jobs::progressive_billing::check_usage_thresholds(
        &st.db,
        &sub,
        Utc::now().date_naive(),
    )
    .await
    .map_err(|e| {
        tracing::error!(subscription_id = %sub.id, error = %e, "usage threshold check failed");
        ApiError::Internal("usage_threshold_check_failed".into())
    })?;
    Ok(Json(json!({
        "progressive_invoice": issued.map(|inv| json!({
            "lago_id": inv.id,
            "number": inv.number,
            "invoice_type": inv.invoice_type.as_str(),
            "total_amount_cents": inv.total_amount_cents,
            "currency": inv.currency,
        })),
    })))
}
