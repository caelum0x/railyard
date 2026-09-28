//! `/v1/tools/*` and `/v1/passthrough/*`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use switchyard_auth::scopes;
use switchyard_catalog::SearchFilters;
use switchyard_core::{new_id, ApiError, HttpRequest, SideEffect};
use switchyard_store::NewCall;

use crate::http_util::{ok, with_headers, ApiResult, AppError};
use crate::pipeline::{self, BatchHold, CallOptions, HoldMode, Outcome};
use crate::principal::{authenticate, Principal};
use crate::state::AppState;

pub async fn principal(st: &AppState, headers: &HeaderMap) -> Result<Principal, AppError> {
    let auds = crate::principal::api_audiences(st);
    let refs: Vec<&str> = auds.iter().map(String::as_str).collect();
    Ok(authenticate(st, headers, &refs).await?)
}

#[derive(Deserialize)]
pub struct SearchQ {
    pub q: String,
    pub side_effect: Option<String>,
    pub max_price_micros: Option<i64>,
    pub limit: Option<usize>,
}

pub async fn search_value(st: &AppState, q: &str, side_effect: Option<&str>, max_price: Option<i64>, limit: Option<usize>) -> Result<Value, ApiError> {
    let side = match side_effect {
        Some(s) => Some(SideEffect::parse(s).ok_or_else(|| ApiError::bad_request("side_effect must be read|write|send|spend"))?),
        None => None,
    };
    let stats = st.store.stats().await.unwrap_or_default();
    let filters = SearchFilters {
        side_effect: side,
        max_price_micros: max_price,
        limit: limit.unwrap_or(10).clamp(1, 50),
        success_rate: stats.iter().map(|s| (s.tool_id.clone(), s.success_rate())).collect(),
    };
    let hits = st.catalog.search(q, &filters);
    Ok(json!({ "query": q, "results": hits }))
}

pub async fn search(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<SearchQ>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    Ok(ok(search_value(&st, &q.q, q.side_effect.as_deref(), q.max_price_micros, q.limit).await?))
}

/// Describe a tool or capability; spends nothing.
pub async fn describe_value(st: &AppState, p: &Principal, id: &str) -> Result<Value, ApiError> {
    let stats: std::collections::HashMap<String, (f64, f64)> =
        st.store.stats().await.unwrap_or_default().into_iter().map(|s| (s.tool_id.clone(), (s.success_rate(), s.avg_latency_ms()))).collect();
    let (kind, capability, tools) = if let Some(t) = st.catalog.tool(id) {
        ("tool", t.capability.clone(), vec![t.clone()])
    } else if let Some(c) = st.catalog.capability(id) {
        ("capability", c.id.clone(), st.catalog.tools_for_capability(&c.id).into_iter().cloned().collect())
    } else {
        return Err(ApiError::not_found(format!("no tool or capability named {id}")));
    };
    let cap = st.catalog.capability(&capability).expect("tool capability exists");
    let example = tools.iter().find_map(|t| t.example.clone());
    let example_input = example.as_ref().map(|e| e.input.clone()).unwrap_or(json!({}));
    // The chain this project would use for the example input.
    let chain = match crate::routing::plan(st, p, id, &example_input, None, &stats).await {
        Ok(plan) => json!({
            "policy": p.project.routing_policy,
            "fallback": plan.fallback,
            "candidates": plan.candidates.iter().map(|c| json!({"tool": c.tool.id, "provider": c.tool.provider, "quote_micros": c.quote, "metered": c.metered})).collect::<Vec<_>>(),
            "skipped": plan.skipped,
        }),
        Err(e) => json!({ "error": e.message }),
    };
    let tool_views: Vec<Value> = tools
        .iter()
        .map(|t| {
            let provider = st.catalog.provider(&t.provider);
            let pct = st.flow.percentiles(&t.id);
            json!({
                "id": t.id,
                "title": t.title,
                "description": t.description,
                "provider": t.provider,
                "provider_kind": provider.map(|p| p.kind.as_str()),
                "side_effect": t.side_effect.as_str(),
                "price": t.price,
                "free": t.price.is_free(),
                "requires_byo_key": t.auth == switchyard_core::AuthMode::ByoRequired,
                "cache_ttl_s": t.cache_ttl_s,
                "idempotent": t.idempotent,
                "bill_on_4xx": t.bill_on_4xx,
                "input_schema": st.catalog.input_schema(&t.id),
                "output_schema": st.catalog.output_schema(&t.id),
                "example": t.example,
                "stats": stats.get(&t.id).map(|(r, l)| json!({"success_rate": r, "avg_latency_ms": l, "p50_ms": pct.map(|x| x.0), "p95_ms": pct.map(|x| x.1)})),
            })
        })
        .collect();
    Ok(json!({
        "id": id,
        "kind": kind,
        "capability": { "id": cap.id, "title": cap.title, "description": cap.description },
        "input_schema": st.catalog.input_schema(id),
        "output_schema": st.catalog.output_schema(id),
        "tools": tool_views,
        "example": example,
        "routing": chain,
    }))
}

