//! Settlement-status API: look up the terminal state of a paid request by the
//! `AITP-Request-Id` (the authorization nonce) that Turnstile hands back.
//!
//! A client that receives a `503` with `AITP-Settlement: capture_pending` and an
//! `AITP-Request-Id` header (see [`response`](crate::response)) has no way, on
//! its own, to learn whether the durable recovery worker ultimately captured,
//! released, or rejected that request. This module closes that loop: it exposes
//! the already-durable [`SettlementJournal::get`] read as a small HTTP handler
//! returning a privacy-safe status view.
//!
//! Wire it as a plain axum route whose state is the shared journal handle:
//!
//! ```ignore
//! use std::sync::Arc;
//! use axum::{routing::get, Router};
//! use turnstile_axum::settlement_status;
//! use turnstile_core::SettlementJournal;
//!
//! let journal: Arc<dyn SettlementJournal> = /* the same handle the gate uses */;
//! let api = Router::new()
//!     .route("/v1/settlements/:request_id", get(settlement_status))
//!     .with_state(journal);
//! ```
//!
//! The response deliberately omits the recovery-ownership fields
//! (`owner_token`, `lease_until_ms`): those are internal to at-most-once
//! recovery and are never a client concern. It DOES return the signed
//! `AITP-Receipt` when one exists, because that receipt is the client's
//! settlement evidence.

use std::sync::Arc;

use aitp_client::{Receipt, Scheme};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use turnstile_core::{SettlementJournal, SettlementPhase, SettlementRecord};

/// Privacy-safe projection of a [`SettlementRecord`] for the status API.
#[derive(Debug, Clone, Serialize)]
pub struct SettlementStatus {
    /// The `AITP-Request-Id` this state belongs to (the authorization nonce).
    pub request_id: String,
    /// Monotonic settlement lifecycle phase.
    pub phase: SettlementPhase,
    /// Whether the phase is terminal (captured / released / transfer / rejected).
    pub settled: bool,
    /// Settlement scheme once bound (`aitp-hold` / `aitp-transfer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<Scheme>,
    /// Paying account echoed from the authorization.
    pub payer: String,
    /// Provider receive address echoed from the authorization.
    pub payee: String,
    /// Resource that was paid for.
    pub resource: String,
    /// Price authorized for this request, in microdollars.
    pub authorized_micros: i64,
    /// Opaque Mint hold reference, once a hold was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// Actual amount captured/released, in microdollars, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_micros: Option<i64>,
    /// The signed settlement receipt — the client's evidence — once captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// Last recorded error, when the request is stuck or was rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<&SettlementRecord> for SettlementStatus {
    fn from(record: &SettlementRecord) -> Self {
        let error = if record.last_error.trim().is_empty() {
            None
        } else {
            Some(record.last_error.clone())
        };
        Self {
            request_id: record.nonce().to_string(),
            phase: record.phase,
            settled: record.phase.is_terminal(),
            scheme: record.scheme,
            payer: record.authorization.payer.clone(),
            payee: record.authorization.payee.clone(),
            resource: record.authorization.resource.clone(),
            authorized_micros: record.authorization.amount_micros.0,
            hold: record.hold.as_ref().map(|h| h.0.clone()),
            actual_micros: record.actual_micros.map(|a| a.0),
            receipt: record.receipt.clone(),
            error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct StatusError {
    error: &'static str,
    request_id: String,
}

/// `GET /…/:request_id` — return the settlement status for one request id.
///
/// - `200` with a [`SettlementStatus`] body when the journal knows the request;
/// - `404` when no settlement command exists for that id;
/// - `503` when the journal read itself fails (fail-closed: the client should
///   retry rather than assume a terminal state).
pub async fn settlement_status(
    State(journal): State<Arc<dyn SettlementJournal>>,
    Path(request_id): Path<String>,
) -> Response {
    match journal.get(&request_id).await {
        Ok(Some(record)) => (StatusCode::OK, Json(SettlementStatus::from(&record))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(StatusError {
                error: "unknown request_id",
                request_id,
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(StatusError {
                error: "settlement journal unavailable",
                request_id,
            }),
        )
            .into_response(),
    }
}
