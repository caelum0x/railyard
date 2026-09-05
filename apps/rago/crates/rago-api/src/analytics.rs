//! v1 analytics — the `invoiced_usage` data-API endpoint, computed from the
//! existing per-subscription invoiced-usage op (the four other analytics
//! endpoints reuse the phase-15 aggregate queries directly at the v1 paths).

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::Authed;
use crate::error::ApiResult;
use crate::state::AppState;

/// `GET /api/v1/analytics/invoiced_usage` — total invoiced charge usage across
/// the organization's subscriptions (aggregate; reuses
/// `subscription_invoiced_usage`).
pub async fn v1_invoiced_usage(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let subs = st.db.list_subscriptions(org, None, None, 10_000, 0).await?;
    let mut total: i64 = 0;
    for sub in &subs {
        total += st.db.subscription_invoiced_usage(org, &sub.external_id).await?;
    }
    Ok(Json(json!({
        "invoiced_usages": [{
            "amount_cents": total,
            "currency": auth.organization.default_currency,
            "subscriptions_count": subs.len(),
        }]
    })))
}
