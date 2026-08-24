//! AITP-402 protocol handlers (§5.3): challenge / verify+settle / capture /
//! release. These call the real `Aitp402` state machine and set the AITP wire
//! headers so the agent `aitp-client` interoperates.

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mint_core::{Address, HoldId};
use mint_protocol::{encode_challenge, encode_receipt, Authorization, Settlement, AITP_VERSION};
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{parse_account, AitpCaptureReq, AitpChallengeReq, AitpReleaseReq};
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

const H_CHALLENGE: HeaderName = HeaderName::from_static("aitp-challenge");
const H_RECEIPT: HeaderName = HeaderName::from_static("aitp-receipt");
const H_VERSION: HeaderName = HeaderName::from_static("aitp-version");

/// `POST /v1/aitp/challenge` → `Aitp402::issue_challenge` (issuer helper). Returns
/// `402` with the `AITP-Challenge` + `AITP-Version` headers set.
pub async fn challenge<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<AitpChallengeReq>,
) -> ApiResult {
    let payee = Address::parse(&body.payee).map_err(ApiError::from)?;
    let challenge = st.aitp.issue_challenge(
        &payee,
        body.amount_micros,
        &body.resource,
        body.scheme,
        body.ttl_ms,
    );
    let encoded = encode_challenge(&challenge)
        .map_err(|e| ApiError::Internal(format!("challenge encode: {e}")))?;

    let mut resp = (
        StatusCode::PAYMENT_REQUIRED,
        Json(json!({
            "payee": challenge.payee,
            "amount_micros": challenge.amount_micros.get(),
            "currency": challenge.currency,
            "resource": challenge.resource,
            "nonce": challenge.nonce,
            "expiry": challenge.expiry,
            "scheme": challenge.scheme,
        })),
    )
        .into_response();
    insert_header(&mut resp, H_CHALLENGE, &encoded);
    insert_header(&mut resp, H_VERSION, AITP_VERSION);
    Ok(resp)
}

/// `POST /v1/aitp/verify` → `Aitp402::verify_and_settle`. Sets the `AITP-Receipt`
/// + `AITP-Version` headers on success (§5.3).
pub async fn verify<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(auth): Json<Authorization>,
) -> ApiResult {
    let settlement = st.aitp.verify_and_settle(&auth).await?;
    let (body, aitp_receipt) = match &settlement {
        Settlement::Transfer {
            receipt,
            aitp_receipt,
        } => (
            json!({
                "scheme": "transfer",
                "receipt": views::transfer_receipt(receipt),
            }),
            aitp_receipt,
        ),
        Settlement::Hold {
            hold_id,
            receipt,
            aitp_receipt,
        } => (
            json!({
                "scheme": "hold",
                "hold_id": hold_id.as_str(),
                "receipt": views::hold_receipt(receipt),
            }),
            aitp_receipt,
        ),
    };
    let encoded = encode_receipt(aitp_receipt)
        .map_err(|e| ApiError::Internal(format!("receipt encode: {e}")))?;

    let mut resp = Json(json!({ "success": true, "data": body })).into_response();
    insert_header(&mut resp, H_RECEIPT, &encoded);
    insert_header(&mut resp, H_VERSION, AITP_VERSION);
    Ok(resp)
}

/// `POST /v1/aitp/capture` → `Aitp402::capture` (success path).
pub async fn capture<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<AitpCaptureReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&body.hold_id).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .aitp
        .capture(hold_id, body.amount_micros, to, body.idem_key)
        .await?;
    Ok(ok(views::hold_receipt(&receipt)))
}

/// `POST /v1/aitp/release` → `Aitp402::release` (failure path, ADR-0002).
pub async fn release<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<AitpReleaseReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&body.hold_id).map_err(ApiError::from)?;
    let receipt = st.aitp.release(hold_id, body.idem_key).await?;
    Ok(ok(views::hold_receipt(&receipt)))
}

fn insert_header(resp: &mut Response, name: HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        resp.headers_mut().insert(name, v);
    }
}
