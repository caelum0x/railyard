//! Lago REST routes for audit logs, usage analytics and document/quote aliases:
//! `GET /activity_logs/:id`, `/api_logs(/:request_id)`, `/security_logs(/:id)`,
//! `GET /analytics/usage`, `GET /organizations/grpc_token`, the
//! `POST /invoices/:id/download_pdf|download_xml` and
//! `POST /credit_notes/:id/download_pdf` document requests, and the flat
//! `quote_versions` routes. The API-log recorder middleware lives here too.

use axum::body::Body;
use axum::extract::{OriginalUri, Path, Query, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::pagination::{self, PageParams};
use crate::state::AppState;

// ---- activity logs ---------------------------------------------------------

/// `GET /activity_logs/:activity_id`.
pub async fn show_activity_log(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let log = st
        .db
        .find_activity_log(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("activity_log".into()))?;
    Ok(Json(json!({ "activity_log": log })))
}

// ---- api logs ----------------------------------------------------------------

fn api_log_json(r: &rago_db::rest_parity::ApiLogRow) -> Value {
    json!({
        "request_id": r.request_id,
        "api_key_id": r.api_key_id,
        "api_version": r.api_version,
        "http_method": r.http_method,
        "http_status": r.http_status,
        "request_path": r.request_path,
        "request_origin": r.request_origin,
        "client": r.client,
        "request_body": r.request_body,
        "logged_at": r.created_at.and_utc().to_rfc3339(),
        "created_at": r.created_at.and_utc().to_rfc3339(),
    })
}

/// `GET /api_logs`.
pub async fn list_api_logs(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let (rows, total) = st.db.list_api_logs(auth.organization.id, pg.per_page(), pg.offset()).await?;
    Ok(Json(json!({
        "api_logs": rows.iter().map(api_log_json).collect::<Vec<_>>(),
        "meta": pagination::meta(pg.page(), pg.per_page(), total),
    })))
}

/// `GET /api_logs/:request_id`.
pub async fn show_api_log(
    State(st): State<AppState>,
    auth: Authed,
    Path(request_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st
        .db
        .find_api_log(auth.organization.id, request_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("api_log".into()))?;
    Ok(Json(json!({ "api_log": api_log_json(&row) })))
}

/// Keys whose values never reach the API log.
const SECRET_KEYS: [&str; 7] = ["password", "secret", "token", "api_key", "hmac", "private_key", "authorization"];
/// The largest request body kept in the API log.
const MAX_LOGGED_BODY: usize = 64 * 1024;

/// A request body with secret-looking fields replaced by `[FILTERED]`.
pub fn mask_secrets(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let lower = k.to_lowercase();
                    let masked = if SECRET_KEYS.iter().any(|s| lower.contains(s)) {
                        Value::String("[FILTERED]".into())
                    } else {
                        mask_secrets(v)
                    };
                    (k.clone(), masked)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask_secrets).collect()),
        other => other.clone(),
    }
}

/// Middleware: record every mutating API request (Lago `ApiLog`) after it
/// runs, with the caller's key, status, path and a secret-masked body.
pub async fn record_api_log(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    if !matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        return next.run(req).await;
    }
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let (token, origin, client) = {
        let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        (
            header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string)),
            header("origin"),
            header("user-agent"),
        )
    };
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_LOGGED_BODY * 16).await else {
        return Response::builder().status(413).body(Body::from("request body too large")).unwrap_or_default();
    };
    let logged_body = (bytes.len() <= MAX_LOGGED_BODY)
        .then(|| serde_json::from_slice::<Value>(&bytes).ok())
        .flatten()
        .map(|v| mask_secrets(&v));
    let response = next.run(Request::from_parts(parts, Body::from(bytes))).await;

    let Some(token) = token else { return response };
    match crate::auth::resolve_bearer(&st, &token).await {
        Ok(Some((key, org))) => {
            let version = if path.starts_with("/api/v2") { "v2" } else { "v1" };
            let status = i32::from(response.status().as_u16());
            if let Err(e) = st
                .db
                .insert_api_log(
                    org.id,
                    Uuid::new_v4(),
                    Some(key.id),
                    version,
                    method.as_str(),
                    status,
                    &path,
                    origin.as_deref(),
                    client.as_deref(),
                    logged_body.as_ref(),
                )
                .await
            {
                tracing::warn!(error = %e, "api log write failed");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = ?e, "api log: bearer resolution failed"),
    }
    response
}

// ---- security logs ---------------------------------------------------------

fn security_log_json(r: &rago_db::rest_parity::SecurityLogRow) -> Value {
    json!({
        "log_id": r.id,
        "log_type": r.log_type,
        "log_event": r.log_event,
        "user_id": r.user_id,
        "api_key_id": r.api_key_id,
        "resources": r.resources,
        "device_info": r.device_info,
        "logged_at": r.created_at.and_utc().to_rfc3339(),
        "created_at": r.created_at.and_utc().to_rfc3339(),
    })
}

/// `GET /security_logs`.
pub async fn list_security_logs(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let (rows, total) = st.db.list_security_logs(auth.organization.id, pg.per_page(), pg.offset()).await?;
    Ok(Json(json!({
        "security_logs": rows.iter().map(security_log_json).collect::<Vec<_>>(),
        "meta": pagination::meta(pg.page(), pg.per_page(), total),
    })))
}

/// `GET /security_logs/:log_id`.
pub async fn show_security_log(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st
        .db
        .find_security_log(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("security_log".into()))?;
    Ok(Json(json!({ "security_log": security_log_json(&row) })))
}

