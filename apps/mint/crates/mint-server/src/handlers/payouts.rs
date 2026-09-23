//! Payout tracking (the durable post-burn off-ramp log): list, inspect (with a
//! live provider status refresh for pending payouts) and retry failed payouts
//! against their already-burned supply.

use axum::extract::{Path, Query, State};
use axum::Json;
use mint_core::{op_hash, TransferAmount};
use mint_store::{LedgerStore, MintCommit, PayoutQuery, PayoutRecord, PayoutRecordStatus};
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
            ApiError::BadRequest("status must be one of: pending, paid, failed, cancelled".into())
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

/// `POST /v1/payouts/:payout_ref/cancel` body.
#[derive(Debug, Default, Deserialize)]
pub struct CancelReq {
    /// Why the payout is being abandoned (kept on the record).
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /v1/payouts/:payout_ref/cancel` → abandon a `failed` payout and
/// re-credit its already-burned amount to the account (a compensating mint,
/// idempotent per payout). Operator-only.
pub async fn cancel<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(payout_ref): Path<String>,
    body: Option<Json<CancelReq>>,
) -> ApiResult {
    let note = body.and_then(|Json(b)| b.note).map(|n| n.trim().to_string());
    let store = st.ledger.store();
    let rec = store
        .get_payout(&payout_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no payout {payout_ref}")))?;
    match rec.status {
        PayoutRecordStatus::Failed => {}
        PayoutRecordStatus::Cancelled => return Ok(ok(views::payout_record(&rec))),
        PayoutRecordStatus::Pending | PayoutRecordStatus::Paid => {
            return Err(ApiError::BadRequest(format!(
                "only a failed payout can be cancelled (this one is {})",
                rec.status.as_str()
            )))
        }
    }
    let amount = TransferAmount::new(rec.amount_micros).map_err(ApiError::from)?;
    // The re-credit is a mint tagged with the payout's own provider namespace
    // (`<provider>:<ref>#cancel`) so per-provider reconciliation nets it out.
    let hash = op_hash(&json!({
        "op": "payout_cancel",
        "to": rec.account_id.as_str(),
        "amount": amount.get(),
        "payout_ref": rec.payout_ref,
    }));
    store
        .commit_mint(MintCommit {
            to: rec.account_id.clone(),
            amount,
            deposit_ref: format!("{}#cancel", rec.payout_ref),
            idem_key: format!("payout-cancel:{}", rec.payout_ref),
            op_hash: hash,
        })
        .await?;
    let reason = note
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "cancelled by operator".to_string());
    let updated = store
        .upsert_payout(PayoutRecord {
            status: PayoutRecordStatus::Cancelled,
            last_error: Some(format!("cancelled: {reason}")),
            ..rec
        })
        .await?;
    tracing::info!(payout = %updated.payout_ref, "payout cancelled and re-credited");
    Ok(ok(views::payout_record(&updated)))
}

/// `POST /v1/payouts/:payout_ref/mark-paid` body.
#[derive(Debug, Default, Deserialize)]
pub struct MarkPaidReq {
    /// The provider-side id of the out-of-band settlement (wire ref, tx hash).
    #[serde(default)]
    pub provider_ref: Option<String>,
}

/// `POST /v1/payouts/:payout_ref/mark-paid` → record an out-of-band settlement
/// of a `failed` / `pending` payout (e.g. a manual bank wire). No money moves:
/// the burn already happened. Operator-only.
pub async fn mark_paid<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(payout_ref): Path<String>,
    body: Option<Json<MarkPaidReq>>,
) -> ApiResult {
    let provider_ref = body
        .and_then(|Json(b)| b.provider_ref)
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    let store = st.ledger.store();
    let rec = store
        .get_payout(&payout_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no payout {payout_ref}")))?;
    match rec.status {
        PayoutRecordStatus::Failed | PayoutRecordStatus::Pending => {}
        PayoutRecordStatus::Paid => return Ok(ok(views::payout_record(&rec))),
        PayoutRecordStatus::Cancelled => {
            return Err(ApiError::BadRequest(
                "a cancelled payout was re-credited and cannot be marked paid".into(),
            ))
        }
    }
    let updated = store
        .upsert_payout(PayoutRecord {
            status: PayoutRecordStatus::Paid,
            provider_ref: provider_ref.or(rec.provider_ref.clone()),
            last_error: None,
            ..rec
        })
        .await?;
    tracing::info!(payout = %updated.payout_ref, "payout marked paid by operator");
    Ok(ok(views::payout_record(&updated)))
}
