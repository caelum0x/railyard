//! REST v2 catalog — products, product categories, rate cards (+ rates), and
//! plan-applied rate cards with rate phases. These tables (migrations 0024/0025)
//! already have full [`rago_db::Db`] CRUD ops (added by the GraphQL admin
//! slices); this module wires the REST surface over them.

use axum::extract::{Path, State};
use axum::Json;
use rago_core::enums::ChargeModel;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{bool_of, req_s, s, wrapped};
use crate::state::AppState;

// ---- products --------------------------------------------------------------

fn product_json(r: &rago_db::ProductRow) -> Value {
    json!({
        "lago_id": r.id,
        "code": r.code,
        "name": r.name,
        "description": r.description,
        "invoice_display_name": r.invoice_display_name,
        "product_type": r.product_type,
        "lago_billable_metric_id": r.billable_metric_id,
        "lago_product_category_id": r.product_category_id,
    })
}

/// `POST /api/v2/products`.
pub async fn create_product(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "product");
    let code = req_s(p, "code")?;
    let name = req_s(p, "name")?;
    let metric_id = match s(p, "billable_metric_code") {
        Some(mc) => st.db.find_metric_by_code(org, &mc).await?.map(|m| m.id),
        None => None,
    };
    let category_id = s(p, "product_category_id").and_then(|v| Uuid::parse_str(&v).ok());
    let row = st
        .db
        .insert_product(
            Uuid::new_v4(),
            org,
            &code,
            &name,
            s(p, "description").as_deref(),
            s(p, "invoice_display_name").as_deref(),
            s(p, "product_type").as_deref(),
            metric_id,
            category_id,
        )
        .await?;
    Ok(Json(json!({ "product": product_json(&row) })))
}

/// `GET /api/v2/products`.
pub async fn list_products(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_products(auth.organization.id).await?;
    Ok(Json(json!({ "products": rows.iter().map(product_json).collect::<Vec<_>>() })))
}

/// `GET /api/v2/products/:id`.
pub async fn show_product(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st.db.find_product(id).await?.ok_or_else(|| ApiError::NotFound("product".into()))?;
    Ok(Json(json!({ "product": product_json(&row) })))
}

/// `PUT /api/v2/products/:id`.
pub async fn update_product(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "product");
    let category_id = s(p, "product_category_id").and_then(|v| Uuid::parse_str(&v).ok());
    let row = st
        .db
        .update_product(id, s(p, "name").as_deref(), s(p, "description").as_deref(), s(p, "invoice_display_name").as_deref(), category_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("product".into()))?;
    Ok(Json(json!({ "product": product_json(&row) })))
}

/// `DELETE /api/v2/products/:id`.
pub async fn destroy_product(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st.db.find_product(id).await?.ok_or_else(|| ApiError::NotFound("product".into()))?;
    let out = product_json(&row);
    st.db.soft_delete_product(id).await?;
    Ok(Json(json!({ "product": out })))
}

// ---- product categories ----------------------------------------------------

fn product_category_json(r: &rago_db::ProductCategoryRow) -> Value {
    // ProductCategoryRow = (id, code, name, description, invoice_display_name).
    json!({
        "lago_id": r.0,
        "code": r.1,
        "name": r.2,
        "description": r.3,
        "invoice_display_name": r.4,
    })
}

/// `POST /api/v2/product_categories`.
pub async fn create_product_category(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "product_category");
    let code = req_s(p, "code")?;
    let name = req_s(p, "name")?;
    let row = st
        .db
        .insert_product_category(Uuid::new_v4(), auth.organization.id, &code, &name, s(p, "description").as_deref(), s(p, "invoice_display_name").as_deref())
        .await?;
    Ok(Json(json!({ "product_category": product_category_json(&row) })))
}

/// `GET /api/v2/product_categories`.
pub async fn list_product_categories(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_product_categories(auth.organization.id).await?;
    Ok(Json(json!({ "product_categories": rows.iter().map(product_category_json).collect::<Vec<_>>() })))
}