pub async fn describe(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::READ)?;
    Ok(ok(describe_value(&st, &p, &id).await?))
}

#[derive(Deserialize)]
pub struct CallBody {
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub options: CallOptions,
}

pub fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn outcome_response(o: Outcome) -> Response {
    let resp = Json(o.body).into_response();
    with_headers(
        resp,
        &[
            ("x-switchyard-call-id", o.call_id),
            ("x-switchyard-provider", o.provider.unwrap_or_default()),
            ("x-switchyard-cost-micros", o.cost_micros.to_string()),
        ],
    )
}

pub async fn call(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    let body: CallBody = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("body must be {{\"input\": {{...}}, \"options\": {{...}}}}: {e}")))?;
    let input = if body.input.is_null() { json!({}) } else { body.input };
    let o = pipeline::call(&st, &p, &id, input, body.options, idem_key(&headers), HoldMode::Own).await?;
    Ok(outcome_response(o))
}

#[derive(Deserialize)]
pub struct BatchItem {
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub options: CallOptions,
}

#[derive(Deserialize)]
pub struct BatchBody {
    pub calls: Vec<BatchItem>,
}

pub const MAX_BATCH: usize = 50;

/// Up to 50 calls under one hold; results in order.
pub async fn batch(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::CALL)?;
    let body: BatchBody = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("body must be {{\"calls\": [...]}}: {e}")))?;
    if body.calls.is_empty() || body.calls.len() > MAX_BATCH {
        return Err(ApiError::bad_request(format!("a batch holds 1..{MAX_BATCH} calls")).into());
    }
    let stats = std::collections::HashMap::new();
    let mut total = 0i64;
    let mut any_send = false;
    for (i, c) in body.calls.iter().enumerate() {
        st.catalog.validate_input(&c.tool, &c.input).map_err(|errs| {
            let e = ApiError::validation(errs);
            ApiError::new(422, "invalid_input", format!("call {i}: {}", e.message)).with_details(json!({ "index": i, "errors": e.details["errors"] }))
        })?;
        let plan = crate::routing::plan(&st, &p, &c.tool, &c.input, c.options.provider.as_deref(), &stats).await?;
        total += plan.hold_amount().max(if plan.candidates.iter().any(|x| x.metered) { c.options.max_cost_micros.unwrap_or(0) } else { 0 });
        any_send |= plan.side_effect() == SideEffect::Send;
    }
    let batch_id = new_id("batch");
    let _guard = st.caps.check_and_reserve(st.store.as_ref(), &p.project, &p.key, "batch", any_send, total, Utc::now()).await?;
    let hold_id = if total > 0 { Some(pipeline::place_hold(&st, &p, total, &batch_id).await?) } else { None };
    let bh = Arc::new(BatchHold { hold_id: hold_id.clone(), spent: std::sync::Mutex::new(0) });
    let idem = idem_key(&headers);
    let mut results = Vec::with_capacity(body.calls.len());
    for (i, c) in body.calls.into_iter().enumerate() {
        let key = idem.as_ref().map(|k| format!("{k}:{i}"));
        let input = if c.input.is_null() { json!({}) } else { c.input };
        results.push(match pipeline::call(&st, &p, &c.tool, input, c.options, key, HoldMode::Batch(bh.clone())).await {
            Ok(o) => o.body,
            Err(e) => e.body(),
        });
    }
    let spent = *bh.spent.lock().expect("batch poisoned");
    let mut outbox = Vec::new();
    let receipt = match &hold_id {
        Some(h) => pipeline::settle_hold(&st, h, spent, &batch_id, &mut outbox).await,
        None => None,
    };
    if !outbox.is_empty() {
        // Durable retry for a settlement Mint could not take right now.
        st.store
            .record_call(
                NewCall {
                    id: batch_id.clone(),
                    project_id: p.project.id.clone(),
                    org_id: p.org.id.clone(),
                    key_public_id: p.key.public_id.clone(),
                    target: "batch".into(),
                    tool_id: None,
                    provider_id: None,
                    side_effect: "read".into(),
                    idempotency_key: idem.clone(),
                    request_hash: batch_id.clone(),
                    status: "ok".into(),
                    http_status: 200,
                    latency_ms: 0,
                    cost_micros: 0,
                    price_basis: "batch_settlement".into(),
                    mint_hold_id: hold_id.clone(),
                    mint_receipt: None,
                    rago_event_id: None,
                    cache: "none".into(),
                    fallback_chain: json!([]),
                    created_at: Utc::now(),
                },
                outbox,
                None,
            )
            .await?;
    }
    Ok(with_headers(
        Json(json!({ "ok": true, "data": results, "meta": { "batch_id": batch_id, "hold_id": hold_id, "held_micros": total, "cost_micros": spent, "receipt_id": receipt } })).into_response(),
        &[("x-switchyard-call-id", batch_id), ("x-switchyard-cost-micros", spent.to_string())],
    ))
}

