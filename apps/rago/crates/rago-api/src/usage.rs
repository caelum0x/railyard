//! Customer usage — real-time (`current`), projected (`projected`) and
//! historical (`past`) usage, all without generating a fresh invoice.
//!
//! * `current_usage` ports `Invoices::CustomerUsageService` (read path):
//!   aggregate the current period's events per charge, apply the charge model,
//!   return per-charge units + amount. Reuses the exact same engine
//!   (`charge_fees`: every filter and pricing group line) the billing run
//!   uses — no invoice is persisted.
//! * `projected_usage` extends that with Lago's `calculate_projected_usage`
//!   run-rate projection (`Fees::ProjectionService`): scale the elapsed-period
//!   usage up to the full period by the elapsed/total day ratio (recurring
//!   metrics are not projected, matching Lago).
//! * `past_usage` ports `PastUsageQuery`: the finalized invoice periods for the
//!   subscription, newest first, with per-charge usage from each period's fees.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use rago_billing::charge_fees::charge_fees;
use rago_core::enums::WalletStatus;
use rago_core::models::{Fee, Subscription, Wallet};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
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
    /// One entry per charge filter line (Lago `charges_usage[].filters`).
    filters: Vec<Value>,
    /// One entry per pricing group (Lago `charges_usage[].grouped_usage`).
    grouped_usage: Vec<Value>,
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
        let Some(loaded) = st.db.load_charge_usage(org, &sub.external_id, charge, from, to, to).await? else {
            continue;
        };
        // Priced like the invoice: one line per filter and pricing group.
        let fees: Vec<Fee> = charge_fees(&loaded.usage(), sub, currency, None)?;
        let metric = &loaded.metric;
        let filter_values = |id: uuid::Uuid| {
            loaded
                .filters
                .iter()
                .find(|f| f.id == id)
                .map(|f| json!(f.values))
                .unwrap_or(Value::Null)
        };
        let filters: Vec<Value> = fees
            .iter()
            .filter_map(|f| {
                let id = f.charge_filter_id?;
                Some(json!({
                    "lago_charge_filter_id": id,
                    "values": filter_values(id),
                    "units": f.units.to_string(),
                    "events_count": f.events_count.unwrap_or(0),
                    "amount_cents": f.amount_cents,
                }))
            })
            .collect();
        let grouped_usage: Vec<Value> = fees
            .iter()
            .filter(|f| f.grouped_by.as_object().is_some_and(|o| !o.is_empty()))
            .map(|f| {
                json!({
                    "grouped_by": f.grouped_by,
                    "lago_charge_filter_id": f.charge_filter_id,
                    "units": f.units.to_string(),
                    "events_count": f.events_count.unwrap_or(0),
                    "amount_cents": f.amount_cents,
                })
            })
            .collect();
        out.push(ChargeUsage {
            charge_json: json!({ "lago_id": charge.id, "charge_model": charge.charge_model.as_str() }),
            metric_json: json!({ "lago_id": metric.id, "code": metric.code, "name": metric.name, "aggregation_type": metric.aggregation_type.as_str() }),
            metric_code: metric.code.clone(),
            recurring: metric.recurring,
            units: fees.iter().map(|f| f.units).sum(),
            events_count: fees.iter().map(|f| f.events_count.unwrap_or(0) as i64).sum(),
            amount_cents: fees.iter().map(|f| f.amount_cents).sum(),
            filters,
            grouped_usage,
        });
    }
    Ok(out)
}

/// One billable metric's usage in the current period (customer portal).
pub(crate) struct MetricUsage {
    pub metric_name: String,
    pub units: Decimal,
    pub amount_cents: i64,
}