// ---- rate cards ------------------------------------------------------------

fn rate_card_json(r: &rago_db::RateCardRow) -> Value {
    json!({
        "lago_id": r.id,
        "lago_product_id": r.product_id,
        "code": r.code,
        "name": r.name,
        "description": r.description,
        "currency": r.currency,
        "billing_timing": r.billing_timing,
        "proration": r.proration,
        "display_on_invoice": r.display_on_invoice,
        "wallet_targetable": r.wallet_targetable,
    })
}

/// `POST /api/v2/rate_cards`.
pub async fn create_rate_card(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "rate_card");
    let product_id = Uuid::parse_str(&req_s(p, "product_id")?)
        .map_err(|_| ApiError::validation("product_id", "value_is_invalid"))?;
    let code = req_s(p, "code")?;
    let name = req_s(p, "name")?;
    let currency = s(p, "currency").unwrap_or_else(|| "USD".into());
    let row = st
        .db
        .insert_rate_card(
            Uuid::new_v4(),
            auth.organization.id,
            product_id,
            &code,
            &name,
            s(p, "description").as_deref(),
            &currency,
            s(p, "billing_timing").as_deref(),
            bool_of(p, "proration").unwrap_or(false),
            bool_of(p, "display_on_invoice").unwrap_or(true),
            bool_of(p, "wallet_targetable").unwrap_or(false),
        )
        .await?;
    Ok(Json(json!({ "rate_card": rate_card_json(&row) })))
}

/// `GET /api/v2/rate_cards`.
pub async fn list_rate_cards(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_rate_cards(auth.organization.id).await?;
    Ok(Json(json!({ "rate_cards": rows.iter().map(rate_card_json).collect::<Vec<_>>() })))
}

/// `GET /api/v2/rate_cards/:id`.
pub async fn show_rate_card(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st.db.find_rate_card(id).await?.ok_or_else(|| ApiError::NotFound("rate_card".into()))?;
    let rates = st.db.list_rate_card_rates(id).await?;
    let mut out = rate_card_json(&row);
    out["rates"] = json!(rates.iter().map(rate_card_rate_json).collect::<Vec<_>>());
    Ok(Json(json!({ "rate_card": out })))
}

/// `PUT /api/v2/rate_cards/:id`.
pub async fn update_rate_card(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "rate_card");
    let row = st
        .db
        .update_rate_card(
            id,
            s(p, "name").as_deref(),
            s(p, "description").as_deref(),
            bool_of(p, "proration"),
            bool_of(p, "display_on_invoice"),
            bool_of(p, "wallet_targetable"),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("rate_card".into()))?;
    Ok(Json(json!({ "rate_card": rate_card_json(&row) })))
}

/// `DELETE /api/v2/rate_cards/:id`.
pub async fn destroy_rate_card(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st.db.find_rate_card(id).await?.ok_or_else(|| ApiError::NotFound("rate_card".into()))?;
    let out = rate_card_json(&row);
    st.db.soft_delete_rate_card(id).await?;
    Ok(Json(json!({ "rate_card": out })))
}

fn rate_card_rate_json(r: &rago_db::RateCardRateRow) -> Value {
    // RateCardRateRow = (id, rate_card_id, charge_model, properties).
    json!({
        "lago_id": r.0,
        "lago_rate_card_id": r.1,
        "charge_model": charge_model_str(r.2),
        "properties": r.3,
    })
}