/// Faithful relay for BYO tools only (admin-registered base URL + credential).
pub async fn passthrough(State(st): State<AppState>, method: Method, headers: HeaderMap, Path((tool_id, rest)): Path<(String, String)>, uri: axum::http::Uri, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(if method == Method::GET || method == Method::HEAD { scopes::CALL } else { scopes::WRITE })?;
    if !p.tool_allowed(&[&tool_id]) {
        return Err(ApiError::forbidden("not in this key's tool allowlist").into());
    }
    let t = st.store.get_byo_tool(&p.org.id, &tool_id).await?.ok_or_else(|| ApiError::not_found(format!("no BYO tool {tool_id}")))?;
    let mut target = format!("{}/{}", t.base_url.trim_end_matches('/'), rest.trim_start_matches('/'));
    if let Some(q) = uri.query() {
        target.push('?');
        target.push_str(q);
    }
    let host = url::Url::parse(&target).ok().and_then(|u| u.host_str().map(str::to_string)).ok_or_else(|| ApiError::bad_request("bad target URL"))?;
    let mut req = HttpRequest::get(&target);
    req.method = method.as_str().into();
    req.extra_hosts = vec![host];
    for name in ["content-type", "accept"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            req.headers.push((name.into(), v.into()));
        }
    }
    if !body.is_empty() {
        req.body = Some(body.to_vec());
    }
    if let Some(cid) = &t.credential_id {
        let cred = crate::vaultops::open_by_id(&st, &p.org.id, cid).await?;
        req.sensitive_headers.push((t.header.clone(), format!("{}{}", t.prefix, cred.expose())));
    }
    let call_id = new_id("call");
    let t0 = std::time::Instant::now();
    let res = st.http.send(req).await;
    let (status, resp) = match res {
        Ok(r) => (r.status, Ok(r)),
        Err(e) => (0, Err(e)),
    };
    st.store
        .record_call(
            NewCall {
                id: call_id.clone(),
                project_id: p.project.id.clone(),
                org_id: p.org.id.clone(),
                key_public_id: p.key.public_id.clone(),
                target: format!("byo:{tool_id}"),
                tool_id: Some(format!("byo:{tool_id}")),
                provider_id: Some("byo".into()),
                side_effect: if method == Method::GET { "read".into() } else { "write".into() },
                idempotency_key: None,
                request_hash: switchyard_core::hash::sha256_hex(format!("{method} {target}").as_bytes()),
                status: if (200..400).contains(&status) { "ok".into() } else { "failed".into() },
                http_status: status as i32,
                latency_ms: t0.elapsed().as_millis() as i64,
                cost_micros: 0,
                price_basis: "byo".into(),
                mint_hold_id: None,
                mint_receipt: None,
                rago_event_id: None,
                cache: "none".into(),
                fallback_chain: json!([]),
                created_at: Utc::now(),
            },
            vec![],
            None,
        )
        .await?;
    let r = resp.map_err(|e| ApiError::new(502, "upstream_unreachable", e.to_string()))?;
    let mut out = axum::http::Response::builder().status(r.status);
    if let Some(ct) = r.header("content-type") {
        out = out.header("content-type", ct);
    }
    let out = out.header("x-switchyard-call-id", call_id).header("x-switchyard-provider", "byo").header("x-switchyard-cost-micros", "0");
    Ok(out.body(axum::body::Body::from(r.body)).map_err(|e| ApiError::internal(e.to_string()))?.into_response())
}
