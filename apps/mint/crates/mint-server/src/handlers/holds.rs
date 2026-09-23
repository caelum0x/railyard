//! Hold (escrow) resource handlers.

use axum::extract::{Path, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use mint_core::HoldId;
use mint_ledger::{HoldCaptureRequest, HoldCreateRequest, HoldReceipt, HoldReleaseRequest};
use mint_protocol::{encode_receipt, AITP_VERSION};
use mint_receipts::Receipt;
use mint_store::LedgerStore;

use crate::attestation::{build_hold_settlement_attestation, HoldSettlementOperation};
use crate::dto::{parse_account, HoldCaptureReq, HoldCreateReq, HoldReleaseReq, HoldView};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::jobs::{now_ms, MAX_HOLD_TTL_MS};
use crate::state::AppState;
use crate::views;

/// `POST /v1/holds` → `Ledger::create_hold`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<HoldCreateReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    if let Some(ttl) = body.expires_in_ms {
        if ttl <= 0 || ttl > MAX_HOLD_TTL_MS {
            return Err(ApiError::BadRequest(format!(
                "expires_in_ms must be between 1 and {MAX_HOLD_TTL_MS}"
            )));
        }
    }
    let receipt = st
        .ledger
        .create_hold(HoldCreateRequest {
            from,
            amount_micros: body.amount_micros,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    if let Some(ttl) = body.expires_in_ms {
        // An idempotent replay keeps the deadline set by the first request.
        let store = st.ledger.store();
        if store.hold_expiry(&receipt.hold_id).await?.is_none() {
            store
                .set_hold_expiry(&receipt.hold_id, now_ms().saturating_add(ttl))
                .await?;
        }
    }
    signed_hold_response(&st, StatusCode::CREATED, &receipt).await
}

/// `POST /v1/holds/:id/capture` → `Ledger::capture_hold`.
pub async fn capture<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<HoldCaptureReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let amount_micros = body.amount_micros;
    let idem_key = body.idem_key;
    let reference = body.ref_;
    let receipt = st
        .ledger
        .capture_hold(HoldCaptureRequest {
            hold_id: hold_id.clone(),
            amount_micros,
            to: to.clone(),
            idem_key: idem_key.clone(),
            ref_: reference.clone(),
        })
        .await?;
    signed_hold_settlement_response(
        &st,
        StatusCode::OK,
        &receipt,
        &hold_id,
        HoldSettlementOperation::Capture {
            payee: &to,
            amount_micros,
        },
        &idem_key,
        reference.as_deref(),
    )
    .await
}

/// `POST /v1/holds/:id/release` → `Ledger::release_hold`.
pub async fn release<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<HoldReleaseReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let idem_key = body.idem_key;
    let reference = body.ref_;
    let receipt = st
        .ledger
        .release_hold(HoldReleaseRequest {
            hold_id: hold_id.clone(),
            idem_key: idem_key.clone(),
            ref_: reference.clone(),
        })
        .await?;
    signed_hold_settlement_response(
        &st,
        StatusCode::OK,
        &receipt,
        &hold_id,
        HoldSettlementOperation::Release,
        &idem_key,
        reference.as_deref(),
    )
    .await
}

/// `GET /v1/holds/:id` → point read of the live hold plus its auto-release
/// deadline (`expires_at_ms`, `null` when none).
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let store = st.ledger.store();
    let record = store.get_hold(&hold_id).await?;
    let expires_at_ms = store.hold_expiry(&hold_id).await?;
    let mut view = serde_json::to_value(HoldView::from(record))
        .map_err(|e| ApiError::Internal(format!("hold encode: {e}")))?;
    view["expires_at_ms"] = serde_json::json!(expires_at_ms);
    Ok(ok(view))
}

/// Return the ordinary hold view plus the exact signed, hash-linked receipt
/// emitted by this operation. `first_seq` is the operation's durable journal
/// sequence; pairing it with the holder account makes lookup concurrency-safe
/// even when unrelated settlements append receipts at the same time.
pub(crate) async fn signed_hold_response<S: LedgerStore + 'static>(
    st: &AppState<S>,
    status: StatusCode,
    hold: &HoldReceipt,
) -> ApiResult {
    let receipt = operation_receipt(st, hold).await?;
    render_signed_hold_response(status, hold, &receipt, None)
}

/// Return a terminal hold response with both the legacy receipt header and a
/// separately signed, privacy-safe command attestation in the JSON data.
pub(crate) async fn signed_hold_settlement_response<S: LedgerStore + 'static>(
    st: &AppState<S>,
    status: StatusCode,
    hold: &HoldReceipt,
    hold_id: &HoldId,
    operation: HoldSettlementOperation<'_>,
    idem_key: &str,
    reference: Option<&str>,
) -> ApiResult {
    if &hold.hold_id != hold_id {
        return Err(ApiError::Internal(
            "committed hold settlement changed hold identity".to_string(),
        ));
    }
    let receipt = operation_receipt(st, hold).await?;
    let attestation = build_hold_settlement_attestation(
        st.receipt_signer.as_ref(),
        hold_id,
        &hold.holder_balance.account_id,
        operation,
        idem_key,
        reference,
        &receipt,
    )
    .map_err(|_| {
        ApiError::Internal("committed hold settlement failed attestation validation".to_string())
    })?;
    render_signed_hold_response(status, hold, &receipt, Some(attestation))
}

async fn operation_receipt<S: LedgerStore + 'static>(
    st: &AppState<S>,
    hold: &HoldReceipt,
) -> Result<Receipt, ApiError> {
    let account = &hold.holder_balance.account_id;
    let journal_seq = hold.holder_balance.updated_seq;
    st.ledger
        .receipts()
        .await
        .into_iter()
        .find(|receipt| {
            receipt.body.journal_seq == journal_seq && &receipt.body.account_id == account
        })
        .ok_or_else(|| {
            ApiError::Internal(format!(
                "missing signed receipt for hold operation at journal seq {}",
                journal_seq
            ))
        })
}

fn render_signed_hold_response(
    status: StatusCode,
    hold: &HoldReceipt,
    receipt: &Receipt,
    attestation: Option<crate::attestation::HoldSettlementAttestation>,
) -> ApiResult {
    let encoded = encode_receipt(receipt)
        .map_err(|error| ApiError::Internal(format!("receipt encode: {error}")))?;

    let mut data = views::hold_receipt(hold);
    if let Some(attestation) = attestation {
        data["attestation"] = serde_json::to_value(attestation)
            .map_err(|error| ApiError::Internal(format!("attestation encode: {error}")))?;
    }
    let mut response = ok_status(status, data);
    insert_header(&mut response, "aitp-receipt", &encoded);
    insert_header(&mut response, "aitp-version", AITP_VERSION);
    Ok(response)
}

fn insert_header(response: &mut Response, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(name), value);
    }
}
