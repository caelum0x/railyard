//! Operator + payer settlement access.
//!
//! - `GET /v1/settlements/:request_id` — the settlement state and signed receipt
//!   for ONE paid request. Access is tied to the payer: the caller must present
//!   the exact signed `AITP-Authorization` they paid with (only the payer ever
//!   held it), or the operator token. A bare request id is no longer enough to
//!   read the payer account, amounts and receipt.
//! - `GET /v1/admin/settlements?limit=&phase=` — recent paid calls (operator).
//! - `GET /v1/admin/summary` — captured revenue, pending captures and failures
//!   across the recent window (operator).
//!
//! The operator token is `TURNSTILE_ADMIN_TOKEN`, presented as
//! `Authorization: Bearer <token>`. Without it the admin routes answer `503`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use turnstile_axum::SettlementStatus;
use turnstile_core::{SettlementJournal, SettlementPhase};

/// Most rows one admin list call returns.
const MAX_LIST: usize = 500;
/// Window the revenue summary aggregates over.
const SUMMARY_WINDOW: usize = 5_000;

#[derive(Clone)]
pub struct OperatorState {
    pub journal: Arc<dyn SettlementJournal>,
    pub admin_token: Option<Arc<str>>,
}

impl OperatorState {
    fn is_operator(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.admin_token else {
            return false;
        };
        let presented = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .unwrap_or("");
        constant_time_eq(expected.as_bytes(), presented.as_bytes())
    }

    fn require_operator(&self, headers: &HeaderMap) -> Result<(), Response> {
        if self.admin_token.is_none() {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "admin_disabled",
                "TURNSTILE_ADMIN_TOKEN is not configured",
            ));
        }
        if self.is_operator(headers) {
            Ok(())
        } else {
            Err(error(StatusCode::UNAUTHORIZED, "unauthorized", "operator token required"))
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

/// `GET /v1/settlements/:request_id` — payer- or operator-scoped status.
pub async fn settlement_for_payer(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    let operator = state.is_operator(&headers);
    let presented = headers
        .get("aitp-authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|encoded| aitp_client::codec::decode_authorization(encoded).ok());
    if !operator && presented.is_none() {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "present the AITP-Authorization you paid with, or the operator token",
        );
    }
    match state.journal.get(&request_id).await {
        Ok(Some(record)) => {
            let payer_proven = presented
                .as_ref()
                .is_some_and(|auth| *auth == record.authorization);
            if operator || payer_proven {
                (StatusCode::OK, Json(SettlementStatus::from(&record))).into_response()
            } else {
                // Same answer as an unknown id: never confirm existence.
                error(StatusCode::NOT_FOUND, "unknown request_id", "unknown request_id")
            }
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "unknown request_id", "unknown request_id"),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            "settlement journal unavailable",
        ),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<usize>,
    phase: Option<SettlementPhase>,
}

/// `GET /v1/admin/settlements` — recent paid calls, newest first.
pub async fn list_settlements(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    if let Err(response) = state.require_operator(&headers) {
        return response;
    }
    let limit = query.limit.unwrap_or(100).clamp(1, MAX_LIST);
    match state.journal.list_recent(limit, query.phase).await {
        Ok(records) => {
            let items: Vec<SettlementStatus> = records.iter().map(SettlementStatus::from).collect();
            Json(json!({ "settlements": items })).into_response()
        }
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "journal_unavailable", &e.to_string()),
    }
}

/// `GET /v1/admin/summary` — revenue and health across the recent window.
pub async fn summary(State(state): State<OperatorState>, headers: HeaderMap) -> Response {
    if let Err(response) = state.require_operator(&headers) {
        return response;
    }
    let records = match state.journal.list_recent(SUMMARY_WINDOW, None).await {
        Ok(records) => records,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "journal_unavailable", &e.to_string()),
    };
    let mut by_phase: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_resource: BTreeMap<String, (u64, i64)> = BTreeMap::new();
    let mut captured_micros: i64 = 0;
    let mut authorized_pending_micros: i64 = 0;
    for record in &records {
        let phase = serde_json::to_value(record.phase)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        *by_phase.entry(phase).or_default() += 1;
        match record.phase {
            SettlementPhase::Captured | SettlementPhase::TransferComplete => {
                let amount = record
                    .actual_micros
                    .map(|a| a.0)
                    .unwrap_or(record.authorization.amount_micros.0);
                captured_micros = captured_micros.saturating_add(amount);
                let entry = by_resource
                    .entry(record.authorization.resource.clone())
                    .or_default();
                entry.0 += 1;
                entry.1 = entry.1.saturating_add(amount);
            }
            SettlementPhase::Held | SettlementPhase::CapturePending | SettlementPhase::Verifying => {
                authorized_pending_micros =
                    authorized_pending_micros.saturating_add(record.authorization.amount_micros.0);
            }
            _ => {}
        }
    }
    let resources: Vec<_> = by_resource
        .into_iter()
        .map(|(resource, (calls, micros))| json!({ "resource": resource, "paid_calls": calls, "captured_micros": micros }))
        .collect();
    Json(json!({
        "window": records.len(),
        "captured_micros": captured_micros,
        "pending_authorized_micros": authorized_pending_micros,
        "by_phase": by_phase,
        "by_resource": resources,
    }))
    .into_response()
}