/// `POST /api/v2/rate_cards/:id/rates`.
pub async fn create_rate_card_rate(
    State(st): State<AppState>,
    auth: Authed,
    Path(rate_card_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let _ = st.db.find_rate_card(rate_card_id).await?.ok_or_else(|| ApiError::NotFound("rate_card".into()))?;
    let p = wrapped(&body, "rate");
    let charge_model = charge_model_i32(&req_s(p, "charge_model")?)
        .ok_or_else(|| ApiError::validation("charge_model", "value_is_invalid"))?;
    let properties = p.get("properties").cloned().unwrap_or_else(|| json!({}));
    let row = st
        .db
        .insert_rate_card_rate(Uuid::new_v4(), auth.organization.id, rate_card_id, charge_model, &properties)
        .await?;
    Ok(Json(json!({ "rate": rate_card_rate_json(&row) })))
}

/// `GET /api/v2/rate_cards/:id/rates`.
pub async fn list_rate_card_rates(
    State(st): State<AppState>,
    _auth: Authed,
    Path(rate_card_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.list_rate_card_rates(rate_card_id).await?;
    Ok(Json(json!({ "rates": rows.iter().map(rate_card_rate_json).collect::<Vec<_>>() })))
}

// ---- plan applied rate cards + rate phases ---------------------------------

fn applied_rate_card_json(r: &rago_db::PlanAppliedRateCardRow) -> Value {
    // PlanAppliedRateCardRow = (id, plan_id, rate_card_id).
    json!({ "lago_id": r.0, "lago_plan_id": r.1, "lago_rate_card_id": r.2 })
}

/// `POST /api/v2/plans/:code/applied_rate_cards`.
pub async fn create_applied_rate_card(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let plan = st.db.find_plan_by_code(org, &code).await?.ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let p = wrapped(&body, "applied_rate_card");
    let rate_card_id = Uuid::parse_str(&req_s(p, "rate_card_id")?)
        .map_err(|_| ApiError::validation("rate_card_id", "value_is_invalid"))?;
    let row = st.db.insert_plan_applied_rate_card(Uuid::new_v4(), org, plan.id, rate_card_id).await?;
    Ok(Json(json!({ "applied_rate_card": applied_rate_card_json(&row) })))
}

/// `GET /api/v2/plans/:code/applied_rate_cards`.
pub async fn list_applied_rate_cards(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st.db.find_plan_by_code(auth.organization.id, &code).await?.ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let rows = st.db.list_plan_applied_rate_cards(plan.id).await?;
    Ok(Json(json!({ "applied_rate_cards": rows.iter().map(applied_rate_card_json).collect::<Vec<_>>() })))
}

fn rate_phase_json(r: &rago_db::RatePhaseRow) -> Value {
    json!({
        "lago_id": r.id,
        "lago_applied_rate_card_id": r.plan_applied_rate_card_id,
        "code": r.code,
        "name": r.name,
        "position": r.position,
        "billing_interval_cycle_count": r.billing_interval_cycle_count,
    })
}

/// `POST /api/v2/applied_rate_cards/:id/rate_phases`.
pub async fn create_rate_phase(
    State(st): State<AppState>,
    auth: Authed,
    Path(applied_rate_card_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "rate_phase");
    let code = req_s(p, "code")?;
    let position = p.get("position").and_then(|v| v.as_i64()).map(|v| v as i32);
    let cycle_count = p.get("billing_interval_cycle_count").and_then(|v| v.as_i64()).map(|v| v as i32);
    let row = st
        .db
        .insert_rate_phase(Uuid::new_v4(), auth.organization.id, applied_rate_card_id, &code, s(p, "name").as_deref(), position, cycle_count)
        .await?;
    Ok(Json(json!({ "rate_phase": rate_phase_json(&row) })))
}

/// `GET /api/v2/applied_rate_cards/:id/rate_phases`.
pub async fn list_rate_phases(
    State(st): State<AppState>,
    _auth: Authed,
    Path(applied_rate_card_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.list_rate_phases(applied_rate_card_id).await?;
    Ok(Json(json!({ "rate_phases": rows.iter().map(rate_phase_json).collect::<Vec<_>>() })))
}

// ---- charge-model <-> i32 --------------------------------------------------

fn charge_model_i32(s: &str) -> Option<i32> {
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
fn charge_model_str(v: i32) -> String {
    ChargeModel::try_from(v).map(|c| c.as_str().to_string()).unwrap_or_else(|_| "standard".into())
}
