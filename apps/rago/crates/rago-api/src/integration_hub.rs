//! Integration hub REST surface — the operator console's integrations pages
//! and SSO sign-in.
//!
//! * connection CRUD for every provider (Xero, NetSuite, Salesforce, HubSpot,
//!   Anrok, Avalara, Okta, Entra ID, Google): secrets are sealed at rest and
//!   never returned (only a `****last4` reference);
//! * the OAuth connect step (`GET /integrations/:id/oauth/authorize` →
//!   provider → `GET /integrations/oauth/callback`), bound to the org and the
//!   integration row by a single-use state;
//! * the provider catalog fetch, item / account mappings, collection
//!   mappings, and customer links that shape synced documents;
//! * per-resource sync status (invoice / credit note / customer pages);
//! * SSO sign-in endpoints for the console (`/auth/sso/*`).

use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use rago_db::integration_hub::IntegrationConnection;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{s, wrapped};
use crate::integrations::integration_error;
use crate::state::AppState;

/// Provider code → integration type.
fn integration_type_of(provider: &str) -> Option<&'static str> {
    match provider {
        "xero" | "netsuite" => Some("accounting"),
        "salesforce" | "hubspot" => Some("crm"),
        "anrok" | "avalara" => Some("tax"),
        "okta" | "entra_id" | "google" => Some("auth"),
        _ => None,
    }
}

/// Human label for messages.
fn label(provider: &str) -> &'static str {
    match provider {
        "xero" => "Xero",
        "netsuite" => "NetSuite",
        "salesforce" => "Salesforce",
        "hubspot" => "HubSpot",
        "anrok" => "Anrok",
        "avalara" => "Avalara",
        "okta" => "Okta",
        "entra_id" => "Microsoft Entra ID",
        "google" => "Google",
        _ => "Integration",
    }
}

/// Write-only secret settings: stored as `<key>_sealed` + `<key>_ref`.
const SECRET_KEYS: &[&str] = &["client_secret", "token_secret", "api_key", "license_key"];

/// Plain settings accepted from the console / API (anything else is ignored).
const PLAIN_KEYS: &[&str] = &[
    "account_id",
    "client_id",
    "token_id",
    "script_endpoint_url",
    "subsidiary_id",
    "sync_invoices",
    "sync_credit_notes",
    "sync_payments",
    "sync_subscriptions",
    "sync_customers",
    "default_targeted_object",
    "company_code",
    "connection_id",
    "domain",
    "organization_name",
    "host",
    "tenant_id",
    "enforce_sso",
    "redirect_uri",
    "login_url",
    "sandbox",
    "instance_id",
];

/// Settings each provider needs before it can work.
fn required_settings(provider: &str) -> &'static [&'static str] {
    match provider {
        "netsuite" => &["account_id", "client_id", "client_secret", "token_id", "token_secret", "script_endpoint_url"],
        "anrok" => &["api_key"],
        "avalara" => &["account_id", "license_key", "company_code"],
        "okta" => &["domain", "organization_name", "client_id", "client_secret"],
        "entra_id" => &["domain", "tenant_id", "client_id", "client_secret"],
        "google" => &["domain"],
        _ => &[],
    }
}

/// Collection mapping types (Lago `IntegrationCollectionMappings::Base`).
const COLLECTION_MAPPING_TYPES: &[&str] = &[
    "fallback_item",
    "coupon",
    "subscription_fee",
    "minimum_commitment",
    "tax",
    "prepaid_credit",
    "credit_note",
    "account",
];

fn normalize_domain(d: &str) -> Result<String, ApiError> {
    let d = rago_jobs::sso::email_domain(d.trim().trim_start_matches('@'));
    if d.is_empty() || !d.contains('.') || d.contains(' ') {
        return Err(ApiError::validation("domain", "invalid_domain"));
    }
    Ok(d)
}

