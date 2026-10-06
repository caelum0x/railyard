//! Lago REST parity for plan charges, subscription overrides and entitlements:
//!
//! * `/plans/:code/charges/:charge_code/filters[/:id]` — a plan charge's
//!   filters one by one;
//! * `/subscriptions/:id/charges[/:code[/filters[/:id]]]` and
//!   `/subscriptions/:id/fixed_charges[/:code]` — read the subscription's
//!   charges; a change makes the subscription's own copy of its plan first
//!   (Lago `Plans::OverrideService`), so the plan and its other subscribers
//!   are untouched;
//! * `/plans/:code/entitlements[/:feature]` (show, create = replace all,
//!   PATCH = merge, destroy, privilege destroy), the same on subscriptions
//!   (a plan feature removed for one subscription is remembered), and
//!   `DELETE /features/:code/privileges/:code`.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::Json;
use rago_core::models::{Charge, Plan, Subscription};
use rago_db::rest_parity::{EntitlementLine, EntitlementScope};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{s, wrapped};
use crate::state::AppState;

// ---- lookups ------------------------------------------------------------------

async fn plan(st: &AppState, org: Uuid, code: &str) -> ApiResult<Plan> {
    st.db
        .find_plan_by_code(org, code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))
}

async fn subscription(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Subscription> {
    st.db
        .find_subscription_by_external_id(org, external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))
}

/// A live charge of a plan by code (or id).
async fn charge_in(st: &AppState, plan_id: Uuid, key: &str) -> ApiResult<Charge> {
    let id = key.parse::<Uuid>().ok();
    st.db
        .charges_for_plan(plan_id)
        .await?
        .into_iter()
        .find(|c| c.code == key || Some(c.id) == id)
        .ok_or_else(|| ApiError::NotFound("charge".into()))
}

fn filter_spec(body: &Value) -> ApiResult<rago_db::charge_filters::ChargeFilterSpec> {
    let f = wrapped(body, "filter");
    let mut specs = crate::charge_filters::parse_filters(&json!({ "filters": [f] }))?.unwrap_or_default();
    specs
        .pop()
        .ok_or_else(|| ApiError::validation("filter", "value_is_invalid"))
}

async fn filter_out(st: &AppState, charge: Uuid, id: Uuid) -> ApiResult<Value> {
    let rows = st.db.charge_filter_rows(charge).await?;
    let row = rows
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| ApiError::NotFound("charge_filter".into()))?;
    Ok(crate::charge_filters::filters_json(std::slice::from_ref(row))[0].clone())
}

// ---- charge filters (plan and subscription) ----------------------------------

async fn list_filters(st: &AppState, charge: Uuid) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({ "filters": crate::charge_filters::filters_json(&st.db.charge_filter_rows(charge).await?) }),
    ))
}

async fn create_filter(st: &AppState, org: Uuid, charge: Uuid, body: &Value) -> ApiResult<Json<Value>> {
    let spec = filter_spec(body)?;
    let id = st.db.add_charge_filter(org, charge, &spec).await?;
    Ok(Json(json!({ "filter": filter_out(st, charge, id).await? })))
}

async fn update_filter(st: &AppState, charge: Uuid, id: Uuid, body: &Value) -> ApiResult<Json<Value>> {
    let f = wrapped(body, "filter");
    let values = match f.get("values") {
        Some(_) => Some(filter_spec(body)?.values),
        None => None,
    };
    let properties = match f.get("properties") {
        None | Some(Value::Null) => None,
        Some(p @ Value::Object(_)) => Some(p.clone()),
        Some(_) => return Err(ApiError::validation("filter", "properties_must_be_an_object")),
    };
    let name = s(f, "invoice_display_name");
    if !st
        .db
        .change_charge_filter(charge, id, name.as_deref(), properties.as_ref(), values.as_ref())
        .await?
    {
        return Err(ApiError::NotFound("charge_filter".into()));
    }
    Ok(Json(json!({ "filter": filter_out(st, charge, id).await? })))
}

async fn destroy_filter(st: &AppState, charge: Uuid, id: Uuid) -> ApiResult<Json<Value>> {
    let out = filter_out(st, charge, id).await?;
    st.db.remove_charge_filter(charge, id).await?;
    Ok(Json(json!({ "filter": out })))
}

