//! New top-level resources — faithful ports of Lago's `Api::V1` controllers for
//! taxes, webhook endpoints, billing entities, organizations, and entitlement
//! features. Each handler authenticates via the api-key extractor, calls an
//! existing [`rago_db::Db`] op, and serializes the Lago-shaped JSON.

use axum::extract::{Path, State};
use axum::Json;
use rago_core::models::Tax;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{now, req_s, s, wrapped};
use crate::state::AppState;

// ---- taxes -----------------------------------------------------------------

fn tax_json(t: &Tax) -> Value {
    json!({
        "lago_id": t.id,
        "name": t.name,
        "code": t.code,
        "rate": t.rate,
        "description": t.description,
        "applied_to_organization": t.applied_to_organization,
        "created_at": t.created_at.to_rfc3339(),
    })
}

/// `POST /taxes` — create a tax rate (`Taxes::CreateService`).
pub async fn create_tax(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "tax");
    let name = req_s(p, "name")?;
    let code = req_s(p, "code")?;
    let rate = p
        .get("rate")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .ok_or_else(|| ApiError::validation("rate", "value_is_mandatory"))?;
    let applied = p.get("applied_to_organization").and_then(Value::as_bool).unwrap_or(false);
    let id = Uuid::new_v4();
    st.db
        .insert_tax(id, auth.organization.id, &name, &code, s(p, "description").as_deref(), rate, applied)
        .await?;
    let tax = st.db.find_tax(id).await?.ok_or_else(|| ApiError::NotFound("tax".into()))?;
    Ok(Json(json!({ "tax": tax_json(&tax) })))
}

/// `GET /taxes` — the org's tax rates.
pub async fn list_taxes(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let taxes = st.db.list_taxes(auth.organization.id).await?;
    Ok(Json(json!({ "taxes": taxes.iter().map(tax_json).collect::<Vec<_>>() })))
}

/// `GET /taxes/:code` — a tax rate by code.
pub async fn show_tax(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let tax = st
        .db
        .find_tax_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("tax".into()))?;
    Ok(Json(json!({ "tax": tax_json(&tax) })))
}

/// `PUT /taxes/:code` — update a tax rate (`Taxes::UpdateService`).
pub async fn update_tax(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let existing = st
        .db
        .find_tax_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("tax".into()))?;
    let p = wrapped(&body, "tax");
    let rate = p.get("rate").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let tax = st
        .db
        .update_tax(
            existing.id,
            s(p, "name").as_deref(),
            s(p, "code").as_deref(),
            s(p, "description").as_deref(),
            rate,
            p.get("applied_to_organization").and_then(Value::as_bool),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("tax".into()))?;
    Ok(Json(json!({ "tax": tax_json(&tax) })))
}

/// `DELETE /taxes/:code` — remove a tax rate (`Taxes::DestroyService`).
pub async fn destroy_tax(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let tax = st
        .db
        .find_tax_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("tax".into()))?;
    let out = tax_json(&tax);
    st.db.delete_tax(tax.id).await?;
    Ok(Json(json!({ "tax": out })))
}

// ---- webhook endpoints -----------------------------------------------------

fn webhook_endpoint_json(id: Uuid, url: &str, algo: &str) -> Value {
    json!({
        "lago_id": id,
        "webhook_url": url,
        "signature_algo": algo,
    })
}

/// `POST /webhook_endpoints` — register an outbound webhook endpoint.
pub async fn create_webhook_endpoint(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "webhook_endpoint");
    let url = req_s(p, "webhook_url")?;
    let id = st.db.insert_webhook_endpoint_returning(auth.organization.id, &url).await?;
    let algo = s(p, "signature_algo").unwrap_or_else(|| "jwt".into());
    Ok(Json(json!({ "webhook_endpoint": webhook_endpoint_json(id, &url, &algo) })))
}

/// `GET /webhook_endpoints` — the org's registered endpoints.
pub async fn list_webhook_endpoints(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_webhook_endpoints(auth.organization.id).await?;
    let out: Vec<Value> = rows.iter().map(|(id, url, algo)| webhook_endpoint_json(*id, url, algo)).collect();
    Ok(Json(json!({ "webhook_endpoints": out })))
}

/// `GET /webhook_endpoints/:id` — a single endpoint.
pub async fn show_webhook_endpoint(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (id, url, algo) = st
        .db
        .find_webhook_endpoint(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("webhook_endpoint".into()))?;
    Ok(Json(json!({ "webhook_endpoint": webhook_endpoint_json(id, &url, &algo) })))
}

/// `PUT /webhook_endpoints/:id` — update the URL / signature algorithm.
pub async fn update_webhook_endpoint(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "webhook_endpoint");
    let (id, url, algo) = st
        .db
        .update_webhook_endpoint(id, s(p, "webhook_url").as_deref(), s(p, "signature_algo").as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound("webhook_endpoint".into()))?;
    Ok(Json(json!({ "webhook_endpoint": webhook_endpoint_json(id, &url, &algo) })))
}

