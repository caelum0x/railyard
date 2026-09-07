//! Integration tests for the `settlement_status` API handler + `SettlementStatus`
//! projection, driven over a real axum Router with `tower::ServiceExt::oneshot`.
//!
//! The handler exposes the durable [`SettlementJournal::get`] read as a
//! privacy-safe status view. We assert its three documented edges:
//! - `200` with the correct `SettlementStatus` projection for a known settlement;
//! - `404` `unknown request_id` when the journal has no such command;
//! - `503` `settlement journal unavailable` (fail-closed) when the journal read
//!   itself errors — so a client retries rather than assuming a terminal state.
//!
//! The known-settlement case is seeded through the journal's own public command
//! surface (`begin_verification` → `bind_verified`) so the projection is asserted
//! against a genuinely-recorded record, not a hand-forged one. The failure case
//! uses a tiny `FailingJournal` whose reads always error at the storage seam.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`
use turnstile_axum::settlement_status;
use turnstile_core::{
    AmountMicros, HoldRef, InMemorySettlementJournal, PaymentAuthorization, Receipt, Scheme,
    SettlementJournal, SettlementRecord, TurnstileError,
};

const NONCE: &str = "nonce-known-1";
const OWNER: &str = "owner-token-1";
const PAYER: &str = "acct_payer";
const PAYEE: &str = "ait1qpayee";
const RESOURCE: &str = "POST /v1/echo";
const PRICE: i64 = 1_000;

/// A hold-scheme authorization for the known-settlement fixture.
fn authorization() -> PaymentAuthorization {
    PaymentAuthorization {
        payer: PAYER.into(),
        payee: PAYEE.into(),
        amount_micros: AmountMicros(PRICE),
        nonce: NONCE.into(),
        resource: RESOURCE.into(),
        expiry: 9_999_999_999_000,
        scheme: Scheme::Hold,
        idempotency_key: format!("aitp:{NONCE}"),
        pubkey_id: "key_1".into(),
        op_hash: String::new(),
        sig: String::new(),
    }
}

/// Mount the handler on its documented route with the journal as state.
fn app(journal: Arc<dyn SettlementJournal>) -> Router {
    Router::new()
        .route("/v1/settlements/:request_id", get(settlement_status))
        .with_state(journal)
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn known_settlement_returns_200_with_projection() {
    let journal: Arc<dyn SettlementJournal> = Arc::new(InMemorySettlementJournal::new());

    // Seed a genuine record through the journal's own command surface, then bind a
    // verified hold so the projection carries scheme + hold identity.
    let lease = 60_000;
    journal
        .begin_verification(&authorization(), OWNER, lease)
        .await
        .unwrap();
    let hold = HoldRef("hold-abc".into());
    journal
        .bind_verified(NONCE, OWNER, Some(&hold), Scheme::Hold, lease)
        .await
        .unwrap();

    let resp = app(journal)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/settlements/{NONCE}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;

    // The projected view mirrors the record faithfully.
    assert_eq!(body["request_id"], NONCE);
    assert_eq!(body["phase"], "held");
    assert_eq!(body["settled"], false);
    assert_eq!(body["scheme"], "aitp-hold");
    assert_eq!(body["payer"], PAYER);
    assert_eq!(body["payee"], PAYEE);
    assert_eq!(body["resource"], RESOURCE);
    assert_eq!(body["authorized_micros"], PRICE);
    assert_eq!(body["hold"], "hold-abc");

    // Privacy-safe omissions: internal recovery ownership is never projected, and
    // absent optionals are skipped (no receipt/actual/error yet).
    assert!(body.get("owner_token").is_none());
    assert!(body.get("lease_until_ms").is_none());
    assert!(body.get("receipt").is_none());
    assert!(body.get("actual_micros").is_none());
    assert!(body.get("error").is_none());
}

#[tokio::test]
async fn unknown_request_id_returns_404() {
    let journal: Arc<dyn SettlementJournal> = Arc::new(InMemorySettlementJournal::new());

    let resp = app(journal)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/settlements/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["error"], "unknown request_id");
    assert_eq!(body["request_id"], "does-not-exist");
}

#[tokio::test]
async fn journal_read_failure_returns_503_fail_closed() {
    let journal: Arc<dyn SettlementJournal> = Arc::new(FailingJournal);

    let resp = app(journal)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/settlements/{NONCE}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Fail-closed: an infrastructure read error is a 503, never a spurious 404
    // that a client might read as a terminal "unknown/settled" answer.
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json_body(resp).await;
    assert_eq!(body["error"], "settlement journal unavailable");
    assert_eq!(body["request_id"], NONCE);
}

/// A journal whose reads always fail at the storage seam. Only `get` is exercised
/// by the status handler; the rest satisfy the trait and return the same error.
struct FailingJournal;

fn boom() -> TurnstileError {
    TurnstileError::Settlement("journal backend unavailable".into())
}

#[async_trait]
impl SettlementJournal for FailingJournal {
    async fn begin_verification(
        &self,
        _authorization: &PaymentAuthorization,
        _owner_token: &str,
        _lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        Err(boom())
    }

    async fn bind_verified(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _hold: Option<&HoldRef>,
        _scheme: Scheme,
        _lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        Err(boom())
    }

    async fn renew(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _lease_until_ms: i64,
    ) -> Result<(), TurnstileError> {
        Err(boom())
    }

    async fn prepare_capture(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _actual: AmountMicros,
        _lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        Err(boom())
    }

    async fn prepare_release(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        Err(boom())
    }

    async fn complete_capture(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _receipt: &Receipt,
    ) -> Result<(), TurnstileError> {
        Err(boom())
    }

    async fn complete_release(
        &self,
        _nonce: &str,
        _owner_token: &str,
    ) -> Result<(), TurnstileError> {
        Err(boom())
    }

    async fn reject_verification(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _error: &str,
    ) -> Result<(), TurnstileError> {
        Err(boom())
    }

    async fn record_error(
        &self,
        _nonce: &str,
        _owner_token: &str,
        _error: &str,
    ) -> Result<(), TurnstileError> {
        Err(boom())
    }

    async fn claim_recoverable(
        &self,
        _worker_token: &str,
        _now_ms: i64,
        _lease_until_ms: i64,
        _limit: usize,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        Err(boom())
    }

    async fn get(&self, _nonce: &str) -> Result<Option<SettlementRecord>, TurnstileError> {
        Err(boom())
    }
}
