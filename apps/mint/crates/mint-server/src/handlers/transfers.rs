//! Transfer resource handler.

use axum::extract::State;
use axum::Json;
use mint_core::Address;
use mint_ledger::TransferRequest;
use mint_store::LedgerStore;
use serde_json::json;

use crate::attestation::build_transfer_attestation;
use crate::dto::{parse_account, TransferReq};
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

/// `POST /v1/transfers` → `Ledger::transfer`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<TransferReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    // Public Mint addresses are routing identifiers, never authority. Resolve
    // them through Mint's authoritative address index before constructing the
    // immutable ledger command; receipts and attestations therefore continue
    // to bind the actual recipient account rather than an alias.
    let to = if body.to.starts_with("ait1") {
        let address = Address::parse(&body.to).map_err(ApiError::from)?;
        st.ledger.store().lookup_address(&address).await?
    } else {
        parse_account(&body.to).map_err(ApiError::from)?
    };
    let receipt_owner = from.clone();
    let receipt_recipient = to.clone();
    let reference = body.ref_.clone();
    let receipt = st
        .ledger
        .transfer(TransferRequest {
            from,
            to,
            amount_micros: body.amount_micros,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    // TransferReceipt carries the balance/result projection. Export the exact
    // signed sender-chain receipt as well so downstream products can persist
    // portable evidence without racing a separate "latest receipt" query.
    // `first_seq` is the sender journal leg by construction and remains stable
    // on an idempotent replay.
    let signed_receipt = st
        .ledger
        .receipt(&receipt_owner, receipt.first_seq)
        .await
        .ok_or_else(|| {
            ApiError::Internal(
                "committed transfer is missing its signed sender receipt".to_string(),
            )
        })?;
    let attestation = build_transfer_attestation(
        st.receipt_signer.as_ref(),
        &receipt_owner,
        &receipt_recipient,
        reference.as_deref(),
        &signed_receipt,
    )
    .map_err(|_| {
        ApiError::Internal("committed transfer receipt failed attestation validation".to_string())
    })?;
    Ok(ok(json!({
        "first_seq": receipt.first_seq,
        "from_balance": serde_json::to_value(&receipt.from_balance).unwrap_or(json!(null)),
        "to_balance": serde_json::to_value(&receipt.to_balance).unwrap_or(json!(null)),
        "idempotent_replay": receipt.idempotent_replay,
        "receipt": signed_receipt,
        "attestation": attestation,
    })))
}
