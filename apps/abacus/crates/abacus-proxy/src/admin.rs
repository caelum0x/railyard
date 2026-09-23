//! Operator + agent budget views.
//!
//! - `GET /v1/admin/overview`  — every agent with its policy, open reservations,
//!   recent spend and (for allowance-funded agents) live remaining headroom.
//! - `GET /v1/admin/receipts`  — recent settled receipts (newest first).
//! - `GET /v1/admin/holds`     — Mint holds currently open (in flight / orphaned).
//!
//! These sit behind the SEPARATE operator key (`ABACUS_ADMIN_API_KEY`); the
//! agents' ingress key cannot read fleet-wide spend. `GET /v1/budget` is the
//! agent-facing view of its own remaining budget, behind the ingress key.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use abacus_core::envelope::{ErrorBody, ErrorEnvelope, SuccessEnvelope};
use abacus_core::error::AbacusError;
use abacus_core::identity::{AgentProfile, FundingPath};
use abacus_core::ids::{AgentId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptOutcome};

use crate::handler::AGENT_HEADER;
use crate::AppState;

const DEFAULT_RECEIPTS: usize = 100;
const MAX_RECEIPTS: usize = 1_000;
/// Receipts folded into the per-agent "recent spend" figures on the overview.
const OVERVIEW_RECEIPT_WINDOW: usize = 1_000;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorEnvelope {
            success: false,
            error: ErrorBody {
                code: code.to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}

fn store_error(e: AbacusError) -> Response {
    tracing::error!(error = %e, "admin view store read failed");
    error(StatusCode::BAD_GATEWAY, "storage_error", "store unavailable")
}

fn ok(v: Value) -> Response {
    (StatusCode::OK, Json(SuccessEnvelope::new(v))).into_response()
}

#[derive(Default)]
struct Spend {
    requests: u64,
    captured_micros: i64,
    released: u64,
}

fn fold_spend(receipts: &[MintReceipt]) -> HashMap<String, Spend> {
    let mut by_agent: HashMap<String, Spend> = HashMap::new();
    for r in receipts {
        let s = by_agent.entry(r.agent_id.as_str().to_string()).or_default();
        s.requests += 1;
        match r.outcome {
            ReceiptOutcome::Captured => {
                s.captured_micros = s.captured_micros.saturating_add(r.captured_micros)
            }
            ReceiptOutcome::Released => s.released += 1,
        }
    }
    by_agent
}

/// Live remaining allowance headroom for an allowance-funded agent.
async fn allowance_remaining(state: &AppState, agent: &AgentProfile) -> Value {
    match (&agent.budget.funding_path, &agent.budget.allowance_id) {
        (FundingPath::AllowanceCeiling, Some(id)) => match state.mint.get_allowance(id).await {
            Ok(a) => json!({
                "allowance_id": id,
                "limit_micros": a.limit_micros,
                "spent_micros": a.spent_micros,
                "remaining_micros": a.remaining_micros(),
                "revoked": a.revoked,
            }),
            Err(e) => json!({ "allowance_id": id, "error": e.to_string() }),
        },
        _ => Value::Null,
    }
}

/// `GET /v1/admin/overview`
pub async fn overview(State(state): State<AppState>) -> Response {
    let agents = match state.budgets.list_agents().await {
        Ok(a) => a,
        Err(e) => return store_error(e),
    };
    let holds = match state.holds.list().await {
        Ok(h) => h,
        Err(e) => return store_error(e),
    };
    let receipts = match state
        .receipts
        .list_recent(None, OVERVIEW_RECEIPT_WINDOW)
        .await
    {
        Ok(r) => r,
        Err(e) => return store_error(e),
    };
    let spend = fold_spend(&receipts);
    let mut open_by_agent: HashMap<String, (u64, i64)> = HashMap::new();
    let mut untracked_open = (0u64, 0i64);
    for h in &holds {
        let slot = match &h.receipt_context {
            Some(ctx) => open_by_agent
                .entry(ctx.agent_id.as_str().to_string())
                .or_default(),
            None => &mut untracked_open,
        };
        slot.0 += 1;
        slot.1 = slot.1.saturating_add(h.reserved_micros);
    }
    let mut rows = Vec::with_capacity(agents.len());
    for agent in &agents {
        let id = agent.agent_id.as_str();
        let s = spend.get(id);
        let open = open_by_agent.get(id).copied().unwrap_or_default();
        rows.push(json!({
            "agent_id": id,
            "agent_acct": agent.agent_acct,
            "treasury": agent.treasury,
            "budget_id": agent.budget.budget_id,
            "funding_path": agent.budget.funding_path,
            "per_request_max_micros": agent.budget.per_request_max_micros,
            "open_holds": open.0,
            "reserved_micros": open.1,
            "recent_requests": s.map(|s| s.requests).unwrap_or(0),
            "recent_captured_micros": s.map(|s| s.captured_micros).unwrap_or(0),
            "recent_released": s.map(|s| s.released).unwrap_or(0),
            "allowance": allowance_remaining(&state, agent).await,
        }));
    }
    ok(json!({
        "agents": rows,
        "open_holds_total": holds.len(),
        "untracked_open_holds": untracked_open.0,
        "receipt_window": receipts.len(),
    }))
}

#[derive(Debug, Default, Deserialize)]
pub struct ReceiptParams {
    agent_id: Option<String>,
    limit: Option<usize>,
}

/// `GET /v1/admin/receipts?agent_id=&limit=`
pub async fn receipts(State(state): State<AppState>, Query(p): Query<ReceiptParams>) -> Response {
    let agent = p.agent_id.as_deref().filter(|a| !a.is_empty());
    if let Some(a) = agent {
        if AgentId::parse(a).is_err() {
            return error(StatusCode::BAD_REQUEST, "bad_id", "agent_id is malformed");
        }
    }
    let limit = p.limit.unwrap_or(DEFAULT_RECEIPTS).clamp(1, MAX_RECEIPTS);
    match state.receipts.list_recent(agent, limit).await {
        Ok(r) => ok(json!({ "items": r })),
        Err(e) => store_error(e),
    }
}

/// `GET /v1/admin/holds`
pub async fn holds(State(state): State<AppState>) -> Response {
    match state.holds.list().await {
        Ok(mut h) => {
            h.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
            ok(json!({ "items": h }))
        }
        Err(e) => store_error(e),
    }
}

/// `GET /v1/budget` (ingress key + `x-abacus-agent`) — an agent's own policy,
/// live allowance headroom, and what it currently has reserved in flight.
pub async fn own_budget(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(raw) = headers.get(AGENT_HEADER).and_then(|v| v.to_str().ok()) else {
        return error(StatusCode::BAD_REQUEST, "bad_id", "x-abacus-agent header is required");
    };
    let Ok(agent_id) = AgentId::parse(raw) else {
        return error(StatusCode::BAD_REQUEST, "bad_id", "x-abacus-agent is malformed");
    };
    let identity = match state.budgets.resolve(&agent_id, &RunId::generate()).await {
        Ok(i) => i,
        Err(AbacusError::AccountNotFound) => {
            return error(StatusCode::NOT_FOUND, "agent_not_found", "unknown agent")
        }
        Err(e) => return store_error(e),
    };
    let profile = AgentProfile {
        agent_id: agent_id.clone(),
        agent_acct: identity.agent_acct,
        treasury: identity.treasury,
        payee: identity.payee,
        budget: identity.budget,
    };
    let (open, reserved) = match state.holds.list().await {
        Ok(h) => h
            .iter()
            .filter(|r| {
                r.receipt_context
                    .as_ref()
                    .is_some_and(|c| c.agent_id.as_str() == agent_id.as_str())
            })
            .fold((0u64, 0i64), |acc, r| {
                (acc.0 + 1, acc.1.saturating_add(r.reserved_micros))
            }),
        Err(e) => return store_error(e),
    };
    ok(json!({
        "agent_id": agent_id.as_str(),
        "budget_id": profile.budget.budget_id,
        "funding_path": profile.budget.funding_path,
        "per_request_max_micros": profile.budget.per_request_max_micros,
        "open_holds": open,
        "reserved_micros": reserved,
        "allowance": allowance_remaining(&state, &profile).await,
    }))
}