/// Current-period usage of `sub` per billable metric, priced by the same
/// engine as the billing run: `(currency, period start, period end, rows)`.
pub(crate) async fn current_period_usage(
    st: &AppState,
    org: uuid::Uuid,
    sub: &Subscription,
) -> ApiResult<(rago_core::money::Currency, NaiveDate, NaiveDate, Vec<MetricUsage>)> {
    let plan = st.db.find_plan(sub.plan_id).await?;
    let currency = plan.currency();
    let b = CurrentPeriod::of(sub, &plan);
    let from = b.from_date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let usages = compute_charges_usage(st, org, sub, currency, from, Utc::now().naive_utc()).await?;
    let rows = usages
        .into_iter()
        .map(|u| MetricUsage {
            metric_name: u.metric_json["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .unwrap_or(&u.metric_code)
                .to_string(),
            units: u.units,
            amount_cents: u.amount_cents,
        })
        .collect();
    Ok((currency, b.from_date, b.to_date, rows))
}

/// Load the customer + subscription (scoped to the org) and the plan currency.
async fn load_scope(
    st: &AppState,
    org: uuid::Uuid,
    external_customer_id: &str,
    external_subscription_id: &str,
) -> ApiResult<(Subscription, rago_core::money::Currency, CurrentPeriod)> {
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
    let dates = CurrentPeriod::of(&sub, &plan);
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
    let b = dates;
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
            "filters": u.filters,
            "grouped_usage": u.grouped_usage,
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
    let b = dates;
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
            "filters": u.filters,
            "grouped_usage": u.grouped_usage,
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

/// The subscription's current usage period (any interval, either billing
/// time), starting no earlier than the subscription.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CurrentPeriod {
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
}

impl CurrentPeriod {
    pub(crate) fn of(sub: &Subscription, plan: &rago_core::models::Plan) -> Self {
        let (from_date, to_date) = rago_jobs::scheduler::current_usage_period(sub, plan, now_date());
        Self { from_date, to_date }
    }
}

// ---- reel generation: billing catalog, usage ingest, credit/cap gate --------

/// Default per-unit reel charge prices, in **cents** per billed unit, applied
/// when `RAGO_REEL_PRICING` is unset or omits a metric. Non-zero so an operator
/// can bill reel usage out of the box; override per metric via the env var (or
/// by editing the plan's charges after provisioning).
mod reel_pricing_default {
    /// `reel_generated` — price per generated reel.
    pub const GENERATED_CENTS: i64 = 20;
    /// `reel_render_seconds` — price per render-second.
    pub const RENDER_SECONDS_CENTS: i64 = 1;
    /// `reel_ai_credits` — price per AI credit consumed.
    pub const AI_CREDITS_CENTS: i64 = 1;
}

/// Per-unit reel charge prices in the plan currency's **main unit** — the value
/// a `standard` charge's `properties.amount` expects — one per reel metric.
#[derive(Debug, Clone)]
pub struct ReelPricing {
    generated: Decimal,
    render_seconds: Decimal,
    ai_credits: Decimal,
}

impl ReelPricing {
    /// Parse `RAGO_REEL_PRICING` — a JSON object mapping a reel metric `code` to
    /// an integer **cents-per-unit** price (e.g. `{"reel_generated":25}`) — into
    /// main-unit charge amounts. Falls back to [`reel_pricing_default`] for any
    /// metric the variable omits, and to the full default set when the variable
    /// is unset or not valid JSON. Never fails: a malformed value yields the
    /// defaults, so a bad env var can never take reel billing down at startup.
    fn from_env() -> Self {
        use crate::emit::reel_metric as m;
        let overrides: std::collections::HashMap<String, i64> =
            std::env::var("RAGO_REEL_PRICING")
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        let amount = |code: &str, default: i64| {
            cents_per_unit(overrides.get(code).copied().unwrap_or(default))
        };
        Self {
            generated: amount(m::GENERATED, reel_pricing_default::GENERATED_CENTS),
            render_seconds: amount(m::RENDER_SECONDS, reel_pricing_default::RENDER_SECONDS_CENTS),
            ai_credits: amount(m::AI_CREDITS, reel_pricing_default::AI_CREDITS_CENTS),
        }
    }
}

/// Process-wide reel pricing, parsed once from the environment at first use
/// (effectively at startup — the first `/reels/catalog` or provisioning read).
fn reel_pricing() -> &'static ReelPricing {
    static PRICING: std::sync::OnceLock<ReelPricing> = std::sync::OnceLock::new();
    PRICING.get_or_init(ReelPricing::from_env)
}