pub async fn plan_charge_filters(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, charge)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    list_filters(&st, charge_in(&st, p.id, &charge).await?.id).await
}
pub async fn create_plan_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, charge)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    let c = charge_in(&st, p.id, &charge).await?;
    create_filter(&st, auth.organization.id, c.id, &body).await
}
pub async fn show_plan_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, charge, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    let c = charge_in(&st, p.id, &charge).await?;
    Ok(Json(json!({ "filter": filter_out(&st, c.id, id).await? })))
}
pub async fn update_plan_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, charge, id)): Path<(String, String, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    let c = charge_in(&st, p.id, &charge).await?;
    update_filter(&st, c.id, id, &body).await
}
pub async fn destroy_plan_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, charge, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    let c = charge_in(&st, p.id, &charge).await?;
    destroy_filter(&st, c.id, id).await
}

// ---- subscription charges (overrides) ------------------------------------------

/// The subscription's charge by code, after making the override plan when
/// `write` (the override's charges are copies with the same codes).
async fn sub_charge(st: &AppState, sub: &Subscription, key: &str, write: bool) -> ApiResult<Charge> {
    let plan_id = if write {
        st.db.ensure_override_plan(sub.id).await?
    } else {
        sub.plan_id
    };
    charge_in(st, plan_id, key).await
}

async fn sub_charge_json(st: &AppState, c: &Charge) -> ApiResult<Value> {
    let mut v = crate::charge_filters::charge_json(st, c).await?;
    v["lago_parent_id"] = json!(c.parent_id);
    Ok(v)
}

pub async fn subscription_charges(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let mut out = Vec::new();
    for c in st.db.charges_for_plan(sub.plan_id).await? {
        out.push(sub_charge_json(&st, &c).await?);
    }
    Ok(Json(json!({ "charges": out })))
}

pub async fn subscription_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let c = sub_charge(&st, &sub, &code, false).await?;
    Ok(Json(json!({ "charge": sub_charge_json(&st, &c).await? })))
}

/// `PUT /subscriptions/:id/charges/:code` — Lago
/// `Subscriptions::UpdateOrOverrideChargeService`: invoice_display_name,
/// min_amount_cents, properties and filters of this subscription only.
pub async fn update_subscription_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = subscription(&st, org, &external_id).await?;
    let cp = wrapped(&body, "charge");
    let filters = crate::charge_filters::parse_filters(cp)?;
    let min = crate::crud::min_amount_of(cp)?;
    let props = match cp.get("properties") {
        None | Some(Value::Null) => None,
        Some(p @ Value::Object(_)) => Some(p.clone()),
        Some(_) => return Err(ApiError::validation("properties", "value_is_invalid")),
    };
    let c = sub_charge(&st, &sub, &code, true).await?;
    let updated = st
        .db
        .update_charge(
            c.id,
            None,
            props.as_ref(),
            None,
            None,
            None,
            min,
            s(cp, "invoice_display_name").as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("charge".into()))?;
    if let Some(f) = filters {
        st.db.replace_charge_filters(org, updated.id, &f).await?;
    }
    Ok(Json(json!({ "charge": sub_charge_json(&st, &updated).await? })))
}

pub async fn subscription_charge_filters(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    list_filters(&st, sub_charge(&st, &sub, &code, false).await?.id).await
}
pub async fn create_subscription_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let c = sub_charge(&st, &sub, &code, true).await?;
    create_filter(&st, auth.organization.id, c.id, &body).await
}
pub async fn show_subscription_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let c = sub_charge(&st, &sub, &code, false).await?;
    Ok(Json(json!({ "filter": filter_out(&st, c.id, id).await? })))
}

/// A filter id of the plan maps to its copy on the override (same values).
async fn override_filter_id(st: &AppState, sub: &Subscription, code: &str, id: Uuid) -> ApiResult<(Uuid, Uuid)> {
    let before = sub_charge(st, sub, code, false).await?;
    let original = st
        .db
        .charge_filter_rows(before.id)
        .await?
        .into_iter()
        .find(|r| r.id == id);
    let c = sub_charge(st, sub, code, true).await?;
    if c.id == before.id {
        return Ok((c.id, id));
    }
    let original = original.ok_or_else(|| ApiError::NotFound("charge_filter".into()))?;
    let copy = st
        .db
        .charge_filter_rows(c.id)
        .await?
        .into_iter()
        .find(|r| r.spec.values == original.spec.values)
        .ok_or_else(|| ApiError::NotFound("charge_filter".into()))?;
    Ok((c.id, copy.id))
}

