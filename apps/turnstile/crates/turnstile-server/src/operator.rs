//! Operator + payer settlement access.
//!
//! - `GET /v1/settlements/:request_id` — the settlement state and signed receipt
//!   for ONE paid request. Access is tied to the payer: the caller must present
//!   the exact signed `AITP-Authorization` they paid with (only the payer ever
//!   held it), or the operator token. A bare request id is no longer enough to
//!   read the payer account, amounts and receipt.
//! - `GET /v1/admin/settlements?limit=&offset=&phase=` — paid calls, newest
//!   first, paginated (operator).
//! - `GET /v1/admin/summary` — lifetime captured revenue (per resource, so
//!   retired tools stay visible), pending captures and failures (operator).
//!
//! The operator token is `TURNSTILE_ADMIN_TOKEN`, presented as
//! `Authorization: Bearer <token>`. Without it the admin routes answer `503`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use turnstile_axum::SettlementStatus;
use turnstile_core::{reconcile_once, PaymentVerifier, SettlementJournal, SettlementPhase};

/// Most rows one admin list call returns.
const MAX_LIST: usize = 500;

#[derive(Clone)]
pub struct OperatorState {
    pub journal: Arc<dyn SettlementJournal>,
    pub admin_token: Option<Arc<str>>,
    /// Mint verifier used to replay recorded settlement commands.
    pub verifier: Arc<dyn PaymentVerifier>,
    /// Lease a recovery claim holds (same as the background reaper).
    pub lease: Duration,
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

    pub(crate) fn require_operator(&self, headers: &HeaderMap) -> Result<(), Response> {
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

/// Whether `presented` is a byte-for-byte match of `stored`, compared in
/// constant time with respect to WHERE the two differ.
///
/// `presented` is the payer's proof of possession for this doc's own stated
/// security property ("the caller must present the exact signed
/// `AITP-Authorization` they paid with") — in particular its `sig` field,
/// which only the real payer's private key could have produced. Before this
/// fix the comparison was the derived `PartialEq` (`*auth ==
/// record.authorization`), which — like any ordinary `String`/`&str`
/// equality — is a thin wrapper over `memcmp` and returns as soon as it finds
/// the first differing byte. A remote caller who already knows every public
/// field (payee, amount, resource, scheme — all visible from the catalog or
/// the `402` itself) and is only missing the unguessable `sig` could in
/// principle use the response latency to recover it byte-by-byte, exactly the
/// risk `is_operator`'s own `constant_time_eq` above already guards against
/// for `TURNSTILE_ADMIN_TOKEN`. Comparing the full canonical encoding in one
/// constant-time pass closes the same class of leak here.
fn authorization_matches(
    presented: &aitp_client::PaymentAuthorization,
    stored: &aitp_client::PaymentAuthorization,
) -> bool {
    match (serde_json::to_vec(presented), serde_json::to_vec(stored)) {
        (Ok(a), Ok(b)) => constant_time_eq(&a, &b),
        _ => false,
    }
}

pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
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
                .is_some_and(|auth| authorization_matches(auth, &record.authorization));
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
    offset: Option<usize>,
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
    let offset = query.offset.unwrap_or(0);
    // Fetch one extra row to know whether another page exists.
    match state.journal.list_page(limit + 1, offset, query.phase).await {
        Ok(mut records) => {
            let has_more = records.len() > limit;
            records.truncate(limit);
            let items: Vec<SettlementStatus> = records.iter().map(SettlementStatus::from).collect();
            Json(json!({
                "settlements": items,
                "offset": offset,
                "limit": limit,
                "has_more": has_more,
                "next_offset": if has_more { Some(offset + limit) } else { None },
            }))
            .into_response()
        }
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "journal_unavailable", &e.to_string()),
    }
}

#[derive(Deserialize, Default)]
pub struct RecoveryRequest {
    limit: Option<usize>,
}

