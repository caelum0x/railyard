//! Payout tracking (the durable post-burn off-ramp log): list, inspect (with a
//! live provider status refresh for pending payouts) and retry failed payouts
//! against their already-burned supply.

use axum::extract::{Path, Query, State};
use mint_store::{LedgerStore, PayoutQuery, PayoutRecordStatus};
use serde::Deserialize;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 500;

/// Query parameters for `GET /v1/payouts`.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// `pending` | `paid` | `failed`.
    pub status: Option<String>,
    /// Only payouts burned from this account.
    pub account_id: Option<String>,
    /// Page size (default 50, max 500).
    pub limit: Option<usize>,
    /// Keyset cursor: the `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// Encode a `(created_ms, payout_ref)` keyset cursor.
fn encode_cursor(created_ms: i64, payout_ref: &str) -> String {
    format!("{created_ms}~{payout_ref}")
}

fn decode_cursor(raw: &str) -> Result<(i64, String), ApiError> {
    let (ms, r) = raw
        .split_once('~')
        .ok_or_else(|| ApiError::BadRequest("malformed cursor".into()))?;
    let ms = ms
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest("malformed cursor".into()))?;
    Ok((ms, r.to_string()))
}

/// `GET /v1/payouts` → newest-first payout records, filterable by status and
/// account, keyset paginated.
pub async fn list<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Query(p): Query<ListParams>,
) -> ApiResult {
    let status = match p.status.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => Some(PayoutRecordStatus::parse(raw).ok_or_else(|| {
            ApiError::BadRequest("status must be one of: pending, paid, failed".into())
        })?),
        None => None,
    };
    let account_id = match p.account_id.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_account(raw).map_err(ApiError::from)?),
        None => None,
    };
    let before = match p.cursor.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => Some(decode_cursor(raw)?),
        None => None,
    };
    let limit = p.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let rows = st
        .ledger
        .store()
        .list_payouts(PayoutQuery {
            status,
            account_id,
            before,
            limit,
        })
        .await?;
    let next_cursor = (rows.len() == limit)
        .then(|| rows.last().map(|r| encode_cursor(r.created_ms, &r.payout_ref)))
        .flatten();
    Ok(ok(json!({
        "items": rows.iter().map(views::payout_record).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    })))
}

/// `GET /v1/payouts/:payout_ref` → one payout; a `pending` payout is refreshed
/// from the provider (PayPal batch / on-chain receipt) before returning.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(payout_ref): Path<String>,
) -> ApiResult {
    let rec = st
        .reserve
        .refresh_payout(&payout_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no payout {payout_ref}")))?;
    Ok(ok(views::payout_record(&rec)))
}

/// `POST /v1/payouts/:payout_ref/retry` → re-send a `failed` payout with the
/// same `payout_ref` (the provider idempotency key). `paid`/`pending` payouts
/// are returned unchanged — never double-sent.
pub async fn retry<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(payout_ref): Path<String>,
) -> ApiResult {
    let rec = st
        .reserve
        .retry_payout(&payout_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no payout {payout_ref}")))?;
    Ok(ok(views::payout_record(&rec)))
}