/// Merge `input` settings into `base`: plain keys copied (empty string
/// clears), secret keys sealed (empty keeps the stored secret).
fn merge_settings(base: &Value, input: &Value) -> Result<Value, ApiError> {
    let mut out: Map<String, Value> = base.as_object().cloned().unwrap_or_default();
    let Some(obj) = input.as_object() else {
        return Ok(Value::Object(out));
    };
    for key in PLAIN_KEYS {
        if let Some(v) = obj.get(*key) {
            match v {
                Value::String(s) if s.trim().is_empty() => {
                    out.remove(*key);
                }
                Value::String(s) if *key == "domain" => {
                    out.insert((*key).to_string(), json!(normalize_domain(s)?));
                }
                Value::String(s) if *key == "script_endpoint_url" || *key == "login_url" || *key == "redirect_uri" => {
                    let s = s.trim();
                    if !s.starts_with("https://") {
                        return Err(ApiError::validation(key, "must_be_https_url"));
                    }
                    out.insert((*key).to_string(), json!(s));
                }
                Value::String(s) => {
                    out.insert((*key).to_string(), json!(s.trim()));
                }
                Value::Bool(_) | Value::Number(_) => {
                    out.insert((*key).to_string(), v.clone());
                }
                Value::Null => {
                    out.remove(*key);
                }
                _ => return Err(ApiError::validation(key, "invalid_value")),
            }
        }
    }
    for key in SECRET_KEYS {
        let Some(secret) = obj.get(*key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        let sealed = rago_db::secrets::seal(secret).map_err(|e| match e {
            rago_db::secrets::SecretError::KeyMissing => {
                ApiError::validation("integration", "encryption_key_missing: set RAGO_ENCRYPTION_KEY")
            }
            other => ApiError::Internal(other.to_string()),
        })?;
        out.insert(format!("{key}_sealed"), json!(sealed));
        out.insert(format!("{key}_ref"), json!(rago_db::secrets::mask(secret)));
    }
    Ok(Value::Object(out))
}

/// Settings safe to return: sealed values removed.
fn public_settings(settings: &Value) -> Value {
    match settings.as_object() {
        Some(obj) => Value::Object(
            obj.iter()
                .filter(|(k, _)| !k.ends_with("_sealed"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => json!({}),
    }
}

fn has_setting(settings: &Value, key: &str) -> bool {
    let present = |k: &str| settings.get(k).is_some_and(|v| match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Null => false,
        _ => true,
    });
    if SECRET_KEYS.contains(&key) {
        present(&format!("{key}_sealed"))
    } else {
        present(key)
    }
}

fn missing_settings(provider: &str, settings: &Value) -> Vec<&'static str> {
    required_settings(provider).iter().copied().filter(|k| !has_setting(settings, k)).collect()
}

/// Connection status for the console.
fn status_of(c: &IntegrationConnection) -> &'static str {
    let missing = !missing_settings(&c.provider_code, &c.settings).is_empty();
    if missing {
        return "incomplete";
    }
    if rago_jobs::integration_auth::is_oauth_provider(&c.provider_code) {
        return if c.access_token.is_some() { "connected" } else { "needs_authorization" };
    }
    "connected"
}

fn serialize(c: &IntegrationConnection) -> Value {
    json!({
        "lago_id": c.id,
        "integration_type": c.integration_type,
        "provider_code": c.provider_code,
        "provider_name": label(&c.provider_code),
        "code": c.code,
        "name": c.name,
        "status": status_of(c),
        "oauth": rago_jobs::integration_auth::is_oauth_provider(&c.provider_code),
        "has_access_token": c.access_token.is_some(),
        "has_refresh_token": c.refresh_token.is_some(),
        "expires_at": c.token_expires_at.map(|t| t.and_utc().to_rfc3339()),
        "missing_settings": missing_settings(&c.provider_code, &c.settings),
        "settings": public_settings(&c.settings),
        "created_at": c.created_at.and_utc().to_rfc3339(),
    })
}

async fn load(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<IntegrationConnection> {
    st.db
        .find_org_integration(org, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("integration".into()))
}

// ---- connections ------------------------------------------------------------------

/// `GET /integrations` — the organization's live integrations (secrets never
/// returned).
pub async fn list(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let rows = st.db.list_org_integrations(auth.organization.id).await?;
    Ok(Json(json!({ "integrations": rows.iter().map(serialize).collect::<Vec<_>>() })))
}

/// `POST /integrations` — create an integration.
///
/// Body: `{ integration: { provider_code, name, code?, settings: {...} } }`.
/// Secret settings (`client_secret`, `token_secret`, `api_key`,
/// `license_key`) are sealed. OAuth providers (Xero, Salesforce, HubSpot) are
/// then connected through `GET /integrations/:id/oauth/authorize`.
///
/// Legacy API clients that completed the provider redirect themselves post
/// `{ code, state }` instead; the state must have been minted by the
/// authorize step.
pub async fn create(State(st): State<AppState>, auth: Authed, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "integration");
    if let (Some(code), Some(state)) = (s(p, "code").filter(|_| p.get("state").is_some()), s(p, "state")) {
        let transport = rago_integrations::default_transport();
        let connected = rago_jobs::integration_auth::complete(&st.db, &transport, &code, &state)
            .await
            .map_err(|e| integration_error("oauth", e))?;
        if connected.organization_id != org {
            return Err(ApiError::NotFound("integration".into()));
        }
        let conn = load(&st, org, connected.integration_id).await?;
        return Ok(Json(json!({ "integration": serialize(&conn) })));
    }

    let provider = s(p, "provider_code")
        .map(|v| v.trim().to_ascii_lowercase())
        .ok_or_else(|| ApiError::validation("provider_code", "value_is_mandatory"))?;
    let integration_type = integration_type_of(&provider).ok_or_else(|| ApiError::validation("provider_code", "unsupported_provider"))?;
    let name = s(p, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| label(&provider).to_string());
    let code = s(p, "code").filter(|c| !c.trim().is_empty()).unwrap_or_else(|| provider.clone());
    let settings = merge_settings(&json!({}), p.get("settings").unwrap_or(&json!({})))?;
    let missing = missing_settings(&provider, &settings);
    if !missing.is_empty() {
        let details: Map<String, Value> = missing.iter().map(|k| ((*k).to_string(), json!(["value_is_mandatory"]))).collect();
        return Err(ApiError::Validation(Value::Object(details)));
    }
    if integration_type == "auth" {
        let domain = settings.get("domain").and_then(Value::as_str).unwrap_or_default().to_string();
        let taken = st.db.auth_integrations_for_domain(&domain, Some(&provider)).await?;
        if taken.iter().any(|c| c.organization_id != org) {
            return Err(ApiError::validation("domain", "domain_already_claimed"));
        }
    }
    let row = st
        .db
        .insert_integration(Uuid::new_v4(), org, integration_type, &provider, Some(&code), Some(&name), &settings)
        .await?;
    let conn = load(&st, org, row.id).await?;
    Ok(Json(json!({ "integration": serialize(&conn) })))
}

/// `GET /integrations/:id` — one integration with its mappings, collection
/// mappings, linked customers and cached catalog.
pub async fn show(State(st): State<AppState>, auth: Authed, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    let mappings: Vec<Value> = st
        .db
        .list_integration_mappings(conn.id)
        .await?
        .into_iter()
        .filter(|m| {
            let t = m.mappable_type.replace('_', "").to_ascii_lowercase();
            t == "billablemetric" || t == "addon"
        })
        .map(|m| {
            json!({
                "lago_id": m.id,
                "mappable_type": m.mappable_type,
                "mappable_id": m.mappable_id,
                "external_id": m.external_id,
                "external_account_code": m.external_account_code,
                "external_name": m.external_name,
            })
        })
        .collect();
    let collection_mappings: Vec<Value> = st
        .db
        .list_integration_collection_mappings(conn.id)
        .await?
        .into_iter()
        .map(|m| {
            json!({
                "lago_id": m.id,
                "mapping_type": m.mapping_type,
                "external_id": m.external_id,
                "external_account_code": m.external_account_code,
                "external_name": m.external_name,
            })
        })
        .collect();
    let customers: Vec<Value> = st
        .db
        .integration_customers_of(conn.id)
        .await?
        .into_iter()
        .map(|(link, cid, ext, name, provider_id)| {
            json!({
                "lago_id": link,
                "lago_customer_id": cid,
                "external_customer_id": ext,
                "customer_name": name,
                "provider_customer_id": provider_id,
            })
        })
        .collect();
    let items: Vec<Value> = st
        .db
        .integration_items_of_type(conn.id, None)
        .await?
        .into_iter()
        .map(|(id, i)| {
            json!({
                "lago_id": id,
                "item_type": i.item_type,
                "external_id": i.external_id,
                "external_name": i.external_name,
                "external_account_code": i.external_account_code,
            })
        })
        .collect();
    let sso_redirect_uri = if conn.integration_type == "auth" {
        rago_jobs::sso::redirect_uri(&conn.provider_code, Some(&conn))
    } else {
        None
    };
    Ok(Json(json!({
        "integration": serialize(&conn),
        "mappings": mappings,
        "collection_mappings": collection_mappings,
        "customers": customers,
        "items": items,
        "collection_mapping_types": COLLECTION_MAPPING_TYPES,
        "oauth_redirect_uri": rago_jobs::integration_auth::redirect_uri().ok(),
        "sso_redirect_uri": sso_redirect_uri,
    })))
}

/// `PUT /integrations/:id` — rename / re-code and merge settings (an empty
/// secret keeps the stored one).
pub async fn update(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    let p = wrapped(&body, "integration");
    let settings = match p.get("settings") {
        Some(input) => Some(merge_settings(&conn.settings, input)?),
        None => None,
    };
    if let Some(settings) = &settings {
        let missing = missing_settings(&conn.provider_code, settings);
        if !missing.is_empty() {
            let details: Map<String, Value> = missing.iter().map(|k| ((*k).to_string(), json!(["value_is_mandatory"]))).collect();
            return Err(ApiError::Validation(Value::Object(details)));
        }
    }
    st.db
        .update_integration(
            conn.id,
            s(p, "code").filter(|c| !c.trim().is_empty()).as_deref(),
            s(p, "name").filter(|n| !n.trim().is_empty()).as_deref(),
            settings.as_ref(),
        )
        .await?;
    let conn = load(&st, auth.organization.id, id).await?;
    Ok(Json(json!({ "integration": serialize(&conn) })))
}

/// `DELETE /integrations/:id` — disconnect (soft delete). Syncs, tax calls and
/// SSO through it stop immediately.
pub async fn destroy(State(st): State<AppState>, auth: Authed, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    st.db.soft_delete_integration(conn.id).await?;
    Ok(Json(json!({ "integration": { "lago_id": conn.id, "deleted": true } })))
}

// ---- OAuth connect ----------------------------------------------------------------

/// `GET /integrations/:id/oauth/authorize` — the provider authorize URL for
/// this integration (single-use state bound to the org + integration).
pub async fn oauth_authorize(State(st): State<AppState>, auth: Authed, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    if !rago_jobs::integration_auth::is_oauth_provider(&conn.provider_code) {
        return Err(ApiError::validation("integration", "oauth_not_supported_for_provider"));
    }
    let url = rago_jobs::integration_auth::authorize(&st.db, &conn)
        .await
        .map_err(|e| integration_error("oauth", e))?;
    Ok(Json(json!({ "authorize_url": url })))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn callback_error_page(message: &str) -> Response {
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"robots\" content=\"noindex\">\
         <title>Integration not connected</title><style>body{{font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif;background:#0f1117;color:#e6e8ee;}}\
         .box{{max-width:520px;margin:14vh auto;background:#171a23;border:1px solid #272c39;border-radius:10px;padding:26px}}\
         h1{{font-size:18px;margin:0 0 8px}}p{{color:#8b91a1}}code{{color:#f2668b}}a{{color:#6ea8fe}}</style></head>\
         <body><div class=\"box\"><h1>The integration was not connected</h1><p><code>{}</code></p>\
         <p><a href=\"/admin#/integrations\">Back to integrations</a></p></div></body></html>",
        html_escape(message)
    );
    (axum::http::StatusCode::BAD_REQUEST, Html(body)).into_response()
}

/// `GET /integrations/oauth/callback` — the provider redirects here with
/// `code` + `state` (no API key: the state authenticates the callback). On
/// success the browser returns to the integration's console page.
pub async fn oauth_callback(State(st): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    if let Some(err) = q.error {
        return callback_error_page(&format!("{err}: {}", q.error_description.unwrap_or_default()));
    }
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return callback_error_page("oauth_state_or_code_missing");
    };
    let transport = rago_integrations::default_transport();
    match rago_jobs::integration_auth::complete(&st.db, &transport, &code, &state).await {
        Ok(c) => Redirect::to(&format!("/admin#/integrations/{}?connected=1", c.integration_id)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "integration oauth callback failed");
            callback_error_page(&e.to_string())
        }
    }
}

