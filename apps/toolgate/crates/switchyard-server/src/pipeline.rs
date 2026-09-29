//! The call pipeline (spec §4.5), one function end to end:
//! authenticate (caller) → resolve → validate → idempotency → cache and
//! single-flight → caps and hold → execute the chain → settle → record.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use switchyard_auth::scopes;
use switchyard_cache::CachedResponse;
use switchyard_core::hash::request_hash;
use switchyard_core::{new_id, AdapterError, ApiError, CallCtx, ErrorClass, ProviderKind, SideEffect};
use switchyard_meter::{CapScope, MintError};
use switchyard_store::{IdempotencyRow, NewCall, OutboxItem};

use crate::principal::{Principal, Via};
use crate::routing::{self, Plan};
use crate::state::AppState;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CallOptions {
    /// Pin a provider (or a concrete tool id); disables fallback.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub max_cost_micros: Option<i64>,
    /// `default` | `bypass` | `only`
    #[serde(default)]
    pub cache: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// A finished call, shared with single-flight followers.
#[derive(Clone, Debug)]
pub struct Executed {
    pub call_id: String,
    pub tool_id: String,
    pub provider: String,
    pub data: Value,
    pub cost_micros: i64,
    pub cache: &'static str,
    pub fallback_chain: Value,
    pub hold_id: Option<String>,
    pub receipt_id: Option<String>,
    pub units: Option<u64>,
}

pub type Shared = Result<Executed, ApiError>;

impl Executed {
    pub fn body(&self) -> Value {
        json!({
            "ok": true,
            "data": self.data,
            "meta": {
                "call_id": self.call_id,
                "tool": self.tool_id,
                "provider": self.provider,
                "cost_micros": self.cost_micros,
                "cache": self.cache,
                "fallback_chain": self.fallback_chain,
                "hold_id": self.hold_id,
                "receipt_id": self.receipt_id,
                "units": self.units,
            }
        })
    }
}

/// One hold shared by a batch; calls settle into it and the batch captures once.
pub struct BatchHold {
    pub hold_id: Option<String>,
    pub spent: Mutex<i64>,
}

#[derive(Clone)]
pub enum HoldMode {
    Own,
    Batch(Arc<BatchHold>),
}

/// The HTTP-facing outcome.
pub struct Outcome {
    pub body: Value,
    pub call_id: String,
    pub provider: Option<String>,
    pub cost_micros: i64,
}

fn topup_url(st: &AppState) -> String {
    format!("{}/v1/balance/topup", st.settings.public_url)
}

async fn stats_map(st: &AppState) -> HashMap<String, (f64, f64)> {
    st.store.stats().await.unwrap_or_default().into_iter().map(|s| (s.tool_id.clone(), (s.success_rate(), s.avg_latency_ms()))).collect()
}

/// Run one call.
pub async fn call(st: &AppState, p: &Principal, target: &str, input: Value, opts: CallOptions, idem: Option<String>, mode: HoldMode) -> Result<Outcome, ApiError> {
    let now = Utc::now();
    p.require(scopes::CALL)?;
    // Validate before any money moves.
    st.catalog.validate_input(target, &input).map_err(ApiError::validation)?;
    let req_hash = request_hash(target, &input);

    // Idempotent replay.
    let mut _claim = None;
    if let Some(k) = &idem {
        if k.len() > 200 {
            return Err(ApiError::bad_request("Idempotency-Key is too long (max 200)"));
        }
        _claim = Some(st.idem_inflight.claim(&p.project.id, k).ok_or_else(|| {
            ApiError::new(409, "idempotency_in_progress", "A request with this Idempotency-Key is still running; retry after it finishes to get its stored result.")
        })?);
        if let Some(prev) = st.store.get_idempotency(&p.project.id, k, now).await.map_err(|e| ApiError::internal(e.to_string()))? {
            if prev.request_hash != req_hash {
                return Err(ApiError::idempotency_conflict());
            }
            return replay(st, p, target, &req_hash, k, prev.response).await;
        }
    }

    let stats = stats_map(st).await;
    let plan = routing::plan(st, p, target, &input, opts.provider.as_deref(), &stats).await?;
    if plan.candidates.is_empty() {
        let all_scope = !plan.skipped.is_empty() && plan.skipped.iter().all(|s| s.reason.starts_with("needs scope") || s.reason == "not in this key's tool allowlist");
        let details = json!({ "skipped": plan.skipped });
        return Err(if all_scope {
            ApiError::forbidden("your credential is not allowed to use this tool (scope or tool allowlist)").with_details(details)
        } else {
            ApiError::new(422, "no_provider", "No provider can serve this input with your credentials.").with_details(details)
        });
    }
    let side = plan.side_effect();
    if let Via::OAuth { family, .. } = &p.via {
        if matches!(side, SideEffect::Send | SideEffect::Spend) {
            let tool = &plan.candidates[0].tool.id;
            if !st.store.has_consent(family, tool).await.map_err(|e| ApiError::internal(e.to_string()))? {
                let url = format!("{}/oauth/consent?tool={}&grant={}&org={}", st.settings.public_url, tool, family, p.org.id);
                return Err(ApiError::new(403, "consent_required", format!("{tool} can {} on your behalf; approve it once at the consent URL.", side.as_str()))
                    .with_details(json!({ "consent_url": url, "tool": tool })));
            }
        }
    }

    let any_metered = plan.candidates.iter().any(|c| c.metered);
    let hold_amount = plan.hold_amount().max(if any_metered { opts.max_cost_micros.unwrap_or(0) } else { 0 });
    if opts.dry_run {
        return Ok(Outcome {
            body: json!({ "ok": true, "data": null, "meta": {
                "dry_run": true,
                "quote_micros": hold_amount,
                "candidates": plan.candidates.iter().map(|c| json!({"tool": c.tool.id, "provider": c.tool.provider, "quote_micros": c.quote, "metered": c.metered})).collect::<Vec<_>>(),
                "skipped": plan.skipped,
            }}),
            call_id: String::new(),
            provider: plan.candidates.first().map(|c| c.tool.provider.clone()),
            cost_micros: 0,
        });
    }

    let cache_mode = opts.cache.as_deref().unwrap_or("default");
    let cacheable = side == SideEffect::Read && matches!(mode, HoldMode::Own) && cache_mode != "bypass" && plan.candidates.iter().all(|c| c.tool.cache_ttl_s > 0);
    let shared_scope = plan.candidates.iter().all(|c| c.kind == ProviderKind::Public && c.credential.is_none());
    let cache_key = format!("{}|{}|{}", if shared_scope { "pub" } else { p.org.id.as_str() }, opts.provider.as_deref().unwrap_or("*"), req_hash);
    if cacheable || cache_mode == "only" {
        if let Some(hit) = st.cache.get(&cache_key) {
            let call_id = new_id("call");
            let ex = Executed {
                call_id: call_id.clone(),
                tool_id: hit.tool_id.clone(),
                provider: hit.provider.clone(),
                data: hit.data.clone(),
                cost_micros: 0,
                cache: "hit",
                fallback_chain: json!([]),
                hold_id: None,
                receipt_id: None,
                units: None,
            };
            record_shadow(st, p, target, &req_hash, &ex, side).await?;
            return Ok(to_outcome(ex));
        }
        if cache_mode == "only" {
            return Err(ApiError::not_found("no cached response for this input (cache: only)").with_details(json!({ "code": "cache_miss" })));
        }
    }

    if cacheable {
        let (res, deduped) = st.flights.run(&cache_key, || execute(st, p, &plan, &input, hold_amount, idem.clone(), &req_hash, mode.clone(), Some(&cache_key), "miss")).await;
        let ex = res?;
        if deduped {
            let mine = Executed { call_id: new_id("call"), cost_micros: 0, cache: "dedupe", hold_id: None, receipt_id: None, ..ex };
            record_shadow(st, p, target, &req_hash, &mine, side).await?;
            return Ok(to_outcome(mine));
        }
        return Ok(to_outcome(ex));
    }
    let label = if cache_mode == "bypass" { "bypass" } else { "none" };
    let ex = execute(st, p, &plan, &input, hold_amount, idem, &req_hash, mode, None, label).await?;
    Ok(to_outcome(ex))
}

fn to_outcome(ex: Executed) -> Outcome {
    Outcome { body: ex.body(), call_id: ex.call_id.clone(), provider: Some(ex.provider.clone()), cost_micros: ex.cost_micros }
}

async fn replay(st: &AppState, p: &Principal, target: &str, req_hash: &str, key: &str, stored: Value) -> Result<Outcome, ApiError> {
    let call_id = new_id("call");
    let original = stored.pointer("/meta/call_id").cloned().unwrap_or(Value::Null);
    let mut body = stored;
    if let Some(meta) = body.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert("replayed_call_id".into(), original);
        meta.insert("call_id".into(), json!(call_id));
        meta.insert("cache".into(), json!("replay"));
        meta.insert("cost_micros".into(), json!(0));
    }
    let provider = body.pointer("/meta/provider").and_then(Value::as_str).map(str::to_string);
    let tool = body.pointer("/meta/tool").and_then(Value::as_str).map(str::to_string);
    let side = tool.as_deref().and_then(|t| st.catalog.tool(t)).map(|t| t.side_effect.as_str()).unwrap_or("read");
    st.store
        .record_call(
            NewCall {
                id: call_id.clone(),
                project_id: p.project.id.clone(),
                org_id: p.org.id.clone(),
                key_public_id: p.key.public_id.clone(),
                target: target.into(),
                tool_id: tool,
                provider_id: provider.clone(),
                side_effect: side.into(),
                idempotency_key: Some(key.into()),
                request_hash: req_hash.into(),
                status: "ok".into(),
                http_status: 200,
                latency_ms: 0,
                cost_micros: 0,
                price_basis: "replay".into(),
                mint_hold_id: None,
                mint_receipt: None,
                rago_event_id: None,
                cache: "replay".into(),
                fallback_chain: json!([]),
                created_at: Utc::now(),
            },
            vec![],
            None,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Outcome { body, call_id, provider, cost_micros: 0 })
}