pub async fn update_subscription_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, id)): Path<(String, String, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let (charge, fid) = override_filter_id(&st, &sub, &code, id).await?;
    update_filter(&st, charge, fid, &body).await
}
pub async fn destroy_subscription_charge_filter(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let (charge, fid) = override_filter_id(&st, &sub, &code, id).await?;
    destroy_filter(&st, charge, fid).await
}

// ---- subscription fixed charges ------------------------------------------------

pub async fn subscription_fixed_charges(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let list = st.db.plan_fixed_charges(sub.plan_id).await?;
    Ok(Json(
        json!({ "fixed_charges": list.iter().map(crate::fixed_charges::fixed_charge_json).collect::<Vec<_>>() }),
    ))
}

pub async fn subscription_fixed_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let fc = st
        .db
        .plan_fixed_charge(sub.plan_id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("fixed_charge".into()))?;
    Ok(Json(
        json!({ "fixed_charge": crate::fixed_charges::fixed_charge_json(&fc) }),
    ))
}

/// `PUT /subscriptions/:id/fixed_charges/:code` — units, invoice name and
/// properties for this subscription only.
pub async fn update_subscription_fixed_charge(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    let p = wrapped(&body, "fixed_charge");
    let units = match p.get("units") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let u = v
                .as_str()
                .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
                .or_else(|| v.as_f64().and_then(rust_decimal::Decimal::from_f64_retain))
                .filter(|u| !u.is_sign_negative())
                .ok_or_else(|| ApiError::validation("units", "value_is_invalid"))?;
            Some(u)
        }
    };
    let properties = match p.get("properties") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => return Err(ApiError::validation("properties", "value_is_invalid")),
    };
    if st.db.plan_fixed_charge(sub.plan_id, &code).await?.is_none() {
        return Err(ApiError::NotFound("fixed_charge".into()));
    }
    let plan_id = st.db.ensure_override_plan(sub.id).await?;
    let changes = rago_db::fixed_charges_rest::FixedChargeChanges {
        units,
        properties,
        invoice_display_name: s(p, "invoice_display_name"),
        ..Default::default()
    };
    let fc = st
        .db
        .update_plan_fixed_charge(plan_id, &code, &changes)
        .await?
        .ok_or_else(|| ApiError::NotFound("fixed_charge".into()))?;
    Ok(Json(
        json!({ "fixed_charge": crate::fixed_charges::fixed_charge_json(&fc) }),
    ))
}

// ---- entitlements ----------------------------------------------------------------

/// Group lines into Lago entitlement objects.
fn entitlements_json(lines: &[EntitlementLine]) -> Vec<Value> {
    let mut by: BTreeMap<String, (Value, Vec<Value>)> = BTreeMap::new();
    for l in lines {
        let entry = by.entry(l.feature_code.clone()).or_insert_with(|| {
            (
                json!({ "code": l.feature_code, "name": l.feature_name, "description": l.feature_description }),
                Vec::new(),
            )
        });
        if let (Some(code), Some(value)) = (&l.privilege_code, &l.value) {
            entry.1.push(json!({
                "code": code,
                "name": l.privilege_name,
                "value_type": l.value_type,
                "config": l.config,
                "value": typed(l.value_type.as_deref(), value),
            }));
        }
    }
    by.into_values()
        .map(|(mut f, privileges)| {
            f["privileges"] = json!(privileges);
            f
        })
        .collect()
}

fn typed(value_type: Option<&str>, raw: &str) -> Value {
    match value_type {
        Some("integer") => raw.parse::<i64>().map(Value::from).unwrap_or_else(|_| json!(raw)),
        Some("boolean") => json!(raw == "true"),
        _ => json!(raw),
    }
}

