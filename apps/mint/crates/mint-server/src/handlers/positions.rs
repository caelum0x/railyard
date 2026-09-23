//! Per-account open positions for operators and account owners: the holds,
//! streams and allowances that keep an account's `held` balance up (and block
//! a close with `account_not_empty`). Each listing is newest first with a
//! status filter; the console pairs them with release / close / revoke
//! actions on the existing per-resource routes.

use axum::extract::{Path, Query, State};
use mint_store::{HoldStatus, LedgerStore, StreamStatus};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::{parse_account, AllowanceView, HoldView, StreamView};
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 1_000;

/// Query for the per-account listings.
#[derive(Debug, Default, Deserialize)]
pub struct PositionsParams {
    /// Holds: `open` | `captured` | `released`; streams: `open` | `closed`.
    /// Allowances: `active` (default) | `all`.
    pub status: Option<String>,
    /// Page size (default 100, max 1000).
    pub limit: Option<usize>,
}

fn page(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
}

fn status_param(p: &PositionsParams) -> Option<String> {
    p.status
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

/// `GET /v1/accounts/:id/holds` → the account's holds with their
/// auto-release deadline (`expires_at_ms`, `null` when none).
pub async fn holds<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Query(p): Query<PositionsParams>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let status = match status_param(&p).as_deref() {
        None => None,
        Some("open") => Some(HoldStatus::Open),
        Some("captured") => Some(HoldStatus::Captured),
        Some("released") => Some(HoldStatus::Released),
        Some(_) => {
            return Err(ApiError::BadRequest(
                "status must be one of: open, captured, released".into(),
            ))
        }
    };
    let rows = st
        .ledger
        .store()
        .list_account_holds(&account, status, page(p.limit))
        .await?;
    let open_held: i64 = rows
        .iter()
        .filter(|(h, _)| h.status == HoldStatus::Open)
        .map(|(h, _)| h.amount_micros.get())
        .sum();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(h, expires)| {
            let mut v = serde_json::to_value(HoldView::from(h)).unwrap_or(Value::Null);
            v["expires_at_ms"] = json!(expires);
            v
        })
        .collect();
    Ok(ok(json!({
        "account_id": account.as_str(),
        "items": items,
        "open_held_micros": open_held,
    })))
}

/// `GET /v1/accounts/:id/streams` → streams the account pays or receives.
pub async fn streams<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Query(p): Query<PositionsParams>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let status = match status_param(&p).as_deref() {
        None => None,
        Some("open") => Some(StreamStatus::Open),
        Some("closed") => Some(StreamStatus::Closed),
        Some(_) => return Err(ApiError::BadRequest("status must be one of: open, closed".into())),
    };
    let rows = st
        .ledger
        .store()
        .list_account_streams(&account, status, page(p.limit))
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|s| {
            let direction = if s.from_id == account { "outgoing" } else { "incoming" };
            let mut v = serde_json::to_value(StreamView::from(s)).unwrap_or(Value::Null);
            v["direction"] = json!(direction);
            v
        })
        .collect();
    Ok(ok(json!({ "account_id": account.as_str(), "items": items })))
}

/// `GET /v1/accounts/:id/allowances` → allowances the account granted
/// (`status=all` includes revoked ones).
pub async fn allowances<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Query(p): Query<PositionsParams>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let include_revoked = match status_param(&p).as_deref() {
        None | Some("active") => false,
        Some("all") => true,
        Some(_) => return Err(ApiError::BadRequest("status must be one of: active, all".into())),
    };
    let rows = st
        .ledger
        .store()
        .list_account_allowances(&account, include_revoked, page(p.limit))
        .await?;
    let items: Vec<AllowanceView> = rows.into_iter().map(AllowanceView::from).collect();
    Ok(ok(json!({ "account_id": account.as_str(), "items": items })))
}
