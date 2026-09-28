//! Operator surface: `/admin` (HTML console, HTTP Basic with the admin token
//! as password) and `/admin/api/*` (Bearer admin token) for bootstrapping
//! orgs, projects, members, keys and BYO tools.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use subtle_eq::ct_eq;
use switchyard_core::{new_id, ApiError};
use switchyard_store::{ByoTool, Member, Org, Project};

use super::account::{create_key_for, project_view, CreateKey};
use crate::http_util::{html_escape as esc, ok, ApiResult, AppError};
use crate::state::AppState;

mod subtle_eq {
    /// Constant-time string equality.
    pub fn ct_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn admin_ok(st: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = st.settings.admin_token.as_deref() else { return false };
    let Some(h) = headers.get("authorization").and_then(|v| v.to_str().ok()) else { return false };
    if let Some(b) = h.strip_prefix("Bearer ") {
        return ct_eq(b.trim(), expected);
    }
    if let Some(b) = h.strip_prefix("Basic ") {
        if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b.trim()) {
            if let Ok(s) = String::from_utf8(raw) {
                return s.split_once(':').map(|(_, pw)| ct_eq(pw, expected)).unwrap_or(false);
            }
        }
    }
    false
}

fn require_admin(st: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if admin_ok(st, headers) {
        Ok(())
    } else {
        Err(ApiError::unauthorized("operator token required").into())
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

#[derive(Deserialize)]
pub struct NewOrg {
    pub slug: String,
    pub name: String,
    pub mint_account_id: Option<String>,
    pub rago_subscription_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub project_name: Option<String>,
    pub monthly_cap_micros: Option<i64>,
    pub daily_cap_micros: Option<i64>,
}

pub fn new_project(org_id: &str, name: &str, monthly: Option<i64>, daily: Option<i64>) -> Project {
    Project {
        id: new_id("prj"),
        org_id: org_id.into(),
        name: name.into(),
        monthly_cap_micros: monthly,
        daily_cap_micros: daily,
        tool_daily_caps: Default::default(),
        routing_policy: "free_first".into(),
        egress_extra_hosts: vec![],
        pinned_tools: vec![],
        daily_send_limit: Some(200),
        created_at: Utc::now(),
    }
}

pub async fn create_org(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    require_admin(&st, &headers)?;
    let b: NewOrg = parse(&body)?;
    if b.slug.is_empty() || !b.slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ApiError::bad_request("slug must be lowercase letters, digits and dashes").into());
    }
    let org = st
        .store
        .create_org(Org { id: new_id("org"), slug: b.slug, name: b.name, mint_account_id: b.mint_account_id, rago_subscription_id: b.rago_subscription_id, created_at: Utc::now() })
        .await?;
    let project = st.store.create_project(new_project(&org.id, b.project_name.as_deref().unwrap_or("default"), b.monthly_cap_micros, b.daily_cap_micros)).await?;
    if let Some(u) = b.owner_user_id {
        st.store.upsert_member(Member { org_id: org.id.clone(), user_id: u, role: "owner".into() }).await?;
    }
    Ok((StatusCode::CREATED, axum::Json(json!({ "ok": true, "data": { "org": org, "project": project_view(&project) } }))).into_response())
}

#[derive(Deserialize)]
pub struct NewProject {
    pub org_id: String,
    pub name: String,
    pub monthly_cap_micros: Option<i64>,
    pub daily_cap_micros: Option<i64>,
}

pub async fn create_project(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    require_admin(&st, &headers)?;
    let b: NewProject = parse(&body)?;
    st.store.get_org(&b.org_id).await?;
    let p = st.store.create_project(new_project(&b.org_id, &b.name, b.monthly_cap_micros, b.daily_cap_micros)).await?;
    Ok(ok(project_view(&p)))
}

