//! Keys, credentials (write-only), bundles, project settings, usage, audit
//! and balance.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use chrono::{DateTime, Duration, Utc};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};
use switchyard_auth::scopes;
use switchyard_core::hash::verify_chain;
use switchyard_core::{ApiError, CredentialKind};
use switchyard_meter::caps::{start_of_day, start_of_month};
use switchyard_store::{ApiKey, BundleRow, GroupBy, SpendFilter};

use super::tools::principal;
use crate::http_util::{ok, ApiResult};
use crate::principal::Principal;
use crate::state::AppState;

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

fn key_view(k: &ApiKey) -> Value {
    json!({
        "id": k.public_id,
        "project_id": k.project_id,
        "description": k.description,
        "scopes": k.scopes,
        "tool_allow": k.tool_allow,
        "per_key_daily_cap_micros": k.per_key_daily_cap_micros,
        "revoked_at": k.revoked_at,
        "created_at": k.created_at,
    })
}

// ------------------------------------------------------------------ keys

pub async fn list_keys(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let keys = st.store.list_api_keys(&p.project.id).await?;
    Ok(ok(json!(keys.iter().map(key_view).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct CreateKey {
    #[serde(default)]
    pub description: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,
    #[serde(default)]
    pub per_key_daily_cap_micros: Option<i64>,
}

fn check_scopes(requested: &[String]) -> Result<Vec<String>, ApiError> {
    let parsed = scopes::parse(&requested.join(" "));
    if parsed.len() != requested.len() || parsed.is_empty() {
        return Err(ApiError::bad_request(format!("scopes must be a non-empty subset of {:?}", scopes::ALL)));
    }
    Ok(parsed)
}

fn check_tools(st: &AppState, ids: &Option<Vec<String>>) -> Result<(), ApiError> {
    for id in ids.iter().flatten() {
        if st.catalog.tool(id).is_none() && st.catalog.capability(id).is_none() {
            return Err(ApiError::bad_request(format!("unknown tool or capability {id}")));
        }
    }
    Ok(())
}

/// Mint a key in Relay's key store (one key for both rails) and bind it here.
pub async fn create_key_for(st: &AppState, org_slug: &str, project_id: &str, body: CreateKey) -> Result<(String, ApiKey), ApiError> {
    let scopes = check_scopes(&body.scopes)?;
    check_tools(st, &body.tool_allow)?;
    let (raw, public_id) = st.keys.mint(org_slug, project_id, &body.description).await?;
    let key = ApiKey {
        public_id,
        project_id: project_id.into(),
        description: body.description,
        scopes,
        tool_allow: body.tool_allow,
        per_key_daily_cap_micros: body.per_key_daily_cap_micros,
        extra_projects: vec![],
        revoked_at: None,
        created_at: Utc::now(),
    };
    st.store.insert_api_key(key.clone()).await.map_err(|e| ApiError::internal(e.to_string()))?;
    Ok((raw.expose_secret().to_string(), key))
}

pub async fn create_key(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let (raw, key) = create_key_for(&st, &p.org.slug, &p.project.id, parse(&body)?).await?;
    let mut v = key_view(&key);
    v["key"] = json!(raw);
    v["note"] = json!("Shown once. The same key works on Relay (/openai/v1) and Switchyard (/v1/tools).");
    Ok((axum::http::StatusCode::CREATED, axum::Json(json!({ "ok": true, "data": v }))).into_response())
}

async fn own_key(st: &AppState, p: &Principal, id: &str) -> Result<ApiKey, ApiError> {
    st.store
        .get_api_key(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .filter(|k| k.project_id == p.project.id)
        .ok_or_else(|| ApiError::not_found("key not found in this project"))
}

pub async fn revoke_key(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let mut k = own_key(&st, &p, &id).await?;
    st.keys.disable(&id).await?;
    k.revoked_at = Some(Utc::now());
    let k = st.store.update_api_key(k).await?;
    Ok(ok(key_view(&k)))
}

#[derive(Deserialize)]
pub struct PatchKey {
    pub description: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub tool_allow: Option<Option<Vec<String>>>,
    pub per_key_daily_cap_micros: Option<Option<i64>>,
}

pub async fn patch_key(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let body: PatchKey = parse(&body)?;
    let mut k = own_key(&st, &p, &id).await?;
    if let Some(d) = body.description {
        k.description = d;
    }
    if let Some(s) = body.scopes {
        k.scopes = check_scopes(&s)?;
    }
    if let Some(t) = body.tool_allow {
        check_tools(&st, &t)?;
        k.tool_allow = t;
    }
    if let Some(c) = body.per_key_daily_cap_micros {
        k.per_key_daily_cap_micros = c;
    }
    let k = st.store.update_api_key(k).await?;
    Ok(ok(key_view(&k)))
}

// ------------------------------------------------------------ credentials

#[derive(Deserialize)]
pub struct CreateCred {
    pub provider: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    pub value: String,
    #[serde(default)]
    pub meta: Option<Value>,
}

fn cred_view(c: &switchyard_store::CredentialRow) -> Value {
    json!({ "id": c.id, "provider": c.provider_id, "name": c.name, "kind": c.kind, "meta": c.meta, "created_at": c.created_at, "last_ok_at": c.last_ok_at, "last_error": c.last_error })
}

pub async fn create_credential(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let b: CreateCred = parse(&body)?;
    if st.catalog.provider(&b.provider).is_none() && b.provider != "byo" {
        return Err(ApiError::bad_request(format!("unknown provider {}", b.provider)).into());
    }
    let kind = CredentialKind::parse(b.kind.as_deref().unwrap_or("api_key")).ok_or_else(|| ApiError::bad_request("kind must be api_key|oauth|basic|header_set"))?;
    let name = b.name.unwrap_or_else(|| b.provider.clone());
    let row = crate::vaultops::store_credential(&st, &p.org.id, &b.provider, &name, kind, &b.value, b.meta.unwrap_or(json!({}))).await?;
    Ok((axum::http::StatusCode::CREATED, axum::Json(json!({ "ok": true, "data": cred_view(&row) }))).into_response())
}

pub async fn list_credentials(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    let rows = st.store.list_credentials(&p.org.id).await?;
    Ok(ok(json!(rows.iter().map(cred_view).collect::<Vec<_>>())))
}

pub async fn delete_credential(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    if !st.store.delete_credential(&p.org.id, &id).await? {
        return Err(ApiError::not_found("credential not found").into());
    }
    Ok(ok(json!({ "deleted": id })))
}

// --------------------------------------------------------------- bundles

#[derive(Deserialize)]
pub struct PushBundle {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub skill_md: String,
    pub tool_ids: Vec<String>,
    #[serde(default)]
    pub credential_refs: Vec<String>,
}

/// Refuse anything that looks like a secret: bundles are shared with agents.
pub fn looks_secret(text: &str) -> Option<&'static str> {
    const MARKERS: &[(&str, &str)] = &[
        ("sk-t0-", "Relay key"),
        ("sk-ant-", "Anthropic key"),
        ("sk_live_", "Stripe live key"),
        ("ghp_", "GitHub token"),
        ("github_pat_", "GitHub token"),
        ("xoxb-", "Slack token"),
        ("AKIA", "AWS access key"),
        ("-----BEGIN", "private key"),
        ("scb_agt_", "Scribase agent token"),
        ("rly_", "Rally token"),
    ];
    MARKERS.iter().find(|(m, _)| text.contains(m)).map(|(_, what)| *what)
}

fn bundle_view(b: &BundleRow, public: bool) -> Value {
    json!({ "id": b.id, "name": b.name, "description": b.description, "tool_ids": b.tool_ids, "credential_refs": b.credential_refs, "version": b.version, "public": public, "updated_at": b.updated_at })
}

pub async fn push_bundle(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let b: PushBundle = parse(&body)?;
    if b.id.is_empty() || b.id.len() > 64 || !b.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(ApiError::bad_request("bundle id must be 1-64 chars of [A-Za-z0-9_-]").into());
    }
    if b.skill_md.len() > 256 * 1024 {
        return Err(ApiError::bad_request("SKILL.md is limited to 256 KiB").into());
    }
    if let Some(what) = looks_secret(&b.skill_md) {
        return Err(ApiError::bad_request(format!("SKILL.md appears to contain a {what}; bundles refer to credentials by provider name only")).into());
    }
    check_tools(&st, &Some(b.tool_ids.clone()))?;
    for r in &b.credential_refs {
        if st.catalog.provider(r).is_none() {
            return Err(ApiError::bad_request(format!("credential_refs names unknown provider {r}")).into());
        }
    }
    let row = st
        .store
        .upsert_bundle(BundleRow {
            id: b.id,
            org_id: p.org.id.clone(),
            name: b.name,
            description: b.description,
            skill_md: b.skill_md,
            tool_ids: b.tool_ids,
            credential_refs: b.credential_refs,
            version: 1,
            updated_at: Utc::now(),
        })
        .await?;
    Ok(ok(bundle_view(&row, false)))
}

pub async fn list_bundles(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    let mut out: Vec<Value> = st.store.list_bundles(&p.org.id).await?.iter().map(|b| bundle_view(b, false)).collect();
    for b in st.catalog.bundles() {
        if !out.iter().any(|x| x["id"] == json!(b.id)) {
            out.push(json!({ "id": b.id, "name": b.name, "description": b.description, "tool_ids": b.tool_ids, "credential_refs": b.credential_refs, "version": b.version, "public": true }));
        }
    }
    Ok(ok(json!(out)))
}

pub async fn bundle_skill_text(st: &AppState, org_id: &str, id: &str) -> Result<String, ApiError> {
    if let Some(b) = st.store.get_bundle(org_id, id).await.map_err(|e| ApiError::internal(e.to_string()))? {
        return Ok(b.skill_md);
    }
    st.catalog.bundle(id).map(|b| b.skill_md.clone()).ok_or_else(|| ApiError::not_found(format!("no bundle {id}")))
}

pub async fn bundle_skill(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    let text = bundle_skill_text(&st, &p.org.id, &id).await?;
    Ok(([("content-type", "text/markdown; charset=utf-8")], text).into_response())
}

// --------------------------------------------------------------- project

pub fn project_view(p: &switchyard_store::Project) -> Value {
    json!({
        "id": p.id, "org_id": p.org_id, "name": p.name,
        "monthly_cap_micros": p.monthly_cap_micros, "daily_cap_micros": p.daily_cap_micros,
        "tool_daily_caps": p.tool_daily_caps, "routing_policy": p.routing_policy,
        "pinned_tools": p.pinned_tools, "daily_send_limit": p.daily_send_limit, "egress_extra_hosts": p.egress_extra_hosts,
    })
}

pub async fn get_project(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    Ok(ok(project_view(&p.project)))
}

#[derive(Deserialize)]
pub struct PatchProject {
    pub name: Option<String>,
    pub monthly_cap_micros: Option<Option<i64>>,
    pub daily_cap_micros: Option<Option<i64>>,
    pub tool_daily_caps: Option<std::collections::BTreeMap<String, i64>>,
    pub routing_policy: Option<String>,
    pub pinned_tools: Option<Vec<String>>,
    pub daily_send_limit: Option<Option<i64>>,
}

pub const MAX_PINNED: usize = 20;

pub async fn patch_project(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let b: PatchProject = parse(&body)?;
    let mut pr = p.project.clone();
    if let Some(n) = b.name {
        pr.name = n;
    }
    for (v, _) in [(&b.monthly_cap_micros, "monthly"), (&b.daily_cap_micros, "daily"), (&b.daily_send_limit, "sends")] {
        if let Some(Some(x)) = v {
            if *x < 0 {
                return Err(ApiError::bad_request("caps must be >= 0").into());
            }
        }
    }
    if let Some(c) = b.monthly_cap_micros {
        pr.monthly_cap_micros = c;
    }
    if let Some(c) = b.daily_cap_micros {
        pr.daily_cap_micros = c;
    }
    if let Some(c) = b.daily_send_limit {
        pr.daily_send_limit = c;
    }
    if let Some(t) = b.tool_daily_caps {
        check_tools(&st, &Some(t.keys().cloned().collect()))?;
        pr.tool_daily_caps = t;
    }
    if let Some(pol) = b.routing_policy {
        if !["free_first", "cheapest", "fastest", "pinned"].contains(&pol.as_str()) {
            return Err(ApiError::bad_request("routing_policy must be free_first|cheapest|fastest|pinned").into());
        }
        pr.routing_policy = pol;
    }
    if let Some(pins) = b.pinned_tools {
        if pins.len() > MAX_PINNED {
            return Err(ApiError::bad_request(format!("at most {MAX_PINNED} pinned tools")).into());
        }
        check_tools(&st, &Some(pins.clone()))?;
        pr.pinned_tools = pins;
    }
    let pr = st.store.update_project(pr).await?;
    Ok(ok(project_view(&pr)))
}

// ----------------------------------------------------------------- usage

#[derive(Deserialize)]
pub struct UsageQ {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub group_by: Option<String>,
}

pub async fn usage_value(st: &AppState, p: &Principal, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>, group_by: Option<&str>) -> Result<Value, ApiError> {
    let to = to.unwrap_or_else(|| Utc::now() + Duration::seconds(1));
    let from = from.unwrap_or_else(|| to - Duration::days(30));
    let g = GroupBy::parse(group_by.unwrap_or("tool")).ok_or_else(|| ApiError::bad_request("group_by must be tool|provider|key|day"))?;
    let rows = st.store.usage(&p.project.id, from, to, g).await.map_err(|e| ApiError::internal(e.to_string()))?;
    let total: i64 = rows.iter().map(|r| r.cost_micros).sum();
    let calls: i64 = rows.iter().map(|r| r.calls).sum();
    Ok(json!({ "project_id": p.project.id, "from": from, "to": to, "group_by": group_by.unwrap_or("tool"), "rows": rows, "total_cost_micros": total, "total_calls": calls }))
}

pub async fn usage(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<UsageQ>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    Ok(ok(usage_value(&st, &p, q.from, q.to, q.group_by.as_deref()).await?))
}

#[derive(Deserialize)]
pub struct CallsQ {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_calls(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<CallsQ>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows = st.store.list_calls(&p.project.id, q.cursor.as_deref(), limit).await?;
    let next = if rows.len() as i64 == limit { rows.last().map(|r| r.call.id.clone()) } else { None };
    Ok(ok(json!({ "calls": rows, "next_cursor": next })))
}

/// One call with its receipt and a verified hash-chain proof.
pub async fn get_call(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    let row = st.store.get_call(&p.project.id, &id).await?.ok_or_else(|| ApiError::not_found("call not found"))?;
    let chain = st.store.chain(&p.project.id, row.seq).await?;
    let payloads: Vec<Value> = chain.iter().map(|r| r.call.hash_payload(r.seq)).collect();
    let verified = verify_chain(chain.iter().zip(payloads.iter()).map(|(r, v)| (r.prev_hash.as_str(), r.row_hash.as_str(), v)));
    Ok(ok(json!({
        "call": row,
        "proof": {
            "algorithm": "sha256(prev_hash || \"\\n\" || canonical_json(row))",
            "chain_length": chain.len(),
            "verified": verified.is_ok(),
            "first_bad_index": verified.err(),
            "hash_payload": row.call.hash_payload(row.seq),
        }
    })))
}

// --------------------------------------------------------------- balance

pub async fn balance_value(st: &AppState, p: &Principal) -> Result<Value, ApiError> {
    let now = Utc::now();
    let spent = |since| {
        let f = SpendFilter { project_id: p.project.id.clone(), since: Some(since), ..Default::default() };
        async move { st.store.spend_since(f).await.map_err(|e| ApiError::internal(e.to_string())) }
    };
    let (month, day) = (spent(start_of_month(now)).await?, spent(start_of_day(now)).await?);
    let mint = match (&st.mint, &p.org.mint_account_id) {
        (Some(m), Some(a)) => match m.balance(a).await {
            Ok(b) => json!({ "account_id": a, "available_micros": b.available_micros, "held_micros": b.held_micros }),
            Err(e) => json!({ "account_id": a, "error": e.to_string() }),
        },
        _ => json!(null),
    };
    Ok(json!({
        "balance": mint,
        "project": {
            "spent_today_micros": day,
            "spent_this_month_micros": month,
            "daily_cap_micros": p.project.daily_cap_micros,
            "monthly_cap_micros": p.project.monthly_cap_micros,
            "daily_remaining_micros": p.project.daily_cap_micros.map(|c| (c - day).max(0)),
            "monthly_remaining_micros": p.project.monthly_cap_micros.map(|c| (c - month).max(0)),
        },
        "topup_url": format!("{}/v1/balance/topup", st.settings.public_url),
    }))
}

pub async fn balance(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    Ok(ok(balance_value(&st, &p).await?))
}

#[derive(Deserialize)]
pub struct Topup {
    pub amount_micros: i64,
}

/// Start a credit top-up (Dodo checkout through Mint).
pub async fn topup(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let b: Topup = parse(&body)?;
    if !(1_000_000..=10_000_000_000).contains(&b.amount_micros) {
        return Err(ApiError::bad_request("amount_micros must be between 1000000 ($1) and 10000000000 ($10,000)").into());
    }
    let (Some(mint), Some(account)) = (&st.mint, &p.org.mint_account_id) else {
        return Err(ApiError::new(503, "billing_unavailable", "billing is not configured for this org").into());
    };
    let url = mint
        .checkout(account, b.amount_micros, &st.settings.topup_provider, st.settings.topup_success_url.as_deref())
        .await
        .map_err(|e| crate::pipeline::mint_error(&st, e, b.amount_micros, None))?;
    Ok(ok(json!({ "checkout_url": url, "provider": st.settings.topup_provider, "amount_micros": b.amount_micros })))
}
