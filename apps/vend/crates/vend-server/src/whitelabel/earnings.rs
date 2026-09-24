//! Earnings and payouts.
//!
//! - `GET /v1/customer/earnings?from=&to=` — the founder's captured revenue per
//!   UTC day (what end users paid, provider cost, platform fee, tenant net),
//!   payout totals and the most recent split payouts to the tenant account.
//! - `GET /v1/control/payouts?pending=true&limit=` — platform-wide payout
//!   progress (operator).
//! - `GET /v1/control/tenants/:tenant_id/earnings?from=&to=` — one tenant's
//!   revenue (operator).
//!
//! Every figure is derived from captured usage and the durable payout plan;
//! Mint remains the authoritative ledger for balances.

use axum::extract::{Extension, Path, Query, State};
use axum::response::Response;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::json;

use vend_core::error::VendError;
use vend_core::ids::TenantId;
use vend_store::CustomerPrincipal;

use super::{fail, ok, ConsoleApiState};

/// Longest range served in one request.
const MAX_RANGE_DAYS: i64 = 366;

#[derive(Deserialize)]
pub struct RangeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Deserialize)]
pub struct PayoutQuery {
    pub pending: Option<bool>,
    pub limit: Option<i64>,
}

/// Resolve `[from, to]`, defaulting to the last 30 UTC days.
fn range(query: &RangeQuery) -> Result<(String, String), VendError> {
    let parse = |raw: &Option<String>, default: NaiveDate| match raw {
        Some(raw) => NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| VendError::BadId),
        None => Ok(default),
    };
    let to = parse(&query.to, Utc::now().date_naive())?;
    let from = parse(&query.from, to - chrono::Duration::days(29))?;
    let span = (to - from).num_days();
    if !(0..MAX_RANGE_DAYS).contains(&span) {
        return Err(VendError::BadId);
    }
    Ok((from.to_string(), to.to_string()))
}

async fn earnings_report(
    state: &ConsoleApiState,
    tenant_id: &TenantId,
    query: &RangeQuery,
) -> Result<serde_json::Value, VendError> {
    let (from, to) = range(query)?;
    let days = state
        .whitelabel
        .tenant_earnings(tenant_id, &from, &to)
        .await?;
    let totals = state.whitelabel.payout_totals(Some(tenant_id)).await?;
    let recent = state
        .whitelabel
        .list_payouts(Some(tenant_id), false, 50)
        .await?;
    let sum = |f: fn(&vend_core::whitelabel::EarningsDay) -> i64| days.iter().map(f).sum::<i64>();
    Ok(json!({
        "from": from,
        "to": to,
        "summary": {
            "captured_requests": sum(|d| d.captured_requests),
            "charge_micros": sum(|d| d.charge_micros),
            "provider_cost_micros": sum(|d| d.provider_cost_micros),
            "platform_fee_micros": sum(|d| d.platform_fee_micros),
            "tenant_net_micros": sum(|d| d.tenant_net_micros),
        },
        "days": days,
        "payouts": { "totals": totals, "recent": recent },
    }))
}

pub async fn customer_earnings(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Query(query): Query<RangeQuery>,
) -> Response {
    match earnings_report(&state, &principal.tenant_id, &query).await {
        Ok(report) => ok(report),
        Err(error) => fail(error),
    }
}

pub async fn operator_tenant_earnings(
    State(state): State<ConsoleApiState>,
    Path(raw): Path<String>,
    Query(query): Query<RangeQuery>,
) -> Response {
    let tenant_id = match TenantId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match earnings_report(&state, &tenant_id, &query).await {
        Ok(report) => ok(report),
        Err(error) => fail(error),
    }
}

pub async fn operator_payouts(
    State(state): State<ConsoleApiState>,
    Query(query): Query<PayoutQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let result = async {
        let totals = state.whitelabel.payout_totals(None).await?;
        let payouts = state
            .whitelabel
            .list_payouts(None, query.pending.unwrap_or(false), limit)
            .await?;
        Ok::<_, VendError>(json!({ "totals": totals, "payouts": payouts }))
    }
    .await;
    match result {
        Ok(body) => ok(body),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_defaults_to_thirty_days_and_rejects_inverted_or_huge_ranges() {
        let (from, to) = range(&RangeQuery {
            from: None,
            to: Some("2026-09-30".into()),
        })
        .unwrap();
        assert_eq!((from.as_str(), to.as_str()), ("2026-09-01", "2026-09-30"));
        assert!(range(&RangeQuery {
            from: Some("2026-09-30".into()),
            to: Some("2026-09-01".into())
        })
        .is_err());
        assert!(range(&RangeQuery {
            from: Some("2024-01-01".into()),
            to: Some("2026-01-01".into())
        })
        .is_err());
        assert!(range(&RangeQuery {
            from: Some("bad".into()),
            to: None
        })
        .is_err());
    }
}