pub async fn add_member(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    require_admin(&st, &headers)?;
    let m: Member = parse(&body)?;
    if !["owner", "admin", "member", "viewer"].contains(&m.role.as_str()) {
        return Err(ApiError::bad_request("role must be owner|admin|member|viewer").into());
    }
    st.store.get_org(&m.org_id).await?;
    st.store.upsert_member(m.clone()).await?;
    Ok(ok(json!(m)))
}

#[derive(Deserialize)]
pub struct AdminKey {
    pub project_id: String,
    #[serde(flatten)]
    pub key: CreateKey,
}

pub async fn create_key(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    require_admin(&st, &headers)?;
    let b: AdminKey = parse(&body)?;
    let project = st.store.get_project(&b.project_id).await?;
    let org = st.store.get_org(&project.org_id).await?;
    let (raw, key) = create_key_for(&st, &org.slug, &project.id, b.key).await?;
    Ok((StatusCode::CREATED, axum::Json(json!({ "ok": true, "data": { "key": raw, "id": key.public_id, "project_id": key.project_id, "scopes": key.scopes } }))).into_response())
}

#[derive(Deserialize)]
pub struct NewByo {
    pub org_id: String,
    pub id: String,
    pub base_url: String,
    pub credential_id: Option<String>,
    #[serde(default = "auth_header")]
    pub header: String,
    #[serde(default)]
    pub prefix: String,
}

fn auth_header() -> String {
    "Authorization".into()
}