// ---- catalog, mappings, customer links ------------------------------------------

/// `POST /integrations/:id/catalog` — fetch the provider's items / accounts /
/// subsidiaries into the local cache.
pub async fn refresh_catalog(State(st): State<AppState>, auth: Authed, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let transport = rago_integrations::default_transport();
    let counts = rago_jobs::integrations_sync::refresh_integration_catalog(&st.db, &transport, auth.organization.id, id)
        .await
        .map_err(|e| integration_error("integration", e))?;
    Ok(Json(json!({
        "catalog": counts.into_iter().map(|(kind, count)| json!({ "item_type": kind, "count": count })).collect::<Vec<_>>()
    })))
}

fn normalize_mappable(t: &str) -> Option<&'static str> {
    match t.replace('_', "").to_ascii_lowercase().as_str() {
        "billablemetric" => Some("BillableMetric"),
        "addon" => Some("AddOn"),
        _ => None,
    }
}

fn uuid_field(v: &Value, k: &str) -> ApiResult<Uuid> {
    s(v, k)
        .and_then(|x| Uuid::parse_str(&x).ok())
        .ok_or_else(|| ApiError::validation(k, "invalid_uuid"))
}

/// `POST /integrations/:id/mappings` — map a billable metric or add-on to a
/// provider item (and optional ledger account).
pub async fn create_mapping(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let conn = load(&st, org, id).await?;
    let p = wrapped(&body, "mapping");
    let mappable_type = s(p, "mappable_type")
        .as_deref()
        .and_then(normalize_mappable)
        .ok_or_else(|| ApiError::validation("mappable_type", "must_be_billable_metric_or_add_on"))?;
    let mappable_id = uuid_field(p, "mappable_id")?;
    let external_id = s(p, "external_id").filter(|v| !v.trim().is_empty()).ok_or_else(|| ApiError::validation("external_id", "value_is_mandatory"))?;
    // Replace an existing mapping for the same object.
    if let Some(existing) = st
        .db
        .list_integration_mappings(conn.id)
        .await?
        .into_iter()
        .find(|m| m.mappable_id == mappable_id && normalize_mappable(&m.mappable_type) == Some(mappable_type))
    {
        st.db.soft_delete_integration_mapping(existing.id).await?;
    }
    let row = st
        .db
        .insert_integration_mapping_v2(
            Uuid::new_v4(),
            org,
            conn.id,
            mappable_type,
            mappable_id,
            &external_id,
            s(p, "external_account_code").filter(|v| !v.is_empty()).as_deref(),
            s(p, "external_name").filter(|v| !v.is_empty()).as_deref(),
        )
        .await?;
    Ok(Json(json!({ "mapping": { "lago_id": row.id, "mappable_type": row.mappable_type, "mappable_id": row.mappable_id, "external_id": row.external_id } })))
}

