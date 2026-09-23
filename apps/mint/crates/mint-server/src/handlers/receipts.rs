//! Receipt export + Merkle audit handlers (§6).

use axum::extract::{Path, Query, State};
use mint_store::{LedgerStore, ReceiptQuery};
use serde::Deserialize;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

/// Default / maximum page size for a receipt-chain page.
const DEFAULT_CHAIN_PAGE: usize = 1_000;
const MAX_CHAIN_PAGE: usize = 5_000;

/// Query parameters for `GET /v1/accounts/:id/receipts`.
#[derive(Debug, Default, Deserialize)]
pub struct ChainParams {
    /// Only receipts with `account_seq > after_seq` (pagination cursor).
    pub after_seq: Option<u64>,
    /// Page size (default 1000, max 5000).
    pub limit: Option<usize>,
    /// Only receipts with `ts >= from_ms`.
    pub from_ms: Option<i64>,
    /// Only receipts with `ts < to_ms`.
    pub to_ms: Option<i64>,
}

/// `GET /v1/accounts/:id/receipts` → a page of the account's hash-linked
/// receipt chain in chain order. Page with `?after_seq=<last account_seq>`.
/// Only that account's rows are read (never the whole receipt table).
pub async fn account_receipts<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Query(p): Query<ChainParams>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    // Distinguish "unknown account" (404) from "no activity yet" (empty).
    st.ledger.read_balance(&account).await?;
    let limit = p.limit.unwrap_or(DEFAULT_CHAIN_PAGE).clamp(1, MAX_CHAIN_PAGE);
    let chain = st
        .ledger
        .store()
        .query_account_receipts(
            &account,
            ReceiptQuery {
                after_seq: p.after_seq,
                from_ms: p.from_ms,
                to_ms: p.to_ms,
                limit,
            },
        )
        .await?;
    Ok(ok(chain))
}

/// `GET /v1/receipts/audit` → the Merkle root over the whole receipt log (§6).
/// A storage outage is a 5xx, never an "epoch 0" empty-ledger root.
pub async fn audit<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let store = st.ledger.store();
    let root = store.try_receipt_merkle_root().await?;
    let epoch = store.try_journal_len().await?;
    Ok(ok(json!({
        "merkle_root": root.to_hex(),
        "epoch": epoch,
    })))
}