/// Apply `{ feature_code: { privilege_code: value } }` to a scope. Without
/// `partial` the features (and privileges) not listed are removed.
async fn apply_entitlements(
    st: &AppState,
    org: Uuid,
    scope: EntitlementScope,
    body: &Value,
    partial: bool,
) -> ApiResult<()> {
    let map = body
        .get("entitlements")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::validation("entitlements", "value_is_mandatory"))?;
    let mut kept_features = Vec::new();
    for (feature_code, values) in map {
        let feature = st
            .db
            .find_feature_by_code(org, feature_code)
            .await?
            .ok_or_else(|| ApiError::NotFound("feature".into()))?;
        let values = match values {
            Value::Null => serde_json::Map::new(),
            Value::Object(o) => o.clone(),
            _ => return Err(ApiError::validation(feature_code, "value_is_invalid")),
        };
        // Validate every value before writing any.
        let mut resolved = Vec::new();
        for (code, raw) in &values {
            let p = st
                .db
                .live_privilege(feature, code)
                .await?
                .ok_or_else(|| ApiError::NotFound("privilege".into()))?;
            let v = match raw {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !crate::handlers::valid_privilege_value(&p.value_type, &v) {
                return Err(ApiError::validation(code, "value_is_not_valid"));
            }
            resolved.push((p.id, v));
        }
        let ent = st.db.ensure_scoped_entitlement(org, scope, feature).await?;
        for (pid, v) in &resolved {
            st.db.set_entitlement_value(org, ent, *pid, v).await?;
        }
        if !partial {
            st.db
                .clear_entitlement_values_except(ent, &resolved.iter().map(|(p, _)| *p).collect::<Vec<_>>())
                .await?;
        }
        if let EntitlementScope::Subscription(sid) = scope {
            st.db.clear_feature_removals(sid, feature, None).await?;
        }
        kept_features.push(feature);
    }
    if !partial {
        st.db.clear_scope_entitlements_except(scope, &kept_features).await?;
    }
    Ok(())
}

pub async fn plan_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    Ok(Json(
        json!({ "plan_code": code, "entitlements": entitlements_json(&st.db.entitlement_lines(EntitlementScope::Plan(p.id)).await?) }),
    ))
}

pub async fn plan_entitlement(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, feature)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = plan(&st, auth.organization.id, &code).await?;
    let lines = st.db.entitlement_lines(EntitlementScope::Plan(p.id)).await?;
    let one = entitlements_json(&lines)
        .into_iter()
        .find(|e| e["code"] == feature)
        .ok_or_else(|| ApiError::NotFound("entitlement".into()))?;
    Ok(Json(json!({ "entitlement": one })))
}

/// `POST /plans/:code/entitlements` — replace the plan's entitlements.
pub async fn create_plan_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = plan(&st, org, &code).await?;
    apply_entitlements(&st, org, EntitlementScope::Plan(p.id), &body, false).await?;
    plan_entitlements(State(st), auth, Path(code)).await
}

/// `PATCH /plans/:code/entitlements` — merge into the plan's entitlements.
pub async fn patch_plan_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = plan(&st, org, &code).await?;
    apply_entitlements(&st, org, EntitlementScope::Plan(p.id), &body, true).await?;
    plan_entitlements(State(st), auth, Path(code)).await
}

pub async fn destroy_plan_entitlement(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, feature)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = plan(&st, org, &code).await?;
    let Json(out) = plan_entitlement(State(st.clone()), auth, Path((code, feature.clone()))).await?;
    let fid = st
        .db
        .find_feature_by_code(org, &feature)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    st.db
        .delete_scoped_entitlement(EntitlementScope::Plan(p.id), fid)
        .await?;
    Ok(Json(out))
}

pub async fn destroy_plan_entitlement_privilege(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, feature, privilege)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = plan(&st, org, &code).await?;
    let fid = st
        .db
        .find_feature_by_code(org, &feature)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    let priv_row = st
        .db
        .live_privilege(fid, &privilege)
        .await?
        .ok_or_else(|| ApiError::NotFound("privilege".into()))?;
    if !st
        .db
        .delete_scoped_entitlement_value(EntitlementScope::Plan(p.id), fid, priv_row.id)
        .await?
    {
        return Err(ApiError::NotFound("entitlement".into()));
    }
    plan_entitlement(State(st), auth, Path((code, feature))).await
}