/// `DELETE /integrations/:id/mappings/:mapping_id`.
pub async fn destroy_mapping(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, mapping_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    let m = st
        .db
        .find_integration_mapping_by_id(mapping_id)
        .await?
        .filter(|m| m.integration_connection_id == conn.id)
        .ok_or_else(|| ApiError::NotFound("mapping".into()))?;
    st.db.soft_delete_integration_mapping(m.id).await?;
    Ok(Json(json!({ "mapping": { "lago_id": m.id, "deleted": true } })))
}

/// `POST /integrations/:id/collection_mappings` — set the item/account used
/// for a document-level concept (fallback item, coupons, taxes, ...). One
/// mapping per type: an existing one is replaced.
pub async fn upsert_collection_mapping(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let conn = load(&st, org, id).await?;
    let p = wrapped(&body, "collection_mapping");
    let mapping_type = s(p, "mapping_type")
        .filter(|t| COLLECTION_MAPPING_TYPES.contains(&t.as_str()))
        .ok_or_else(|| ApiError::validation("mapping_type", "invalid_mapping_type"))?;
    let external_id = s(p, "external_id").filter(|v| !v.trim().is_empty());
    let account = s(p, "external_account_code").filter(|v| !v.trim().is_empty());
    if external_id.is_none() && account.is_none() {
        return Err(ApiError::validation("external_id", "value_is_mandatory"));
    }
    let name = s(p, "external_name").filter(|v| !v.trim().is_empty());
    let existing = st
        .db
        .list_integration_collection_mappings(conn.id)
        .await?
        .into_iter()
        .find(|m| m.mapping_type == mapping_type);
    let row = match existing {
        Some(m) => {
            st.db.soft_delete_integration_collection_mapping(m.id).await?;
            st.db
                .insert_integration_collection_mapping(Uuid::new_v4(), org, conn.id, &mapping_type, external_id.as_deref(), account.as_deref(), name.as_deref())
                .await?
        }
        None => {
            st.db
                .insert_integration_collection_mapping(Uuid::new_v4(), org, conn.id, &mapping_type, external_id.as_deref(), account.as_deref(), name.as_deref())
                .await?
        }
    };
    Ok(Json(json!({ "collection_mapping": { "lago_id": row.id, "mapping_type": row.mapping_type, "external_id": row.external_id, "external_account_code": row.external_account_code } })))
}