/// Admin-only: register a team's private HTTPS API for passthrough.
pub async fn create_byo(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    require_admin(&st, &headers)?;
    let b: NewByo = parse(&body)?;
    let u = url::Url::parse(&b.base_url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if u.scheme() != "https" || u.host_str().is_none() {
        return Err(ApiError::bad_request("BYO base_url must be https").into());
    }
    if let Some(c) = &b.credential_id {
        st.store.get_credential(&b.org_id, c).await?.ok_or_else(|| ApiError::bad_request("credential_id not found in that org"))?;
    }
    let t = st
        .store
        .upsert_byo_tool(ByoTool { id: b.id, org_id: b.org_id, base_url: b.base_url, credential_id: b.credential_id, header: b.header, prefix: b.prefix, created_at: Utc::now() })
        .await?;
    Ok(ok(json!(t)))
}

async fn overview_value(st: &AppState) -> Result<Value, ApiError> {
    let stats = st.store.stats().await.map_err(|e| ApiError::internal(e.to_string()))?;
    let mut projects = Vec::new();
    let mut calls = Vec::new();
    for org in st.store.list_orgs().await.map_err(|e| ApiError::internal(e.to_string()))? {
        for p in st.store.list_projects(&org.id).await.map_err(|e| ApiError::internal(e.to_string()))? {
            let spent = st
                .store
                .spend_since(switchyard_store::SpendFilter { project_id: p.id.clone(), since: Some(switchyard_meter::caps::start_of_month(Utc::now())), ..Default::default() })
                .await
                .unwrap_or(0);
            for c in st.store.list_calls(&p.id, None, 10).await.unwrap_or_default() {
                calls.push(json!({ "org": org.slug, "project": p.name, "id": c.call.id, "tool": c.call.tool_id, "provider": c.call.provider_id, "status": c.call.status, "cost_micros": c.call.cost_micros, "cache": c.call.cache, "at": c.call.created_at }));
            }
            projects.push(json!({ "org": org.slug, "project": p.name, "id": p.id, "monthly_cap_micros": p.monthly_cap_micros, "daily_cap_micros": p.daily_cap_micros, "spent_this_month_micros": spent, "policy": p.routing_policy }));
        }
    }
    calls.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
    calls.truncate(50);
    let tools: Vec<Value> = st
        .catalog
        .tools()
        .map(|t| {
            let s = stats.iter().find(|s| s.tool_id == t.id);
            let pct = st.flow.percentiles(&t.id);
            json!({ "id": t.id, "provider": t.provider, "side_effect": t.side_effect.as_str(), "price_micros": t.price.micros, "basis": t.price.basis,
                    "calls": s.map(|s| s.calls), "success_rate": s.map(|s| s.success_rate()), "p50_ms": pct.map(|p| p.0), "p95_ms": pct.map(|p| p.1),
                    "circuit_open": st.flow.is_open(&t.provider) })
        })
        .collect();
    Ok(json!({ "uptime_s": st.started.elapsed().as_secs(), "tools": tools, "projects": projects, "recent_calls": calls, "cache_bytes": st.cache.bytes() }))
}

pub async fn overview(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    require_admin(&st, &headers)?;
    Ok(ok(overview_value(&st).await?))
}

fn table(rows: &[Value], cols: &[(&str, &str)]) -> String {
    let head: String = cols.iter().map(|(_, h)| format!("<th>{}</th>", esc(h))).collect();
    let body: String = rows
        .iter()
        .map(|r| {
            let tds: String = cols
                .iter()
                .map(|(k, _)| {
                    let v = &r[*k];
                    let s = match v {
                        Value::Null => "-".to_string(),
                        Value::String(s) => s.clone(),
                        Value::Number(n) if n.is_f64() => format!("{:.2}", n.as_f64().unwrap_or(0.0)),
                        other => other.to_string(),
                    };
                    format!("<td>{}</td>", esc(&s))
                })
                .collect();
            format!("<tr>{tds}</tr>")
        })
        .collect();
    format!("<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>")
}

pub async fn console(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_ok(&st, &headers) {
        return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Basic realm=\"switchyard-admin\"")], "operator token required").into_response();
    }
    let v = match overview_value(&st).await {
        Ok(v) => v,
        Err(e) => return AppError(e).into_response(),
    };
    let empty = vec![];
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Switchyard admin</title><style>
body{{font:14px/1.45 system-ui,sans-serif;margin:0;background:#f4f5f7;color:#1c2330}}header{{padding:20px 28px;background:#fff;box-shadow:0 1px 0 #e3e7ee}}
h1{{font-size:18px;margin:0}}h2{{font-size:15px;margin:28px 28px 8px}}section{{margin:0 28px;background:#fff;border-radius:12px;overflow:auto;box-shadow:0 4px 16px rgba(20,30,50,.05)}}
table{{border-collapse:collapse;width:100%}}th,td{{text-align:left;padding:8px 12px;border-bottom:1px solid #eef1f5;white-space:nowrap}}th{{color:#718096;font-weight:600}}</style></head>
<body><header><h1>Switchyard: tools, providers, calls and caps</h1><div>uptime {}s · cache {} bytes</div></header>
<h2>Projects and caps</h2><section>{}</section><h2>Recent calls</h2><section>{}</section><h2>Tools and provider stats</h2><section>{}</section></body></html>",
        v["uptime_s"],
        v["cache_bytes"],
        table(v["projects"].as_array().unwrap_or(&empty), &[("org", "Org"), ("project", "Project"), ("policy", "Policy"), ("spent_this_month_micros", "Spent (µ$, month)"), ("monthly_cap_micros", "Monthly cap"), ("daily_cap_micros", "Daily cap")]),
        table(v["recent_calls"].as_array().unwrap_or(&empty), &[("at", "When"), ("org", "Org"), ("tool", "Tool"), ("provider", "Provider"), ("status", "Status"), ("cache", "Cache"), ("cost_micros", "Cost µ$")]),
        table(v["tools"].as_array().unwrap_or(&empty), &[("id", "Tool"), ("provider", "Provider"), ("side_effect", "Effect"), ("price_micros", "Price µ$"), ("calls", "Calls"), ("success_rate", "Success"), ("p50_ms", "p50 ms"), ("p95_ms", "p95 ms"), ("circuit_open", "Circuit open")]),
    );
    Html(html).into_response()
}