/// The subscription's resolved entitlements: the plan's, minus removals,
/// overlaid with the subscription's own (Lago's subscription entitlements).
pub(crate) async fn resolved_subscription_entitlements(st: &AppState, sub: &Subscription) -> ApiResult<Vec<Value>> {
    let removals = st.db.feature_removals(sub.id).await?;
    let removed_feature = |f: &str| removals.iter().any(|(rf, rp)| rf == f && rp.is_none());
    let removed_privilege = |f: &str, p: &str| removals.iter().any(|(rf, rp)| rf == f && rp.as_deref() == Some(p));
    let mut lines: Vec<EntitlementLine> = st
        .db
        .entitlement_lines(EntitlementScope::Plan(sub.plan_id))
        .await?
        .into_iter()
        .filter(|l| !removed_feature(&l.feature_code))
        .filter(|l| {
            l.privilege_code
                .as_deref()
                .is_none_or(|p| !removed_privilege(&l.feature_code, p))
        })
        .collect();
    for own in st.db.entitlement_lines(EntitlementScope::Subscription(sub.id)).await? {
        lines.retain(|l| {
            !(l.feature_code == own.feature_code
                && (l.privilege_code == own.privilege_code || l.privilege_code.is_none()))
        });
        lines.push(own);
    }
    Ok(entitlements_json(&lines))
}

/// `GET /subscriptions/:id/entitlements` — the resolved entitlements.
pub async fn subscription_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = subscription(&st, auth.organization.id, &external_id).await?;
    Ok(Json(
        json!({ "external_subscription_id": external_id, "entitlements": resolved_subscription_entitlements(&st, &sub).await? }),
    ))
}

/// `PATCH /subscriptions/:id/entitlements` — merge overrides for this subscription.
pub async fn patch_subscription_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = subscription(&st, org, &external_id).await?;
    apply_entitlements(&st, org, EntitlementScope::Subscription(sub.id), &body, true).await?;
    Ok(Json(
        json!({ "entitlements": resolved_subscription_entitlements(&st, &sub).await? }),
    ))
}

/// `DELETE /subscriptions/:id/entitlements/:feature` — the feature is gone for
/// this subscription (its own override and the plan's).
pub async fn destroy_subscription_entitlement(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, feature)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = subscription(&st, org, &external_id).await?;
    let fid = st
        .db
        .find_feature_by_code(org, &feature)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    let had_own = st
        .db
        .delete_scoped_entitlement(EntitlementScope::Subscription(sub.id), fid)
        .await?;
    let on_plan = st
        .db
        .scoped_entitlement(EntitlementScope::Plan(sub.plan_id), fid)
        .await?
        .is_some();
    if !had_own && !on_plan {
        return Err(ApiError::NotFound("entitlement".into()));
    }
    if on_plan {
        st.db.add_feature_removal(org, sub.id, fid, None).await?;
    }
    Ok(Json(
        json!({ "entitlements": resolved_subscription_entitlements(&st, &sub).await? }),
    ))
}

/// `DELETE /subscriptions/:id/entitlements/:feature/privileges/:privilege`.
pub async fn destroy_subscription_entitlement_privilege(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, feature, privilege)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = subscription(&st, org, &external_id).await?;
    let fid = st
        .db
        .find_feature_by_code(org, &feature)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    let p = st
        .db
        .live_privilege(fid, &privilege)
        .await?
        .ok_or_else(|| ApiError::NotFound("privilege".into()))?;
    let had_own = st
        .db
        .delete_scoped_entitlement_value(EntitlementScope::Subscription(sub.id), fid, p.id)
        .await?;
    let plan_lines = st.db.entitlement_lines(EntitlementScope::Plan(sub.plan_id)).await?;
    let on_plan = plan_lines
        .iter()
        .any(|l| l.feature_id == fid && l.privilege_code.as_deref() == Some(privilege.as_str()));
    if !had_own && !on_plan {
        return Err(ApiError::NotFound("entitlement".into()));
    }
    if on_plan {
        st.db.add_feature_removal(org, sub.id, fid, Some(p.id)).await?;
    }
    Ok(Json(
        json!({ "entitlements": resolved_subscription_entitlements(&st, &sub).await? }),
    ))
}

/// `DELETE /features/:code/privileges/:code` — remove a privilege from a feature.
pub async fn destroy_feature_privilege(
    State(st): State<AppState>,
    auth: Authed,
    Path((feature, privilege)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let fid = st
        .db
        .find_feature_by_code(org, &feature)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    let p = st
        .db
        .live_privilege(fid, &privilege)
        .await?
        .ok_or_else(|| ApiError::NotFound("privilege".into()))?;
    st.db.delete_privilege(fid, &privilege).await?;
    Ok(Json(
        json!({ "privilege": { "code": p.code, "value_type": p.value_type } }),
    ))
}