/// `DELETE /integrations/:id/collection_mappings/:mapping_id`.
pub async fn destroy_collection_mapping(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, mapping_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    let m = st
        .db
        .find_integration_collection_mapping(mapping_id)
        .await?
        .filter(|m| m.integration_id == conn.id)
        .ok_or_else(|| ApiError::NotFound("collection_mapping".into()))?;
    st.db.soft_delete_integration_collection_mapping(m.id).await?;
    Ok(Json(json!({ "collection_mapping": { "lago_id": m.id, "deleted": true } })))
}

/// `POST /integrations/:id/customers` — link a Rago customer to an existing
/// provider contact (so syncs reuse it instead of creating a new one).
pub async fn link_customer(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let conn = load(&st, org, id).await?;
    if conn.integration_type != "accounting" && conn.integration_type != "crm" {
        return Err(ApiError::validation("integration", "customers_link_unsupported"));
    }
    let p = wrapped(&body, "integration_customer");
    let ext = s(p, "external_customer_id").ok_or_else(|| ApiError::validation("external_customer_id", "value_is_mandatory"))?;
    let provider_id = s(p, "provider_customer_id")
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| ApiError::validation("provider_customer_id", "value_is_mandatory"))?;
    let customer = st
        .db
        .find_customer_by_external_id(org, &ext)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    for (link, integration_id, _, _, _) in st.db.customer_integration_links(org, customer.id).await? {
        if integration_id == conn.id {
            st.db.soft_delete_integration_customer(link).await?;
        }
    }
    let row = st
        .db
        .insert_integration_customer(Uuid::new_v4(), org, conn.id, customer.id, Some(provider_id.trim()), None)
        .await?;
    Ok(Json(json!({ "integration_customer": {
        "lago_id": row.id,
        "external_customer_id": customer.external_id,
        "provider_customer_id": row.external_customer_id,
    } })))
}