/// Audit row for a zero-cost cache hit or single-flight follower.
async fn record_shadow(st: &AppState, p: &Principal, target: &str, req_hash: &str, ex: &Executed, side: SideEffect) -> Result<(), ApiError> {
    st.store
        .record_call(
            NewCall {
                id: ex.call_id.clone(),
                project_id: p.project.id.clone(),
                org_id: p.org.id.clone(),
                key_public_id: p.key.public_id.clone(),
                target: target.into(),
                tool_id: Some(ex.tool_id.clone()),
                provider_id: Some(ex.provider.clone()),
                side_effect: side.as_str().into(),
                idempotency_key: None,
                request_hash: req_hash.into(),
                status: "ok".into(),
                http_status: 200,
                latency_ms: 0,
                cost_micros: 0,
                price_basis: ex.cache.into(),
                mint_hold_id: None,
                mint_receipt: None,
                rago_event_id: None,
                cache: ex.cache.into(),
                fallback_chain: json!([]),
                created_at: Utc::now(),
            },
            vec![],
            None,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

pub fn mint_error(st: &AppState, e: MintError, amount: i64, balance: Option<i64>) -> ApiError {
    match e {
        MintError::InsufficientFunds => ApiError::payment_required(balance.unwrap_or(0), amount, &topup_url(st)),
        MintError::AccountFrozen => ApiError::new(402, "account_frozen", "The billing account is frozen. Contact support."),
        MintError::AccountNotFound => ApiError::payment_required(0, amount, &topup_url(st)),
        MintError::Unavailable(m) => {
            tracing::error!(error = %m, "mint unavailable");
            ApiError::new(503, "billing_unavailable", "Billing is temporarily unavailable; nothing was charged. Retry shortly.")
        }
        other => {
            tracing::error!(error = %other, "mint error");
            ApiError::new(502, "billing_error", "Billing rejected the hold; nothing was charged.")
        }
    }
}

/// Place a Mint hold for `amount` against the org's account.
pub async fn place_hold(st: &AppState, p: &Principal, amount: i64, idem: &str) -> Result<String, ApiError> {
    let (Some(mint), Some(account)) = (&st.mint, &p.org.mint_account_id) else {
        return Err(ApiError::payment_required(0, amount, &topup_url(st)));
    };
    match mint.create_hold(account, amount, idem).await {
        Ok(h) => Ok(h.hold_id),
        Err(MintError::InsufficientFunds) => {
            let bal = mint.balance(account).await.ok().map(|b| b.available_micros);
            Err(mint_error(st, MintError::InsufficientFunds, amount, bal))
        }
        Err(e) => Err(mint_error(st, e, amount, None)),
    }
}

/// Capture (cost > 0) or release a hold; failures become outbox retries.
pub async fn settle_hold(st: &AppState, hold_id: &str, cost: i64, idem: &str, outbox: &mut Vec<OutboxItem>) -> Option<String> {
    let mint = st.mint.as_ref()?;
    let to = st.settings.mint_revenue_account.clone().unwrap_or_default();
    if cost > 0 {
        match mint.capture(hold_id, cost, &to, idem).await {
            Ok(r) => Some(r.receipt_id),
            Err(e) => {
                tracing::warn!(error = %e, hold_id, "capture failed; queued for retry");
                outbox.push(OutboxItem::new(new_id("obx"), "mint_capture", json!({ "hold_id": hold_id, "amount_micros": cost, "to": to, "idem_key": idem }), Utc::now()));
                None
            }
        }
    } else {
        match mint.release(hold_id, idem).await {
            Ok(r) => Some(r.receipt_id),
            Err(e) => {
                tracing::warn!(error = %e, hold_id, "release failed; queued for retry");
                outbox.push(OutboxItem::new(new_id("obx"), "mint_release", json!({ "hold_id": hold_id, "idem_key": idem }), Utc::now()));
                None
            }
        }
    }
}

pub fn rago_event(p: &Principal, call_id: &str, tool: &str, provider: &str, cost: i64) -> Option<OutboxItem> {
    let sub = p.org.rago_subscription_id.as_ref()?;
    Some(OutboxItem::new(
        new_id("obx"),
        "rago_event",
        json!({
            "transaction_id": call_id,
            "external_subscription_id": sub,
            "code": "tool_call",
            "timestamp": Utc::now().timestamp(),
            "properties": { "tool": tool, "provider": provider, "cost_micros": cost, "project": p.project.id },
        }),
        Utc::now(),
    ))
}

/// Replace any injected secret (stored credential or the caller's own key)
/// that appears in `text`. Short values are skipped to avoid mangling text.
fn redact_secrets(text: &str, ctx: &CallCtx) -> String {
    use secrecy::ExposeSecret;
    let mut out = text.to_string();
    let secrets = ctx.credential.iter().map(|c| c.expose()).chain(ctx.caller_key.iter().map(|k| k.expose_secret()));
    for secret in secrets {
        if secret.len() >= 8 && out.contains(secret) {
            out = out.replace(secret, "[redacted]");
        }
    }
    out
}

/// The per-tool caps that apply to this plan. A cap may be keyed by a tool id
/// or by a capability id; either way it covers calls made through the
/// capability and through any concrete tool that serves it.
fn tool_cap_scope(st: &AppState, p: &Principal, plan: &Plan) -> Vec<(String, Vec<String>)> {
    let caps = &p.project.tool_daily_caps;
    if caps.is_empty() {
        return Vec::new();
    }
    let mut names: Vec<String> = vec![plan.target.clone()];
    for c in &plan.candidates {
        names.push(c.tool.id.clone());
        names.push(c.tool.capability.clone());
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|n| caps.contains_key(n))
        .map(|n| {
            let mut covers = vec![n.clone()];
            if st.catalog.capability(&n).is_some() {
                covers.extend(st.catalog.tools_for_capability(&n).into_iter().map(|t| t.id.clone()));
            }
            (n, covers)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    st: &AppState,
    p: &Principal,
    plan: &Plan,
    input: &Value,
    hold_amount: i64,
    idem: Option<String>,
    req_hash: &str,
    mode: HoldMode,
    cache_key: Option<&str>,
    cache_label: &'static str,
) -> Result<Executed, ApiError> {
    let call_id = new_id("call");
    let started = Instant::now();
    let side = plan.side_effect();

    // Caps and hold (a batch already did both for all of its calls).
    // A batch reserved the money caps for all of its calls; per-tool caps and
    // the send limit are still checked call by call.
    let scope = CapScope { money: matches!(mode, HoldMode::Own), tools: tool_cap_scope(st, p, plan), sends: i64::from(side == SideEffect::Send) };
    let _guard = st.caps.check_scope(st.store.as_ref(), &p.project, &p.key, &scope, hold_amount, Utc::now()).await?;
    let hold_id = match &mode {
        HoldMode::Own if hold_amount > 0 => Some(place_hold(st, p, hold_amount, &call_id).await?),
        HoldMode::Own => None,
        HoldMode::Batch(b) => b.hold_id.clone(),
    };

    // Execute the chain.
    let mut chain = Vec::new();
    let mut success = None;
    let mut last_err: Option<(usize, AdapterError)> = None;
    for (i, c) in plan.candidates.iter().enumerate() {
        if !st.flow.acquire(&c.tool.provider, Duration::from_secs(2)).await {
            chain.push(json!({ "tool": c.tool.id, "provider": c.tool.provider, "outcome": "rate_limited" }));
            last_err = Some((i, AdapterError::retryable(format!("{} is rate limited; try again shortly", c.tool.provider)).with_status(429)));
            if plan.fallback {
                continue;
            }
            break;
        }
        let ctx = CallCtx {
            call_id: call_id.clone(),
            project_id: p.project.id.clone(),
            org_id: p.org.id.clone(),
            http: st.http.clone(),
            credential: c.credential.clone(),
            caller_key: p.caller_key.clone(),
            config: st.adapter_config.clone(),
            timeout: Duration::from_secs(c.tool.timeout_s.clamp(1, 600)),
            idempotency_key: idem.clone(),
        };
        let t0 = Instant::now();
        // An upstream may echo the injected credential back in an error body;
        // scrub it before the text reaches the caller, the audit log or logs.
        let res = c.adapter.call(&ctx, &c.tool.id, input.clone()).await.map_err(|mut e| {
            e.message = redact_secrets(&e.message, &ctx);
            e
        });
        let ms = t0.elapsed().as_millis() as i64;
        let ok = res.is_ok();
        st.flow.record(&c.tool.provider, &c.tool.id, ok || matches!(&res, Err(e) if e.class == ErrorClass::InputRejected), ms as u32);
        let _ = st.store.record_stat(&c.tool.id, ok, ms, 0).await;
        if let Some(cred) = c.credential.as_ref().filter(|c| !c.platform) {
            match &res {
                Ok(_) => {
                    let _ = st.store.mark_credential(&cred.id, true, None).await;
                }
                Err(e) if matches!(e.status, Some(401) | Some(403)) => {
                    let _ = st.store.mark_credential(&cred.id, false, Some(e.message.chars().take(300).collect())).await;
                }
                _ => {}
            }
        }
        match res {
            Ok(out) => {
                chain.push(json!({ "tool": c.tool.id, "provider": c.tool.provider, "outcome": "ok", "latency_ms": ms }));
                success = Some((i, out));
                break;
            }
            Err(e) => {
                chain.push(json!({ "tool": c.tool.id, "provider": c.tool.provider, "outcome": e.class, "status": e.status, "error": e.message.chars().take(300).collect::<String>(), "latency_ms": ms }));
                let retry = e.class == ErrorClass::Retryable && plan.fallback;
                last_err = Some((i, e));
                if !retry {
                    break;
                }
            }
        }
    }

    // Settle.
    let (status_str, http_status, cost, used, data, units) = match (&success, &last_err) {
        (Some((i, out)), _) => {
            let c = &plan.candidates[*i];
            let cost = if c.metered { c.tool.price.settle(out.status, out.units, out.reported_cost_micros, c.tool.bill_on_4xx, hold_amount) } else { 0 };
            ("ok", out.status as i32, cost, Some(*i), out.data.clone(), out.units)
        }
        (None, Some((i, e))) if e.class == ErrorClass::ProviderBilled4xx => {
            let c = &plan.candidates[*i];
            let status = e.status.unwrap_or(400);
            let cost = if c.metered { c.tool.price.settle(status, None, None, true, hold_amount) } else { 0 };
            ("rejected", status as i32, cost, Some(*i), Value::Null, None)
        }
        (None, Some((i, e))) => ("failed", e.status.unwrap_or(0) as i32, 0, Some(*i), Value::Null, None),
        (None, None) => ("failed", 0, 0, None, Value::Null, None),
    };
    let cost = cost.min(hold_amount.max(0));
    let mut outbox = Vec::new();
    let receipt_id = match (&mode, &hold_id) {
        (HoldMode::Own, Some(h)) => settle_hold(st, h, cost, &call_id, &mut outbox).await,
        (HoldMode::Batch(b), _) => {
            *b.spent.lock().expect("batch poisoned") += cost;
            None
        }
        _ => None,
    };
    let used_tool = used.map(|i| &plan.candidates[i].tool);
    let tool_id = used_tool.map(|t| t.id.clone()).unwrap_or_else(|| plan.target.clone());
    let provider = used_tool.map(|t| t.provider.clone()).unwrap_or_default();
    if cost > 0 {
        if let Some(ev) = rago_event(p, &call_id, &tool_id, &provider, cost) {
            outbox.push(ev);
        }
    }
    let ex = Executed {
        call_id: call_id.clone(),
        tool_id: tool_id.clone(),
        provider: provider.clone(),
        data,
        cost_micros: cost,
        cache: cache_label,
        fallback_chain: Value::Array(chain.clone()),
        hold_id: hold_id.clone(),
        receipt_id: receipt_id.clone().filter(|r| !r.is_empty()),
        units,
    };
    let idem_row = match (&idem, status_str) {
        (Some(k), "ok") => Some(IdempotencyRow {
            project_id: p.project.id.clone(),
            key: k.clone(),
            request_hash: req_hash.into(),
            http_status: 200,
            response: ex.body(),
            expires_at: Utc::now() + chrono::Duration::hours(st.settings.idempotency_hours),
        }),
        _ => None,
    };
    st.store
        .record_call(
            NewCall {
                id: call_id.clone(),
                project_id: p.project.id.clone(),
                org_id: p.org.id.clone(),
                key_public_id: p.key.public_id.clone(),
                target: plan.target.clone(),
                tool_id: Some(tool_id.clone()),
                provider_id: Some(provider.clone()).filter(|s| !s.is_empty()),
                side_effect: side.as_str().into(),
                idempotency_key: idem.clone(),
                request_hash: req_hash.into(),
                status: status_str.into(),
                http_status,
                latency_ms: started.elapsed().as_millis() as i64,
                cost_micros: cost,
                price_basis: used_tool.map(|t| serde_json::to_value(t.price.basis).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()).unwrap_or_default(),
                mint_hold_id: hold_id.clone(),
                mint_receipt: ex.receipt_id.clone(),
                rago_event_id: (cost > 0 && p.org.rago_subscription_id.is_some()).then(|| call_id.clone()),
                cache: ex.cache.into(),
                fallback_chain: Value::Array(chain.clone()),
                created_at: Utc::now(),
            },
            outbox,
            idem_row,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    match status_str {
        "ok" => {
            if let (Some(k), Some(t)) = (cache_key, used_tool) {
                st.cache.put(k, CachedResponse { tool_id: t.id.clone(), provider: t.provider.clone(), data: ex.data.clone() }, Duration::from_secs(t.cache_ttl_s));
            }
            Ok(ex)
        }
        _ => {
            let (_, e) = last_err.expect("failure has an error");
            let details = json!({ "call_id": call_id, "fallback_chain": chain, "billed_micros": cost, "upstream_status": e.status });
            Err(match e.class {
                ErrorClass::Retryable => ApiError::chain_failed(Value::Array(chain.clone())).with_details(details),
                ErrorClass::InputRejected => ApiError::new(422, "provider_rejected_input", e.message).with_details(details),
                ErrorClass::ProviderBilled4xx => ApiError::new(422, "provider_rejected_input_billed", e.message).with_details(details),
                ErrorClass::Fatal => ApiError::new(502, "provider_unavailable", e.message).with_details(details),
            })
        }
    }
}
