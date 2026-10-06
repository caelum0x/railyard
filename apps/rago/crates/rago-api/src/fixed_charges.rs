//! `/plans/:code/fixed_charges` — Lago `Api::V1::Plans::FixedChargesController`.
//!
//! A fixed charge bills a number of units of an add-on every period (seats, a
//! platform fee), see `rago_billing::fixed_charges`. Charge models: standard,
//! graduated and volume (Lago's fixed-charge models).

use axum::extract::{Path, State};
use axum::Json;
use rago_core::enums::ChargeModel;
use rago_db::fixed_charges_rest::{FixedChargeChanges, FixedChargeRecord, NewFixedCharge};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{bool_of, dec_of, s, wrapped};
use crate::state::AppState;

/// `V1::FixedChargeSerializer`.
pub(crate) fn fixed_charge_json(fc: &FixedChargeRecord) -> Value {
    json!({
        "lago_id": fc.id,
        "lago_add_on_id": fc.add_on_id,
        "code": fc.code,
        "invoice_display_name": fc.invoice_display_name,
        "add_on_code": fc.add_on_code,
        "created_at": fc.created_at.and_utc().to_rfc3339(),
        "charge_model": ChargeModel::try_from(fc.charge_model).map(|m| m.as_str()).unwrap_or("standard"),
        "pay_in_advance": fc.pay_in_advance,
        "prorated": fc.prorated,
        "properties": fc.properties,
        "units": fc.units.to_string(),
        "lago_parent_id": fc.parent_id,
    })
}

fn charge_model(raw: &str) -> ApiResult<i32> {
    match raw {
        "standard" => Ok(ChargeModel::Standard.as_i32()),
        "graduated" => Ok(ChargeModel::Graduated.as_i32()),
        "volume" => Ok(ChargeModel::Volume.as_i32()),
        _ => Err(ApiError::validation("charge_model", "value_is_invalid")),
    }
}

fn units(p: &Value) -> ApiResult<Option<Decimal>> {
    let Some(raw) = p.get("units").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let units = dec_of(p, "units")
        .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| ApiError::validation("units", "value_is_invalid"))?;
    if units < Decimal::ZERO {
        return Err(ApiError::validation("units", "value_is_out_of_range"));
    }
    Ok(Some(units))
}

fn properties(p: &Value) -> ApiResult<Option<Value>> {
    match p.get("properties") {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
        Some(_) => Err(ApiError::validation("properties", "value_is_invalid")),
    }
}

async fn plan_id(st: &AppState, org: Uuid, code: &str) -> ApiResult<Uuid> {
    Ok(st
        .db
        .find_plan_by_code(org, code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?
        .id)
}

/// `GET /plans/:code/fixed_charges`.
pub async fn index(State(st): State<AppState>, auth: Authed, Path(code): Path<String>) -> ApiResult<Json<Value>> {
    let plan = plan_id(&st, auth.organization.id, &code).await?;
    let list = st.db.plan_fixed_charges(plan).await?;
    let total = list.len();
    Ok(Json(json!({
        "fixed_charges": list.iter().map(fixed_charge_json).collect::<Vec<_>>(),
        "meta": crate::pagination::meta(1, total.max(1) as i64, total as i64),
    })))
}

/// `GET /plans/:code/fixed_charges/:fixed_charge_code`.
pub async fn show(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, fc_code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let plan = plan_id(&st, auth.organization.id, &code).await?;
    let fc = st
        .db
        .plan_fixed_charge(plan, &fc_code)
        .await?
        .ok_or_else(|| ApiError::NotFound("fixed_charge".into()))?;
    Ok(Json(json!({ "fixed_charge": fixed_charge_json(&fc) })))
}

/// `POST /plans/:code/fixed_charges` — `{ fixed_charge: { add_on_code | add_on_id,
/// code?, charge_model, units, pay_in_advance, prorated, invoice_display_name,
/// properties } }`.
pub async fn create(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let plan = plan_id(&st, org, &code).await?;
    let p = wrapped(&body, "fixed_charge");
    let add_on = match (s(p, "add_on_code"), s(p, "add_on_id")) {
        (Some(c), _) => st.db.find_add_on_by_code(org, &c).await?.map(|a| (a.0, a.2)),
        (None, Some(id)) => {
            let id: Uuid = id
                .parse()
                .map_err(|_| ApiError::validation("add_on_id", "value_is_invalid"))?;
            st.db.find_add_on(id).await?.map(|a| (a.0, a.2))
        }
        (None, None) => return Err(ApiError::validation("add_on", "value_is_mandatory")),
    }
    .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
    let new = NewFixedCharge {
        code: s(p, "code")
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| add_on.1.clone()),
        add_on_id: Some(add_on.0),
        charge_model: charge_model(&s(p, "charge_model").unwrap_or_else(|| "standard".into()))?,
        properties: properties(p)?.unwrap_or_else(|| json!({})),
        units: units(p)?.unwrap_or(Decimal::ZERO),
        pay_in_advance: bool_of(p, "pay_in_advance").unwrap_or(false),
        prorated: bool_of(p, "prorated").unwrap_or(false),
        invoice_display_name: s(p, "invoice_display_name"),
    };
    let fc = st
        .db
        .create_plan_fixed_charge(org, plan, &new)
        .await?
        .ok_or_else(|| ApiError::validation("code", "value_already_exist"))?;
    Ok(Json(json!({ "fixed_charge": fixed_charge_json(&fc) })))
}

/// `PUT /plans/:code/fixed_charges/:fixed_charge_code`.
pub async fn update(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, fc_code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let plan = plan_id(&st, auth.organization.id, &code).await?;
    let p = wrapped(&body, "fixed_charge");
    let changes = FixedChargeChanges {
        charge_model: s(p, "charge_model").map(|m| charge_model(&m)).transpose()?,
        properties: properties(p)?,
        units: units(p)?,
        pay_in_advance: bool_of(p, "pay_in_advance"),
        prorated: bool_of(p, "prorated"),
        invoice_display_name: s(p, "invoice_display_name"),
    };
    let fc = st
        .db
        .update_plan_fixed_charge(plan, &fc_code, &changes)
        .await?
        .ok_or_else(|| ApiError::NotFound("fixed_charge".into()))?;
    Ok(Json(json!({ "fixed_charge": fixed_charge_json(&fc) })))
}

/// `DELETE /plans/:code/fixed_charges/:fixed_charge_code`.
pub async fn destroy(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, fc_code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let plan = plan_id(&st, auth.organization.id, &code).await?;
    let fc = st
        .db
        .delete_plan_fixed_charge(plan, &fc_code)
        .await?
        .ok_or_else(|| ApiError::NotFound("fixed_charge".into()))?;
    Ok(Json(json!({ "fixed_charge": fixed_charge_json(&fc) })))
}