/// `DELETE /integrations/:id/customers/:link_id` — unlink a customer.
pub async fn unlink_customer(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, link_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let conn = load(&st, auth.organization.id, id).await?;
    let link = st
        .db
        .find_integration_customer(link_id)
        .await?
        .filter(|l| l.integration_id == conn.id)
        .ok_or_else(|| ApiError::NotFound("integration_customer".into()))?;
    st.db.soft_delete_integration_customer(link.id).await?;
    Ok(Json(json!({ "integration_customer": { "lago_id": link.id, "deleted": true } })))
}

// ---- per-resource status ------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    resource_type: String,
    lago_id: Uuid,
}

/// `GET /integration_sync_status?resource_type=invoice|credit_note|customer|subscription|payment&lago_id=`
/// — the org's active accounting / CRM / tax integrations and this resource's
/// sync records, for the invoice, credit-note and customer pages.
pub async fn sync_status(State(st): State<AppState>, auth: Authed, Query(q): Query<StatusQuery>) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    if !matches!(q.resource_type.as_str(), "invoice" | "credit_note" | "customer" | "subscription" | "payment") {
        return Err(ApiError::validation("resource_type", "invalid_resource_type"));
    }
    if !st.db.not_foreign(org, q.lago_id).await? {
        return Err(ApiError::NotFound(q.resource_type.clone()));
    }
    let mut active = Map::new();
    for kind in ["accounting", "crm", "tax"] {
        let v = match st.db.active_integration_of_type(org, kind).await? {
            Some(c) => json!({
                "lago_id": c.id,
                "provider_code": c.provider_code,
                "provider_name": label(&c.provider_code),
                "name": c.name,
                "status": status_of(&c),
            }),
            None => Value::Null,
        };
        active.insert(kind.to_string(), v);
    }
    let records: Vec<Value> = st
        .db
        .resource_mappings(org, &q.resource_type, q.lago_id)
        .await?
        .into_iter()
        .map(|m| {
            json!({
                "lago_integration_id": m.integration_id,
                "integration_type": m.integration_type,
                "provider_code": m.provider_code,
                "provider_name": label(&m.provider_code),
                "integration_name": m.integration_name,
                "external_id": m.external_id,
                "synced_at": m.synced_at.and_utc().to_rfc3339(),
            })
        })
        .collect();
    let links: Vec<Value> = if q.resource_type == "customer" {
        st.db
            .customer_integration_links(org, q.lago_id)
            .await?
            .into_iter()
            .map(|(link, integration_id, provider, itype, ext)| {
                json!({
                    "lago_id": link,
                    "lago_integration_id": integration_id,
                    "provider_code": provider,
                    "provider_name": label(&provider),
                    "integration_type": itype,
                    "provider_customer_id": ext,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(json!({ "integrations": active, "sync_records": records, "customer_links": links })))
}

/// `POST /credit_notes/:id/sync_accounting` — sync a credit note to the
/// connected accounting system now.
pub async fn sync_credit_note_accounting(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let transport = rago_integrations::default_transport();
    let external_id =
        rago_jobs::integrations_sync::sync_credit_note_accounting(&st.db, &transport, auth.organization.id, id)
            .await
            .map_err(|e| integration_error("accounting_integration", e))?;
    let provider = st
        .db
        .active_integration_of_type(auth.organization.id, "accounting")
        .await?
        .map(|c| c.provider_code);
    Ok(Json(json!({ "integration_credit_note": { "lago_credit_note_id": id, "provider": provider, "external_id": external_id } })))
}

// ---- SSO sign-in (console) -----------------------------------------------------------

/// `POST /auth/sso/authorize` — `{ email, provider? }` → the IdP authorize URL
/// for the organization that enabled SSO for the email's domain.
pub async fn sso_authorize(State(st): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let email = s(&body, "email").filter(|e| e.contains('@') || e.contains('.'));
    let provider = s(&body, "provider").map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty());
    if email.is_none() && provider.is_none() {
        return Err(ApiError::validation("email", "value_is_mandatory"));
    }
    let started = rago_jobs::sso::start(&st.db, provider.as_deref(), email.as_deref())
        .await
        .map_err(|e| integration_error("sso", e))?;
    Ok(Json(json!({ "authorize_url": started.authorize_url, "provider": started.provider })))
}

/// `POST /auth/sso/callback` — `{ code, state }` → `{ token, user }`: a session
/// JWT pinned to the organization whose SSO integration authenticated the
/// user. The token is accepted by `/graphql` and every `/api/v1` route.
pub async fn sso_callback(State(st): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let code = s(&body, "code").ok_or_else(|| ApiError::validation("code", "value_is_mandatory"))?;
    let state = s(&body, "state").ok_or_else(|| ApiError::validation("state", "value_is_mandatory"))?;
    let transport = rago_integrations::default_transport();
    let identity = rago_jobs::sso::complete(&st.db, &transport, None, &code, &state)
        .await
        .map_err(|e| integration_error("sso", e))?;
    let login = rago_jobs::sso::resolve_login(&st.db, &identity).await.map_err(|e| match e {
        rago_jobs::JobError::Integration(msg) if msg == "user_not_found" || msg == "sso_not_enabled_for_user" => {
            ApiError::Forbidden(msg)
        }
        other => integration_error("sso", other),
    })?;
    Ok(Json(json!({
        "token": rago_graphql::auth::issue_sso_jwt(login.user_id, login.organization_id, &identity.provider),
        "user": { "id": login.user_id, "email": login.email },
        "organization_id": login.organization_id,
    })))
}