// ---- usage analytics -------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct UsageParams {
    pub from_date: Option<NaiveDate>,
    pub to_date: Option<NaiveDate>,
    pub external_subscription_id: Option<String>,
}

/// The longest window `GET /analytics/usage` serves, in days.
const MAX_USAGE_DAYS: i64 = 366;

/// `GET /analytics/usage` — daily usage amounts (default: the last 30 days).
pub async fn usage_analytics(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<UsageParams>,
) -> ApiResult<Json<Value>> {
    let to = q.to_date.unwrap_or_else(|| Utc::now().date_naive());
    let from = q.from_date.unwrap_or(to - Duration::days(30));
    if from > to || (to - from).num_days() > MAX_USAGE_DAYS {
        return Err(ApiError::validation("from_date", "invalid_date_range"));
    }
    let rows = st
        .db
        .daily_usage(auth.organization.id, from, to, q.external_subscription_id.as_deref())
        .await?;
    let currency = auth.organization.default_currency.clone();
    let usages: Vec<Value> = rows
        .iter()
        .map(|(day, sub, cents)| {
            json!({
                "start_of_period_dt": day,
                "end_of_period_dt": day,
                "external_subscription_id": sub,
                "amount_currency": currency,
                "amount_cents": cents,
            })
        })
        .collect();
    Ok(Json(json!({ "usages": usages })))
}

// ---- organization grpc token ------------------------------------------------

/// `GET /organizations/grpc_token`.
pub async fn grpc_token(auth: Authed) -> ApiResult<Json<Value>> {
    let token = rago_graphql::auth::issue_grpc_jwt(auth.organization.id);
    Ok(Json(json!({ "organization": { "grpc_token": token } })))
}

// ---- document requests ---------------------------------------------------------

/// The public base URL documents are served from (`RAGO_API_URL`, else relative).
fn api_base() -> String {
    std::env::var("RAGO_API_URL")
        .or_else(|_| std::env::var("LAGO_API_URL"))
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

async fn invoice_with_url(st: &AppState, id: Uuid, key: &str, suffix: &str) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(inv.id).await?;
    let mut body = crate::serializers::invoice(&inv, &fees);
    body[key] = json!(format!("{}/api/v1/invoices/{id}/{suffix}", api_base()));
    Ok(Json(json!({ "invoice": body })))
}

/// `POST /invoices/:id/download_pdf` — the invoice with its `file_url`
/// (documents render on demand, so the URL is ready immediately).
pub async fn invoice_download_pdf(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    invoice_with_url(&st, id, "file_url", "download").await
}

/// `POST /invoices/:id/download_xml` — the invoice with its `xml_url`.
pub async fn invoice_download_xml(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    invoice_with_url(&st, id, "xml_url", "download.xml").await
}

/// `POST /credit_notes/:id/download_pdf` — the credit note with its `file_url`.
pub async fn credit_note_download_pdf(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    let mut body = crate::serializers::credit_note(&cn);
    body["file_url"] = json!(format!("{}/api/v1/credit_notes/{id}/download", api_base()));
    Ok(Json(json!({ "credit_note": body })))
}

// ---- flat quote-version routes ------------------------------------------------

async fn version_quote(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<Uuid> {
    st.db
        .quote_version_detail(org, id)
        .await?
        .map(|v| v.quote_id)
        .ok_or_else(|| ApiError::NotFound("quote_version".into()))
}

/// `GET /quotes/:id/versions`.
pub async fn list_quote_versions(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    st.db.quote_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("quote".into()))?;
    let versions = st.db.quote_version_details(org, id).await?;
    Ok(Json(json!({
        "quote_versions": versions.iter().map(crate::order_flow::version_json).collect::<Vec<_>>(),
    })))
}

/// `GET /quote_versions/:id`.
pub async fn show_quote_version(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let v = st
        .db
        .quote_version_detail(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("quote_version".into()))?;
    Ok(Json(json!({ "quote_version": crate::order_flow::version_json(&v) })))
}

/// `POST /quote_versions/:id/approve`.
pub async fn approve_quote_version(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let quote = version_quote(&st, auth.organization.id, id).await?;
    crate::order_flow::approve_version(State(st), auth, Path((quote, id)), body).await
}

/// `POST /quote_versions/:id/void`.
pub async fn void_quote_version(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let quote = version_quote(&st, auth.organization.id, id).await?;
    crate::order_flow::void_version(State(st), auth, Path((quote, id))).await
}

/// `POST /quote_versions/:id/clone` — a new draft copied from this version.
pub async fn clone_quote_version(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = version_quote(&st, org, id).await?;
    let body = crate::order_flow::NewVersionBody { from_version_id: Some(id) };
    crate::order_flow::create_version(State(st), auth, Path(quote), Some(Json(body))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked_at_any_depth() {
        let body = json!({
            "customer": { "name": "A", "api_key": "k", "nested": [{ "password": "p" }] },
            "webhook": { "hmac_key": "h", "url": "u" },
        });
        let m = mask_secrets(&body);
        assert_eq!(m["customer"]["name"], "A");
        assert_eq!(m["customer"]["api_key"], "[FILTERED]");
        assert_eq!(m["customer"]["nested"][0]["password"], "[FILTERED]");
        assert_eq!(m["webhook"]["hmac_key"], "[FILTERED]");
        assert_eq!(m["webhook"]["url"], "u");
    }
}