/// `DELETE /webhook_endpoints/:id` — remove an endpoint.
pub async fn destroy_webhook_endpoint(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (id, url, algo) = st
        .db
        .find_webhook_endpoint(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("webhook_endpoint".into()))?;
    st.db.delete_webhook_endpoint(id).await?;
    Ok(Json(json!({ "webhook_endpoint": webhook_endpoint_json(id, &url, &algo) })))
}

// ---- billing entities ------------------------------------------------------

fn billing_entity_json(r: &rago_db::BillingEntityRow) -> Value {
    json!({
        "lago_id": r.id,
        "code": r.code,
        "name": r.name,
        "email": r.email,
        "country": r.country,
        "default_currency": r.default_currency,
        "timezone": r.timezone,
    })
}

/// `POST /billing_entities` — create a billing entity.
pub async fn create_billing_entity(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "billing_entity");
    let code = req_s(p, "code")?;
    let name = req_s(p, "name")?;
    let currency = s(p, "default_currency").unwrap_or_else(|| "USD".into());
    let timezone = s(p, "timezone").unwrap_or_else(|| "UTC".into());
    let id = Uuid::new_v4();
    st.db
        .insert_billing_entity(id, auth.organization.id, &code, &name, s(p, "email").as_deref(), s(p, "country").as_deref(), &currency, &timezone)
        .await?;
    let entity = st.db.find_billing_entity(id).await?.ok_or_else(|| ApiError::NotFound("billing_entity".into()))?;
    Ok(Json(json!({ "billing_entity": billing_entity_json(&entity) })))
}

/// `GET /billing_entities` — the org's billing entities.
pub async fn list_billing_entities(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_billing_entities(auth.organization.id).await?;
    Ok(Json(json!({ "billing_entities": rows.iter().map(billing_entity_json).collect::<Vec<_>>() })))
}

/// `GET /billing_entities/:code` — a billing entity by code.
pub async fn show_billing_entity(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.list_billing_entities(auth.organization.id).await?;
    let entity = rows
        .into_iter()
        .find(|r| r.code == code)
        .ok_or_else(|| ApiError::NotFound("billing_entity".into()))?;
    Ok(Json(json!({ "billing_entity": billing_entity_json(&entity) })))
}

/// `PUT /billing_entities/:code` — update a billing entity.
pub async fn update_billing_entity(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.list_billing_entities(auth.organization.id).await?;
    let entity = rows
        .into_iter()
        .find(|r| r.code == code)
        .ok_or_else(|| ApiError::NotFound("billing_entity".into()))?;
    let p = wrapped(&body, "billing_entity");
    let updated = st
        .db
        .update_billing_entity(
            entity.id,
            s(p, "name").as_deref(),
            s(p, "email").as_deref(),
            s(p, "country").as_deref(),
            s(p, "default_currency").as_deref(),
            s(p, "timezone").as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("billing_entity".into()))?;
    Ok(Json(json!({ "billing_entity": billing_entity_json(&updated) })))
}

// ---- organizations ---------------------------------------------------------

fn organization_json(o: &rago_core::models::Organization) -> Value {
    json!({
        "lago_id": o.id,
        "name": o.name,
        "default_currency": o.default_currency,
        "email": o.email,
        "country": o.country,
        "tax_identification_number": o.tax_identification_number,
        "timezone": o.timezone,
        "document_locale": o.document_locale,
        "created_at": o.created_at.to_rfc3339(),
    })
}

/// `GET /organizations` — the authenticated organization.
pub async fn show_organization(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let o = &auth.organization;
    let mut json = organization_json(o);
    add_numbering(&st, o, &mut json).await?;
    Ok(Json(json!({ "organization": json })))
}

/// Add the invoice-numbering settings to an organization payload: the custom
/// prefix (if any) and the prefix new invoices will actually carry.
async fn add_numbering(
    st: &AppState,
    o: &rago_core::models::Organization,
    json: &mut Value,
) -> ApiResult<()> {
    let custom = st.db.document_number_prefix(o.id).await?;
    let effective = st.db.effective_document_number_prefix(o.id, &o.name).await?;
    let grace = st.db.organization_invoice_grace_period(o.id).await?;
    if let Value::Object(map) = json {
        map.insert("document_number_prefix".into(), json!(custom));
        map.insert("effective_document_number_prefix".into(), json!(effective));
        map.insert("invoice_grace_period".into(), json!(grace));
    }
    Ok(())
}

