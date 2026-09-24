//! Operator view of x402 payments (operator token required).
//!
//! - `GET /v1/admin/x402/summary` — settled volume, Mint-credited volume,
//!   pending/failed credits, abandoned and unknown-outcome payments, plus the
//!   active x402 configuration (network, payTo, facilitator, Mint account).
//! - `GET /v1/admin/x402/payments?limit=` — newest payments first.
//! - `POST /v1/admin/x402/credit` — retry pending Mint credits now.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use turnstile_core::x402::credit_pending_once;
use turnstile_core::X402Gate;

use crate::operator::{error, OperatorState};

/// Give up crediting one payment into Mint after this many attempts.
pub const MAX_CREDIT_ATTEMPTS: i32 = 30;

#[derive(Clone)]
pub struct X402AdminState {
    pub operator: OperatorState,
    pub gate: Option<Arc<X402Gate>>,
}

fn disabled() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "x402_disabled",
        "x402 is not enabled on this Turnstile (set TURNSTILE_X402_PAY_TO)",
    )
}

pub async fn summary(State(state): State<X402AdminState>, headers: HeaderMap) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let Some(gate) = &state.gate else {
        return disabled();
    };
    match gate.journal().summary().await {
        Ok(summary) => {
            let cfg = gate.config();
            Json(json!({
                "summary": summary,
                "config": {
                    "network": cfg.network,
                    "pay_to": cfg.pay_to,
                    "asset": cfg.asset,
                    "facilitator_url": cfg.facilitator_url,
                    "mint_account": cfg.mint_account,
                    "public_url": cfg.public_url,
                }
            }))
            .into_response()
        }
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            &e.to_string(),
        ),
    }
}

pub async fn payments(
    State(state): State<X402AdminState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let Some(gate) = &state.gate else {
        return disabled();
    };
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    match gate.journal().recent(limit).await {
        Ok(rows) => Json(json!({ "payments": rows })).into_response(),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            &e.to_string(),
        ),
    }
}

pub async fn credit_now(State(state): State<X402AdminState>, headers: HeaderMap) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let Some(gate) = &state.gate else {
        return disabled();
    };
    let Some(mint) = gate.mint() else {
        return error(
            StatusCode::CONFLICT,
            "mint_credit_disabled",
            "set TURNSTILE_X402_MINT_ACCOUNT to credit x402 revenue into Mint",
        );
    };
    match credit_pending_once(gate.journal().as_ref(), mint, 100, MAX_CREDIT_ATTEMPTS).await {
        Ok(attempted) => Json(json!({ "attempted": attempted })).into_response(),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            &e.to_string(),
        ),
    }
}