/// A `standard` charge's per-unit `amount` (currency main unit) from an integer
/// cents-per-unit price: `cents / 100` as an exact 2-dp [`Decimal`]. A negative
/// override is floored to `0` — pricing is never allowed to go negative.
fn cents_per_unit(cents: i64) -> Decimal {
    Decimal::new(cents.max(0), 2)
}

/// Request-ready billable-metric + plan definitions an operator provisions rago
/// with so reel usage (`POST /reels/usage`) prices correctly. The `billable_metrics`
/// entries are `POST /billable_metrics` bodies and `plan` is a `POST /plans` body
/// (same root-wrapped shape those endpoints accept, minus the wrapper).
///
/// Per-charge `amount` values are the active reel pricing loaded from
/// `RAGO_REEL_PRICING` (cents-per-unit), or the built-in [`reel_pricing_default`]
/// when it is unset. `seeds/menivor-reels.json` ships the same defaults as a
/// ready-to-import catalog so operators can bill without setting the env var.
pub fn reel_billing_catalog() -> Value {
    use crate::emit::reel_metric as m;
    let pricing = reel_pricing();
    json!({
        "billable_metrics": [
            {
                "name": "Reels generated",
                "code": m::GENERATED,
                "aggregation_type": "count_agg",
                "recurring": false,
            },
            {
                "name": "Reel render seconds",
                "code": m::RENDER_SECONDS,
                "aggregation_type": "sum_agg",
                "field_name": m::RENDER_SECONDS_FIELD,
                "recurring": false,
            },
            {
                "name": "Reel AI credits",
                "code": m::AI_CREDITS,
                "aggregation_type": "sum_agg",
                "field_name": m::AI_CREDITS_FIELD,
                "recurring": false,
            },
        ],
        "plan": {
            "name": "Menivor Reels",
            "code": crate::emit::REEL_PLAN_CODE,
            "interval": "monthly",
            "amount_cents": 0,
            "amount_currency": "USD",
            "pay_in_advance": false,
            "charges": [
                { "billable_metric_code": m::GENERATED, "charge_model": "standard", "properties": { "amount": pricing.generated.to_string() } },
                { "billable_metric_code": m::RENDER_SECONDS, "charge_model": "standard", "properties": { "amount": pricing.render_seconds.to_string() } },
                { "billable_metric_code": m::AI_CREDITS, "charge_model": "standard", "properties": { "amount": pricing.ai_credits.to_string() } },
            ],
        },
        "note": "Charge `amount` values are the active reel pricing (RAGO_REEL_PRICING cents-per-unit, or built-in defaults). Set RAGO_REEL_PRICING or edit the charges to re-price.",
    })
}

/// `GET /reels/catalog` — the canonical reel billing catalog (see
/// [`reel_billing_catalog`]) so an operator can provision rago consistently.
pub async fn reel_catalog(_auth: Authed) -> Json<Value> {
    Json(json!({ "reel_billing_catalog": reel_billing_catalog() }))
}

/// `POST /reels/usage` — Menivor's generation gateway reports one completed reel
/// render; rago expands it into the billable usage events (see
/// [`crate::emit::reel_usage_events`]) and ingests each through the shared
/// event path. Idempotent by `render_id`: re-reporting a render re-emits the
/// same `transaction_id`s, which are recognised as already billed and skipped.
pub async fn record_reel_usage(
    State(st): State<AppState>,
    auth: Authed,
    Json(usage): Json<crate::emit::ReelRenderUsage>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    if usage.render_id.trim().is_empty() {
        return Err(ApiError::validation("render_id", "value_is_mandatory"));
    }
    if usage.external_subscription_id.trim().is_empty() {
        return Err(ApiError::validation("external_subscription_id", "value_is_mandatory"));
    }

    let mut billed_events: Vec<Value> = Vec::new();
    let mut already_billed_metrics: Vec<String> = Vec::new();
    for params in crate::emit::reel_usage_events(&usage) {
        let code = params.code.clone();
        match crate::emit::ingest_event(&st, org, params).await? {
            crate::emit::Ingest::Created(event) => billed_events.push(ser::event(&event)),
            crate::emit::Ingest::Duplicate => already_billed_metrics.push(code),
        }
    }

    Ok(Json(json!({
        "reel_usage": {
            "render_id": usage.render_id,
            "external_subscription_id": usage.external_subscription_id,
            "billed_events": billed_events,
            "already_billed_metrics": already_billed_metrics,
        }
    })))
}

