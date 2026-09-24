//! REST CRUD tail — `update` / `destroy` / `index` / `show` for the core
//! catalog resources (customers, plans + charges, billable metrics, add-ons,
//! coupons, wallets). Every handler authenticates, calls an existing
//! [`rago_db::Db`] op, and serializes the Lago envelope. Portal / checkout URLs
//! are honest partials (null pending a live payment provider), matching the
//! GraphQL surface.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Datelike;
use rago_core::enums::*;
use rago_core::models::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{bool_of, dec_of, i64_of, now, req_s, s, wrapped};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

// ---- customers: destroy + nested indexes -----------------------------------

/// `DELETE /customers/:external_id` — soft-delete a customer.
pub async fn destroy_customer(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let out = ser::customer(&c);
    st.db.soft_delete_customer(c.id).await?;
    Ok(Json(json!({ "customer": out })))
}

async fn customer_or_404(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Customer> {
    st.db
        .find_customer_by_external_id(org, external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))
}

/// `GET /customers/:external_id/subscriptions`.
pub async fn customer_subscriptions(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let subs = st.db.subscriptions_for_customer(c.id, pg.per_page(), pg.offset()).await?;
    let total = st.db.count_subscriptions_for_customer(c.id).await?;
    let mut out = Vec::new();
    for sub in &subs {
        let plan = st.db.find_plan(sub.plan_id).await?;
        out.push(ser::subscription(sub, &plan, &external_id));
    }
    Ok(Json(json!({ "subscriptions": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `GET /customers/:external_id/invoices`.
pub async fn customer_invoices(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let invoices = st.db.invoices_for_customer(c.id, pg.per_page(), pg.offset()).await?;
    let total = st.db.count_invoices_for_customer(c.id).await?;
    let out: Vec<Value> = invoices.iter().map(|i| ser::invoice(i, &[])).collect();
    Ok(Json(json!({ "invoices": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `GET /customers/:external_id/wallets`.
pub async fn customer_wallets(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let wallets = st.db.wallets_for_customer(c.id).await?;
    let out: Vec<Value> = wallets.iter().map(|w| ser::wallet(w, &external_id)).collect();
    Ok(Json(json!({ "wallets": out })))
}

/// Map a collection-layer result (`Ok(Err(code))` = configuration/state
/// problem) to the REST envelope.
fn provider_url_result(
    field: &str,
    r: Result<Result<String, &'static str>, rago_jobs::JobError>,
) -> ApiResult<String> {
    match r {
        Ok(Ok(url)) => Ok(url),
        Ok(Err(code)) => Err(ApiError::validation(field, code)),
        Err(rago_jobs::JobError::Db(e)) => Err(ApiError::from(e)),
        Err(e) => Err(ApiError::Validation(json!({ field: [e.to_string()] }))),
    }
}

/// `GET /customers/:external_id/portal_url` — a Stripe Billing Portal session
/// where the customer manages payment methods (the Stripe customer is created
/// on first use).
pub async fn customer_portal_url(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let transport = rago_integrations::default_transport();
    let url = provider_url_result(
        "portal_url",
        rago_jobs::collection::portal_url(&st.db, &transport, &c).await,
    )?;
    Ok(Json(json!({ "customer": { "portal_url": url } })))
}

/// `POST /customers/:external_id/checkout_url` — a Stripe Checkout (setup
/// mode) URL where the customer saves a payment method; emits
/// `customer.checkout_url_generated`.
pub async fn customer_checkout_url(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let transport = rago_integrations::default_transport();
    let url = provider_url_result(
        "checkout_url",
        rago_jobs::collection::checkout_url(&st.db, &transport, &c).await,
    )?;
    Ok(Json(json!({
        "customer": {
            "lago_customer_id": c.id,
            "external_customer_id": c.external_id,
            "payment_provider": "stripe",
            "checkout_url": url,
        }
    })))
}

// ---- plans: index / update / destroy + nested charges ----------------------

async fn plan_with_charges(st: &AppState, plan: &Plan) -> ApiResult<Value> {
    let charges = st.db.charges_for_plan(plan.id).await?;
    let mut plan_json = ser::plan(plan);
    plan_json["charges"] = json!(crate::charge_filters::charges_json(st, &charges).await?);
    Ok(plan_json)
}

/// `GET /plans/:code` — a plan with its charges (and their filters).
pub async fn show_plan(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    Ok(Json(json!({ "plan": plan_with_charges(&st, &plan).await? })))
}

/// `min_amount_cents` of a charge payload: a non-negative integer.
fn min_amount_of(cp: &Value) -> ApiResult<Option<i64>> {
    match cp.get("min_amount_cents") {
        None | Some(Value::Null) => Ok(None),
        Some(_) => match i64_of(cp, "min_amount_cents") {
            Some(v) if v >= 0 => Ok(Some(v)),
            _ => Err(ApiError::validation("min_amount_cents", "value_is_invalid")),
        },
    }
}

/// `GET /plans` — the org's plans (with charges).
pub async fn list_plans(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let plans = st.db.list_plans(auth.organization.id, pg.per_page(), pg.offset()).await?;
    let total = st.db.count_plans(auth.organization.id).await?;
    let mut out = Vec::new();
    for plan in &plans {
        out.push(plan_with_charges(&st, plan).await?);
    }
    Ok(Json(json!({ "plans": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `PUT /plans/:code` — update a plan's core fields.
pub async fn update_plan(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let existing = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let p = wrapped(&body, "plan");
    let interval = s(p, "interval").and_then(|i| interval_i32(&i));
    let plan = st
        .db
        .update_plan(
            existing.id,
            s(p, "name").as_deref(),
            s(p, "description").as_deref(),
            i64_of(p, "amount_cents"),
            s(p, "amount_currency").as_deref(),
            bool_of(p, "pay_in_advance"),
            interval,
            s(p, "invoice_display_name").as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    Ok(Json(json!({ "plan": plan_with_charges(&st, &plan).await? })))
}

/// `DELETE /plans/:code` — soft-delete a plan.
pub async fn destroy_plan(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let out = plan_with_charges(&st, &plan).await?;
    st.db.soft_delete_plan(plan.id).await?;
    Ok(Json(json!({ "plan": out })))
}

/// `POST /plans/:code/charges` — add a charge to a plan.
pub async fn create_plan_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let plan = st.db.find_plan_by_code(org, &code).await?.ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let cp = wrapped(&body, "charge");
    let metric_code = req_s(cp, "billable_metric_code")?;
    let metric = st
        .db
        .find_metric_by_code(org, &metric_code)
        .await?
        .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
    let charge_model_i = charge_model_i32_str(&req_s(cp, "charge_model")?)
        .ok_or_else(|| ApiError::validation("charge_model", "value_is_invalid"))?;
    let charge_model = ChargeModel::try_from(charge_model_i)
        .map_err(|_| ApiError::validation("charge_model", "value_is_invalid"))?;
    let filters = crate::charge_filters::parse_filters(cp)?;
    let min_amount_cents = min_amount_of(cp)?.unwrap_or(0);
    let charge = Charge {
        id: Uuid::new_v4(),
        organization_id: org,
        billable_metric_id: Some(metric.id),
        plan_id: Some(plan.id),
        code: metric.code.clone(),
        charge_model,
        properties: cp.get("properties").cloned().unwrap_or_else(|| json!({})),
        pay_in_advance: bool_of(cp, "pay_in_advance").unwrap_or(false),
        prorated: bool_of(cp, "prorated").unwrap_or(false),
        invoiceable: bool_of(cp, "invoiceable").unwrap_or(true),
        min_amount_cents,
        amount_currency: Some(plan.amount_currency.clone()),
        accepts_target_wallet: false,
        invoice_display_name: s(cp, "invoice_display_name"),
        parent_id: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    st.db.insert_charge(&charge).await?;
    if let Some(filters) = filters.filter(|f| !f.is_empty()) {
        st.db.replace_charge_filters(org, charge.id, &filters).await?;
    }
    Ok(Json(json!({ "charge": crate::charge_filters::charge_json(&st, &charge).await? })))
}

/// `PUT /plans/:code/charges/:id` — update a charge.
pub async fn update_plan_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, id)): Path<(String, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let existing = st.db.find_charge_opt(id).await?.filter(|c| c.plan_id == Some(plan.id));
    if existing.is_none() {
        return Err(ApiError::NotFound("charge".into()));
    }
    let cp = wrapped(&body, "charge");
    let charge_model = match s(cp, "charge_model") {
        Some(m) => Some(charge_model_i32_str(&m).ok_or_else(|| ApiError::validation("charge_model", "value_is_invalid"))?),
        None => None,
    };
    let filters = crate::charge_filters::parse_filters(cp)?;
    let min_amount_cents = min_amount_of(cp)?;
    let props = cp.get("properties").cloned();
    let charge = st
        .db
        .update_charge(
            id,
            charge_model,
            props.as_ref(),
            bool_of(cp, "pay_in_advance"),
            bool_of(cp, "prorated"),
            bool_of(cp, "invoiceable"),
            min_amount_cents,
            s(cp, "invoice_display_name").as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("charge".into()))?;
    if let Some(filters) = filters {
        st.db.replace_charge_filters(auth.organization.id, charge.id, &filters).await?;
    }
    Ok(Json(json!({ "charge": crate::charge_filters::charge_json(&st, &charge).await? })))
}

/// `DELETE /plans/:code/charges/:id` — soft-delete a charge.
pub async fn destroy_plan_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let _ = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let charge = st.db.find_charge_opt(id).await?.ok_or_else(|| ApiError::NotFound("charge".into()))?;
    let out = ser::charge(&charge);
    st.db.soft_delete_charge(id).await?;
    Ok(Json(json!({ "charge": out })))
}

// ---- billable metrics: index / show / update / destroy ---------------------

/// `GET /billable_metrics`.
pub async fn list_billable_metrics(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let metrics = st.db.list_billable_metrics(auth.organization.id, pg.per_page(), pg.offset()).await?;
    let total = st.db.count_billable_metrics(auth.organization.id).await?;
    let out: Vec<Value> = metrics.iter().map(ser::billable_metric).collect();
    Ok(Json(json!({ "billable_metrics": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `GET /billable_metrics/:code`.
pub async fn show_billable_metric(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let m = st
        .db
        .find_metric_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
    Ok(Json(json!({ "billable_metric": ser::billable_metric(&m) })))
}

/// `PUT /billable_metrics/:code`.
pub async fn update_billable_metric(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let existing = st
        .db
        .find_metric_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
    let p = wrapped(&body, "billable_metric");
    let agg = match s(p, "aggregation_type") {
        Some(a) => Some(
            aggregation_i32(&a)
                .ok_or_else(|| ApiError::validation("aggregation_type", "value_is_invalid"))?,
        ),
        None => None,
    };
    let custom_aggregator = s(p, "custom_aggregator");
    if let Some(src) = custom_aggregator.as_deref() {
        rago_billing::aggregation::validate_custom_aggregator(src)
            .map_err(|_| ApiError::validation("custom_aggregator", "invalid_expression"))?;
    }
    let becomes_custom = agg == Some(AggregationType::CustomAgg.as_i32())
        || (agg.is_none() && matches!(existing.aggregation_type, AggregationType::CustomAgg));
    if becomes_custom
        && custom_aggregator.is_none()
        && existing.custom_aggregator.as_deref().map_or(true, |c| c.trim().is_empty())
    {
        return Err(ApiError::validation("custom_aggregator", "value_is_mandatory"));
    }
    let m = st
        .db
        .update_billable_metric(
            existing.id,
            s(p, "name").as_deref(),
            s(p, "description").as_deref(),
            agg,
            s(p, "field_name").as_deref(),
            bool_of(p, "recurring"),
            s(p, "expression").as_deref(),
            custom_aggregator.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
    Ok(Json(json!({ "billable_metric": ser::billable_metric(&m) })))
}

/// `DELETE /billable_metrics/:code`.
pub async fn destroy_billable_metric(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let m = st
        .db
        .find_metric_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
    let out = ser::billable_metric(&m);
    st.db.soft_delete_billable_metric(m.id).await?;
    Ok(Json(json!({ "billable_metric": out })))
}

/// `POST /billable_metrics/evaluate_expression` — evaluate a custom-aggregation
/// expression against a sample event (`BillableMetrics::EvaluateExpressionService`)
/// with the ported Lago expression engine (`rago_billing::expression`).
///
/// Body: `{ expression, event: { code, timestamp, properties } }`.
pub async fn evaluate_expression(
    State(_st): State<AppState>,
    _auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    use rago_billing::expression::{ExprError, Expression};
    let p = wrapped(&body, "billable_metric");
    let expression = req_s(p, "expression").or_else(|_| req_s(&body, "expression"))?;
    let event = p
        .get("event")
        .or_else(|| body.get("event"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let parsed = Expression::parse(&expression)
        .map_err(|e| ApiError::Validation(json!({ "expression": [e.to_string()] })))?;
    match parsed.evaluate(&event) {
        Ok(v) => Ok(Json(json!({ "expression_result": { "value": v.to_string() } }))),
        Err(ExprError::MissingVariable(name)) => {
            Err(ApiError::Validation(json!({ "event": [format!("missing_property: {name}")] })))
        }
        Err(e) => Err(ApiError::Validation(json!({ "expression": [e.to_string()] }))),
    }
}

// ---- add-ons: index / show / update / destroy ------------------------------

fn add_on_json(id: Uuid, name: &str, code: &str, description: Option<&str>, amount_cents: i64, currency: &str) -> Value {
    json!({
        "lago_id": id, "name": name, "code": code,
        "description": description, "amount_cents": amount_cents, "amount_currency": currency,
    })
}

/// `GET /add_ons`.
pub async fn list_add_ons(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let (items, total) = st.db.list_add_ons(auth.organization.id, pg.per_page(), pg.offset()).await?;
    let out: Vec<Value> = items
        .iter()
        .map(|(id, name, code, amount_cents, currency)| add_on_json(*id, name, code, None, *amount_cents, currency))
        .collect();
    Ok(Json(json!({ "add_ons": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `GET /add_ons/:code`.
pub async fn show_add_on(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let (id, name, code, amount_cents, currency) = st
        .db
        .find_add_on_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
    Ok(Json(json!({ "add_on": add_on_json(id, &name, &code, None, amount_cents, &currency) })))
}

/// `PUT /add_ons/:code`.
pub async fn update_add_on(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let (id, ..) = st
        .db
        .find_add_on_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
    let p = wrapped(&body, "add_on");
    let (id, name, code, description, amount_cents, currency) = st
        .db
        .update_add_on(id, s(p, "name").as_deref(), s(p, "description").as_deref(), i64_of(p, "amount_cents"))
        .await?
        .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
    Ok(Json(json!({ "add_on": add_on_json(id, &name, &code, description.as_deref(), amount_cents, &currency) })))
}

/// `DELETE /add_ons/:code`.
pub async fn destroy_add_on(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let (id, name, code, amount_cents, currency) = st
        .db
        .find_add_on_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
    let out = add_on_json(id, &name, &code, None, amount_cents, &currency);
    st.db.soft_delete_add_on(id).await?;
    Ok(Json(json!({ "add_on": out })))
}

// ---- coupons: index / show / update / destroy ------------------------------

/// `GET /coupons`.
pub async fn list_coupons(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let coupons = st.db.list_coupons(auth.organization.id, pg.per_page(), pg.offset()).await?;
    let total = st.db.count_coupons(auth.organization.id).await?;
    let out: Vec<Value> = coupons.iter().map(ser::coupon).collect();
    Ok(Json(json!({ "coupons": out, "meta": pagination::meta(pg.page(), pg.per_page(), total) })))
}

/// `GET /coupons/:code`.
pub async fn show_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = st
        .db
        .find_coupon_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("coupon".into()))?;
    Ok(Json(json!({ "coupon": ser::coupon(&c) })))
}

/// `PUT /coupons/:code`.
pub async fn update_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let existing = st
        .db
        .find_coupon_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("coupon".into()))?;
    let p = wrapped(&body, "coupon");
    let frequency = s(p, "frequency").and_then(|f| coupon_frequency_i32(&f));
    let c = st
        .db
        .update_coupon(
            existing.id,
            s(p, "name").as_deref(),
            s(p, "code").as_deref(),
            i64_of(p, "amount_cents"),
            dec_of(p, "percentage_rate"),
            frequency,
            i64_of(p, "frequency_duration").map(|v| v as i32),
            bool_of(p, "reusable"),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("coupon".into()))?;
    Ok(Json(json!({ "coupon": ser::coupon(&c) })))
}

/// `DELETE /coupons/:code`.
pub async fn destroy_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = st
        .db
        .find_coupon_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("coupon".into()))?;
    let out = ser::coupon(&c);
    st.db.soft_delete_coupon(c.id).await?;
    Ok(Json(json!({ "coupon": out })))
}

// ---- wallets: index / update / terminate -----------------------------------

/// `GET /wallets?external_customer_id=` — a customer's wallets.
#[derive(Debug, serde::Deserialize)]
pub struct WalletIndexQuery {
    pub external_customer_id: String,
}

pub async fn list_wallets(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<WalletIndexQuery>,
) -> ApiResult<Json<Value>> {
    let c = customer_or_404(&st, auth.organization.id, &q.external_customer_id).await?;
    let wallets = st.db.wallets_for_customer(c.id).await?;
    let out: Vec<Value> = wallets.iter().map(|w| ser::wallet(w, &q.external_customer_id)).collect();
    Ok(Json(json!({ "wallets": out })))
}

/// `PUT /wallets/:id` — update wallet name / expiration / priority.
pub async fn update_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let _ = auth;
    let p = wrapped(&body, "wallet");
    let expiration_at = s(p, "expiration_at")
        .and_then(|e| chrono::DateTime::parse_from_rfc3339(&e).ok())
        .map(|d| d.naive_utc());
    let w = st
        .db
        .update_wallet_details(id, s(p, "name").as_deref(), expiration_at, i64_of(p, "priority").map(|v| v as i32))
        .await?
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let ext = st.db.customer_external_id(w.customer_id).await?.unwrap_or_default();
    crate::emit::webhook(
        &st.db,
        w.organization_id,
        rago_integrations::webhooks::WebhookKind::WalletUpdated,
        ser::wallet(&w, &ext),
    )
    .await;
    Ok(Json(json!({ "wallet": ser::wallet(&w, &ext) })))
}

/// `DELETE /wallets/:id` — terminate a wallet.
pub async fn terminate_wallet(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let w = st.db.find_wallet(id).await?.ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    st.db.terminate_wallet(id).await?;
    let updated = st.db.find_wallet(id).await?.ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let ext = st.db.customer_external_id(w.customer_id).await?.unwrap_or_default();
    Ok(Json(json!({ "wallet": ser::wallet(&updated, &ext) })))
}

// ---- enum string → i32 helpers ---------------------------------------------

fn interval_i32(s: &str) -> Option<i32> {
    Some(match s {
        "weekly" => PlanInterval::Weekly,
        "monthly" => PlanInterval::Monthly,
        "yearly" => PlanInterval::Yearly,
        "quarterly" => PlanInterval::Quarterly,
        "semiannual" => PlanInterval::Semiannual,
        _ => return None,
    }
    .as_i32())
}
fn charge_model_i32_str(s: &str) -> Option<i32> {
    Some(match s {
        "standard" => ChargeModel::Standard,
        "graduated" => ChargeModel::Graduated,
        "package" => ChargeModel::Package,
        "percentage" => ChargeModel::Percentage,
        "volume" => ChargeModel::Volume,
        "graduated_percentage" => ChargeModel::GraduatedPercentage,
        "custom" => ChargeModel::Custom,
        "dynamic" => ChargeModel::Dynamic,
        _ => return None,
    }
    .as_i32())
}
fn aggregation_i32(s: &str) -> Option<i32> {
    Some(match s {
        "count_agg" => AggregationType::CountAgg,
        "sum_agg" => AggregationType::SumAgg,
        "max_agg" => AggregationType::MaxAgg,
        "unique_count_agg" => AggregationType::UniqueCountAgg,
        "latest_agg" => AggregationType::LatestAgg,
        "weighted_sum_agg" => AggregationType::WeightedSumAgg,
        "custom_agg" => AggregationType::CustomAgg,
        _ => return None,
    }
    .as_i32())
}
fn coupon_frequency_i32(s: &str) -> Option<i32> {
    Some(match s {
        "once" => CouponFrequency::Once,
        "recurring" => CouponFrequency::Recurring,
        "forever" => CouponFrequency::Forever,
        _ => return None,
    }
    .as_i32())
}

// Keep Decimal + Datelike imports meaningful across edits.
#[allow(dead_code)]
fn _touch(d: Decimal, n: chrono::NaiveDate) -> (Decimal, u32) {
    (d, n.month())
}
