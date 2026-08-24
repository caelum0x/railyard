//! Receipt export + Merkle audit handlers (§6).

use axum::extract::{Path, State};
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

/// `GET /v1/accounts/:id/receipts` → the account's hash-linked receipt chain.
pub async fn account_receipts<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let all = st.ledger.receipts().await;
    let chain: Vec<_> = all
        .into_iter()
        .filter(|r| r.body.account_id == account)
        .collect();
    Ok(ok(chain))
}

/// `GET /v1/receipts/audit` → the Merkle root over the whole receipt log (§6).
pub async fn audit<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let root = st.ledger.merkle_root().await;
    let epoch = st.ledger.store().journal_len().await;
    Ok(ok(json!({
        "merkle_root": root.to_hex(),
        "epoch": epoch,
    })))
}