/// `PUT /organizations` — update the authenticated organization.
pub async fn update_organization(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "organization");
    // `document_number_prefix`: a string sets it, `null` / "" resets to the default.
    if let Some(v) = p.get("document_number_prefix") {
        let prefix = v.as_str().map(str::trim).filter(|x| !x.is_empty());
        if let Some(px) = prefix {
            rago_db::numbering::validate_prefix(px)
                .map_err(|code| ApiError::validation("document_number_prefix", code))?;
        }
        st.db.set_document_number_prefix(auth.organization.id, prefix).await?;
    }
    // `invoice_grace_period`: days a new invoice stays an editable draft
    // before it is finalized (0 = finalize immediately).
    if let Some(v) = p.get("invoice_grace_period").filter(|v| !v.is_null()) {
        let days = v
            .as_i64()
            .or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()))
            .ok_or_else(|| ApiError::validation("invoice_grace_period", "value_is_invalid"))?;
        if !(0..=i64::from(rago_db::lifecycle::MAX_INVOICE_GRACE_PERIOD_DAYS)).contains(&days) {
            return Err(ApiError::validation("invoice_grace_period", "value_is_out_of_range"));
        }
        st.db
            .set_organization_invoice_grace_period(auth.organization.id, days as i32)
            .await?;
    }
    let org = st
        .db
        .update_organization(
            auth.organization.id,
            s(p, "name").as_deref(),
            s(p, "default_currency").as_deref(),
            s(p, "email").as_deref(),
            s(p, "country").as_deref(),
            s(p, "timezone").as_deref(),
        )
        .await?;
    let mut json = organization_json(&org);
    add_numbering(&st, &org, &mut json).await?;
    Ok(Json(json!({ "organization": json })))
}

// ---- entitlement features --------------------------------------------------

/// `POST /features` — create an entitlement feature (+ optional privileges).
pub async fn create_feature(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "feature");
    let code = req_s(p, "code")?;
    let feature_id = match st.db.find_feature_by_code(org, &code).await? {
        Some(id) => id,
        None => {
            let id = Uuid::new_v4();
            st.db.insert_feature(id, org, &code, s(p, "name").as_deref(), s(p, "description").as_deref()).await?;
            id
        }
    };
    let mut privileges = Vec::new();
    if let Some(arr) = p.get("privileges").and_then(Value::as_array) {
        for pv in arr {
            let pcode = req_s(pv, "code")?;
            let pid = Uuid::new_v4();
            let value_type = s(pv, "value_type").unwrap_or_else(|| "string".into());
            st.db.insert_privilege(pid, org, feature_id, &pcode, s(pv, "name").as_deref(), &value_type, &json!({})).await?;
            privileges.push(json!({ "code": pcode, "value_type": value_type }));
        }
    }
    // `Entitlement::Features::CreateService` emits `feature.created`.
    crate::emit::webhook(
        &st.db,
        org,
        rago_integrations::webhooks::WebhookKind::FeatureCreated,
        json!({ "lago_id": feature_id, "code": code, "name": s(p, "name") }),
    )
    .await;
    Ok(Json(json!({
        "feature": {
            "lago_id": feature_id,
            "code": code,
            "name": s(p, "name"),
            "description": s(p, "description"),
            "privileges": privileges,
        }
    })))
}

/// `GET /features` — the org's entitlement features.
pub async fn list_features(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_features(auth.organization.id).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(id, code, name, description)| json!({ "lago_id": id, "code": code, "name": name, "description": description }))
        .collect();
    Ok(Json(json!({ "features": out })))
}

/// `GET /features/:code` — a feature by code.
pub async fn show_feature(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let (id, code, name, description) = st
        .db
        .find_feature(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    Ok(Json(json!({ "feature": { "lago_id": id, "code": code, "name": name, "description": description } })))
}

/// `PUT /features/:code` — update a feature's name / description
/// (`Entitlement::Features::UpdateService`).
pub async fn update_feature(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let (feature_id, _, _, _) = st
        .db
        .find_feature(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    let p = wrapped(&body, "feature");
    let (id, code, name, description) = st
        .db
        .update_feature(feature_id, s(p, "name").as_deref(), s(p, "description").as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    // `Entitlement::Features::UpdateService` emits `feature.updated`.
    crate::emit::webhook(
        &st.db,
        auth.organization.id,
        rago_integrations::webhooks::WebhookKind::FeatureUpdated,
        json!({ "lago_id": id, "code": code, "name": name }),
    )
    .await;
    Ok(Json(json!({ "feature": { "lago_id": id, "code": code, "name": name, "description": description } })))
}

/// `DELETE /features/:code` — soft-delete a feature
/// (`Entitlement::Features::DestroyService`).
pub async fn destroy_feature(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let (id, code, name, description) = st
        .db
        .find_feature(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;
    st.db.soft_delete_feature(id).await?;
    // `Entitlement::Features::DestroyService` emits `feature.deleted`.
    crate::emit::webhook(
        &st.db,
        auth.organization.id,
        rago_integrations::webhooks::WebhookKind::FeatureDeleted,
        json!({ "lago_id": id, "code": code }),
    )
    .await;
    Ok(Json(json!({ "feature": { "lago_id": id, "code": code, "name": name, "description": description } })))
}

// Keep `now` referenced for symmetry with sibling modules that timestamp.
#[allow(dead_code)]
fn _touch() {
    let _ = now();
}