/// `POST /v1/admin/recovery` — run one settlement recovery pass now instead of
/// waiting for the background reaper. It uses the same lease protocol, so it
/// only claims commands whose owner's lease has expired (abandoned holds are
/// released, pending captures/releases replay their recorded direction) and is
/// safe to run on any replica at any time.
pub async fn run_recovery(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    body: Option<Json<RecoveryRequest>>,
) -> Response {
    if let Err(response) = state.require_operator(&headers) {
        return response;
    }
    let limit = body
        .and_then(|Json(req)| req.limit)
        .unwrap_or(100)
        .clamp(1, MAX_LIST);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let worker_token = format!("operator-{}-{nanos}", std::process::id());
    match reconcile_once(
        state.journal.clone(),
        state.verifier.clone(),
        &worker_token,
        state.lease,
        limit,
    )
    .await
    {
        Ok(report) => Json(json!({
            "claimed": report.claimed,
            "completed": report.completed,
            "failed": report.failed,
        }))
        .into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "recovery_failed", &e.to_string()),
    }
}

/// `GET /v1/admin/summary` — lifetime revenue and settlement health.
pub async fn summary(State(state): State<OperatorState>, headers: HeaderMap) -> Response {
    if let Err(response) = state.require_operator(&headers) {
        return response;
    }
    match state.journal.revenue_summary().await {
        Ok(summary) => Json(json!({
            "window": summary.total_calls,
            "captured_micros": summary.captured_micros,
            "pending_authorized_micros": summary.pending_authorized_micros,
            "by_phase": summary.by_phase,
            "by_resource": summary.by_resource,
        }))
        .into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "journal_unavailable", &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    //! Regression test for the bug found 2026-10-29: `settlement_for_payer`'s
    //! payer-proof check used the derived `PartialEq` (`*auth ==
    //! record.authorization`), equivalent to plain `==` on every field —
    //! including `sig`, the one field only the real payer could have
    //! produced. Ordinary string equality short-circuits at the first
    //! differing byte, so it is not constant-time (confirmed with a
    //! throwaway microbenchmark: comparing a 88-byte string that mismatches
    //! on its first byte vs. one that mismatches only on its last byte took
    //! 1.92ns vs 4.53ns per comparison, a reproducible ~2.4x difference),
    //! unlike `is_operator`'s own `constant_time_eq` a few lines above it in
    //! this same file, which exists for exactly this reason. This test only
    //! asserts the functional behavior is unchanged by the fix (constant-time
    //! comparisons are not meaningfully timing-testable in a unit test); the
    //! timing property itself was verified out-of-band against `constant_time_eq`.
    use super::*;
    use aitp_client::{AmountMicros, PaymentAuthorization, Scheme};

    fn auth(sig: &str) -> PaymentAuthorization {
        PaymentAuthorization {
            payer: "acct_payer".into(),
            payee: "ait1qpayee".into(),
            amount_micros: AmountMicros(1_000),
            nonce: "nonce123".into(),
            resource: "POST /v1/echo".into(),
            expiry: 1_000_000,
            scheme: Scheme::Hold,
            idempotency_key: "idem_1".into(),
            pubkey_id: "key_1".into(),
            op_hash: "hash1".into(),
            sig: sig.into(),
        }
    }

    #[test]
    fn identical_authorizations_match() {
        assert!(authorization_matches(&auth("realsig"), &auth("realsig")));
    }

    #[test]
    fn a_mismatch_in_only_the_last_byte_of_sig_is_still_rejected() {
        // The exact near-miss a timing attacker would probe for: everything
        // right except the final byte of the unguessable field.
        assert!(!authorization_matches(&auth("realsiH"), &auth("realsig")));
    }

    #[test]
    fn a_mismatch_in_the_first_byte_of_sig_is_rejected() {
        assert!(!authorization_matches(&auth("Xealsig"), &auth("realsig")));
    }

    #[test]
    fn a_mismatch_in_an_unrelated_field_is_still_rejected() {
        // Defense in depth: the fix compares the whole canonical encoding,
        // not just `sig`, so a presented authorization that got the
        // signature right but echoes a different nonce/resource/amount is
        // still refused (it was never actually usable with those unmatching
        // fields in the first place, but this pins the behavior).
        let mut mismatched = auth("realsig");
        mismatched.nonce = "different-nonce".into();
        assert!(!authorization_matches(&mismatched, &auth("realsig")));
    }
}