/// Body of `POST /customers/:external_customer_id/render_allowance`.
#[derive(Debug, Deserialize)]
pub struct AllowanceRequest {
    /// The subscription the render bills against.
    pub external_subscription_id: String,
    /// Estimated cost of the render about to start, in the plan currency's minor
    /// units (Menivor prices the reel's planned duration + AI credits against the
    /// plan's charges, or supplies a conservative upper bound).
    pub estimated_cost_cents: i64,
    /// Optional hard spend cap for the current period. When set, a render whose
    /// projected total usage would exceed it is denied.
    #[serde(default)]
    pub cap_cents: Option<i64>,
}

/// `POST /customers/:external_customer_id/render_allowance` — the server-side
/// credit/cap gate the app calls BEFORE starting a render.
///
/// Denies when the customer's prepaid credit balance cannot cover the estimated
/// cost, or when projected period usage would exceed the supplied spend cap. It
/// never starts (or records) a render — it only answers allow/deny along with
/// the figures behind the decision so the app can explain it to the user.
///
/// A customer with no active prepaid wallet is postpaid: credits never block,
/// only the spend cap can.
pub async fn render_allowance(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_customer_id): Path<String>,
    Json(req): Json<AllowanceRequest>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    if req.estimated_cost_cents < 0 {
        return Err(ApiError::validation("estimated_cost_cents", "value_is_invalid"));
    }
    if let Some(cap) = req.cap_cents {
        if cap < 0 {
            return Err(ApiError::validation("cap_cents", "value_is_invalid"));
        }
    }

    let customer = st
        .db
        .find_customer_by_external_id(org, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let (sub, currency, dates) =
        load_scope(&st, org, &external_customer_id, &req.external_subscription_id).await?;

    // Current-period usage priced through the billing engine (same read path as
    // `current_usage`), summed to a single amount.
    let b = dates;
    let from = b.from_date.and_hms_opt(0, 0, 0).unwrap();
    let now = Utc::now().naive_utc();
    let usages = compute_charges_usage(&st, org, &sub, currency, from, now).await?;
    let current_usage_cents: i64 = usages.iter().map(|u| u.amount_cents).sum();
    let projected_usage_cents = current_usage_cents.saturating_add(req.estimated_cost_cents);

    // Prepaid credit balance: sum of ACTIVE wallets' ongoing balance (net of
    // uninvoiced usage), matching Lago's `ongoing_balance_cents`.
    let wallets = st.db.wallets_for_customer(customer.id).await?;
    let active: Vec<&Wallet> = wallets
        .iter()
        .filter(|w| matches!(w.status, WalletStatus::Active))
        .collect();
    let has_prepaid_wallet = !active.is_empty();
    let credit_balance_cents: i64 = active.iter().map(|w| w.ongoing_balance_cents).sum();

    let mut reasons: Vec<&str> = Vec::new();
    if has_prepaid_wallet && req.estimated_cost_cents > credit_balance_cents {
        reasons.push("insufficient_credits");
    }
    if let Some(cap) = req.cap_cents {
        if projected_usage_cents > cap {
            reasons.push("usage_cap_exceeded");
        }
    }
    let allowed = reasons.is_empty();

    Ok(Json(json!({
        "render_allowance": {
            "allowed": allowed,
            "reasons": reasons,
            "external_customer_id": external_customer_id,
            "external_subscription_id": req.external_subscription_id,
            "currency": currency.code(),
            "estimated_cost_cents": req.estimated_cost_cents,
            "current_usage_cents": current_usage_cents,
            "projected_usage_cents": projected_usage_cents,
            "has_prepaid_wallet": has_prepaid_wallet,
            "credit_balance_cents": credit_balance_cents,
            "cap_cents": req.cap_cents,
        }
    })))
}
