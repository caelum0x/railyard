//! Scheduled plan downgrades (`Subscriptions::PlanDowngradeService`).
//!
//! A downgrade never takes effect mid-period: the current subscription keeps
//! running until its period rolls over, and a **pending** next subscription on
//! the cheaper plan starts at that boundary. The activation clock
//! (`rago_jobs::clock::subscriptions::apply_scheduled_downgrades`) bills the
//! outgoing plan's closing period, then atomically terminates it and activates
//! the new one. No credit note is issued: the closing period is billed in full
//! on the plan the customer had during it.
//!
//! Shared by the REST API (`/subscriptions/:external_id/plan_change`) and the
//! customer portal.

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use rago_billing::downgrade::{check_downgrade, next_billing_date, schedule_downgrade};
use rago_core::enums::{BillingTime, SubscriptionStatus};
use rago_core::models::{Plan, Subscription};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{now, req_s, wrapped};
use crate::serializers as ser;
use crate::state::AppState;

/// Most plans considered when listing downgrade targets.
const PLAN_SCAN_LIMIT: i64 = 500;

/// When a downgrade scheduled now for `sub` (on `plan`) takes effect: 00:00 UTC
/// of the next billing date.
pub fn effective_at(sub: &Subscription, plan: &Plan) -> DateTime<Utc> {
    let today = Utc::now().date_naive();
    let interval = rago_jobs::scheduler::interval_of(&plan.interval);
    let anniversary = matches!(sub.billing_time, BillingTime::Anniversary);
    let anchor = sub.subscription_at.map(|t| t.date_naive()).unwrap_or(today);
    next_billing_date(interval, anniversary, anchor, today)
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default()
        .and_utc()
}

/// Plans `sub` (currently on `current`) can be downgraded to, most expensive
/// first.
pub async fn downgrade_options(st: &AppState, org: Uuid, current: &Plan) -> ApiResult<Vec<Plan>> {
    let mut plans: Vec<Plan> = st
        .db
        .list_plans(org, PLAN_SCAN_LIMIT, 0)
        .await?
        .into_iter()
        .filter(|p| p.parent_id.is_none() && check_downgrade(current, p).is_ok())
        .collect();
    plans.sort_by(|a, b| b.amount_cents.unwrap_or(0).cmp(&a.amount_cents.unwrap_or(0)));
    Ok(plans)
}

/// Schedule `sub` to move to `target` at its next billing date. An earlier
/// scheduled downgrade is replaced. Returns the pending subscription.
pub async fn schedule(st: &AppState, org: Uuid, sub: &Subscription, target: &Plan) -> ApiResult<Subscription> {
    if sub.organization_id != org || target.organization_id != org {
        return Err(ApiError::NotFound("subscription".into()));
    }
    if !matches!(sub.status, SubscriptionStatus::Active) {
        return Err(ApiError::validation("subscription", "subscription_is_not_active"));
    }
    let current = st.db.find_plan(sub.plan_id).await?;
    check_downgrade(&current, target).map_err(|r| ApiError::validation("plan_code", r.code()))?;

    let replaced = st.db.cancel_scheduled_downgrades(sub.id).await?;
    let at = effective_at(sub, &current);
    let mut pending = schedule_downgrade(sub, target, now()).pending_subscription;
    pending.subscription_at = Some(at);
    st.db.insert_subscription(&pending).await?;

    let ext_customer = st.db.customer_external_id(sub.customer_id).await?.unwrap_or_default();
    let _ = st
        .db
        .insert_activity_log(
            org,
            "subscription.downgrade_scheduled",
            "Subscription",
            sub.id,
            Some(&ext_customer),
            Some(&sub.external_id),
            &json!({
                "from_plan_code": current.code,
                "to_plan_code": target.code,
                "effective_at": at.to_rfc3339(),
                "replaced": replaced.iter().map(|s| s.id).collect::<Vec<_>>(),
            }),
        )
        .await;
    let mut body = ser::subscription(sub, &current, &ext_customer);
    body["next_plan_code"] = json!(target.code);
    body["downgrade_plan_date"] = json!(at.date_naive().to_string());
    body["next_subscription"] = ser::subscription(&pending, target, &ext_customer);
    crate::emit::webhook(&st.db, org, rago_integrations::webhooks::WebhookKind::SubscriptionUpdated, body).await;
    Ok(pending)
}

/// Cancel the downgrade scheduled for `sub`. Returns whether one existed.
pub async fn cancel(st: &AppState, org: Uuid, sub: &Subscription) -> ApiResult<bool> {
    let canceled = st.db.cancel_scheduled_downgrades(sub.id).await?;
    for c in &canceled {
        let plan = st.db.find_plan(c.plan_id).await?;
        let ext = st.db.customer_external_id(c.customer_id).await?.unwrap_or_default();
        crate::emit::webhook(
            &st.db,
            org,
            rago_integrations::webhooks::WebhookKind::SubscriptionCanceled,
            ser::subscription(c, &plan, &ext),
        )
        .await;
    }
    Ok(!canceled.is_empty())
}

async fn active_by_external_id(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Subscription> {
    st.db
        .find_subscription_by_external_id(org, external_id)
        .await?
        .filter(|s| matches!(s.status, SubscriptionStatus::Active))
        .ok_or_else(|| ApiError::NotFound("subscription".into()))
}

async fn plan_change_json(st: &AppState, org: Uuid, sub: &Subscription) -> ApiResult<Value> {
    let current = st.db.find_plan(sub.plan_id).await?;
    let scheduled = match st.db.scheduled_downgrade_for(sub.id).await? {
        Some(next) => {
            let plan = st.db.find_plan(next.plan_id).await?;
            json!({
                "lago_id": next.id,
                "plan_code": plan.code,
                "plan_name": plan.name,
                "plan_amount_cents": plan.amount_cents,
                "effective_at": next.subscription_at.map(|t| t.to_rfc3339()),
            })
        }
        None => Value::Null,
    };
    let options = downgrade_options(st, org, &current).await?;
    Ok(json!({ "plan_change": {
        "external_subscription_id": sub.external_id,
        "current_plan_code": current.code,
        "effective_at": effective_at(sub, &current).to_rfc3339(),
        "scheduled": scheduled,
        "downgrade_options": options.iter().map(|p| json!({
            "plan_code": p.code, "name": p.name, "amount_cents": p.amount_cents, "amount_currency": p.amount_currency,
        })).collect::<Vec<_>>(),
    }}))
}

/// `GET /subscriptions/:external_id/plan_change` — the scheduled downgrade
/// and the plans the subscription can move down to.
pub async fn show(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = active_by_external_id(&st, auth.organization.id, &external_id).await?;
    Ok(Json(plan_change_json(&st, auth.organization.id, &sub).await?))
}

/// `POST /subscriptions/:external_id/plan_change` with
/// `{ "plan_change": { "plan_code": "starter" } }` — schedule a downgrade.
pub async fn create(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = active_by_external_id(&st, org, &external_id).await?;
    let code = req_s(wrapped(&body, "plan_change"), "plan_code")?;
    let target = st
        .db
        .find_plan_by_code(org, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    schedule(&st, org, &sub, &target).await?;
    Ok(Json(plan_change_json(&st, org, &sub).await?))
}

/// `DELETE /subscriptions/:external_id/plan_change` — keep the current plan.
pub async fn destroy(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = active_by_external_id(&st, org, &external_id).await?;
    if !cancel(&st, org, &sub).await? {
        return Err(ApiError::NotFound("scheduled_plan_change".into()));
    }
    Ok(Json(plan_change_json(&st, org, &sub).await?))
}
