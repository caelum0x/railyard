//! Route tests for the receipt audit API — `GET /v1/receipts/:request_id`.
//!
//! The audit endpoint reads Abacus's receipt system-of-record back by the stable
//! `x-request-id` a caller supplied on the proxied request. It sits behind the
//! same ingress `X-API-Key` as the enforced route (agent spend is not public).
//! These tests drive the REAL `build_router` with `tower::ServiceExt::oneshot`
//! over the in-memory fakes, asserting the four documented edges:
//! - `200` `{success:true,data:<receipt>}` for a persisted receipt;
//! - `404` `receipt_not_found` when none has settled for that id;
//! - `400` `bad_id` when the request id is malformed;
//! - `502` `storage_error` when the receipt store read fails.
//!
//! This file covers the HTTP handler layer only. Store-layer receipt persistence
//! over live Postgres is covered separately in `postgres_money_path.rs`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // for `oneshot`

use abacus_core::error::AbacusError;
use abacus_core::ids::{AgentId, BudgetId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptOutcome, ReceiptStore};
use abacus_core::transport::SettlementProof;
use abacus_proxy::auth::API_KEY_HEADER;
use abacus_proxy::mem_hold_store::InMemoryHoldStore;
use abacus_proxy::{build_router, AppState, PricingTable};
use abacus_testkit::{FakeBudgetRepo, FakeReceiptStore, LedgerFakeMint, ScriptedUpstream};

const INGRESS_KEY: &str = "test-ingress-key";
const REQUEST_ID: &str = "request_audit_1";

/// A captured receipt fixture keyed on `REQUEST_ID`.
fn receipt() -> MintReceipt {
    MintReceipt {
        agent_id: AgentId::generate(),
        run_id: RunId::generate(),
        budget_id: BudgetId::generate(),
        payer_account: "acct_treasury01".into(),
        request_id: REQUEST_ID.into(),
        hold_id: "hold_audit_1".into(),
        reserved_micros: 5_000,
        captured_micros: 750,
        balance_after_micros: 999_250,
        settlement_seq: 42,
        outcome: ReceiptOutcome::Captured,
        settlement_proof: SettlementProof {
            sequence: 42,
            operation: "hold.capture".into(),
            operation_hash: "op_hash".into(),
            receipt_hash: "receipt_hash".into(),
            signature: "signature".into(),
        },
    }
}

/// Build the real router with the given receipt store; the other dependencies are
/// unused by the audit route but required to construct the shared `AppState`.
fn app(receipts: Arc<dyn ReceiptStore>) -> axum::Router {
    let state = AppState {
        mint: Arc::new(LedgerFakeMint::new()),
        upstream: Arc::new(ScriptedUpstream::new()),
        budgets: Arc::new(FakeBudgetRepo::new()),
        receipts,
        holds: Arc::new(InMemoryHoldStore::new()),
        ingress_api_key: INGRESS_KEY.into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };
    build_router(state)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(API_KEY_HEADER, INGRESS_KEY)
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn persisted_receipt_returns_200_with_body() {
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt()).await.unwrap();

    let resp = app(receipts)
        .oneshot(get(&format!("/v1/receipts/{REQUEST_ID}")))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["request_id"], REQUEST_ID);
    assert_eq!(body["data"]["hold_id"], "hold_audit_1");
    assert_eq!(body["data"]["captured_micros"], 750);
    assert_eq!(body["data"]["outcome"], "captured");
}

#[tokio::test]
async fn missing_receipt_returns_404_receipt_not_found() {
    let receipts = Arc::new(FakeReceiptStore::new());

    let resp = app(receipts)
        .oneshot(get("/v1/receipts/request_never_settled"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "receipt_not_found");
}

#[tokio::test]
async fn malformed_request_id_returns_400_bad_id() {
    let receipts = Arc::new(FakeReceiptStore::new());

    // A space is outside the accepted [A-Za-z0-9_-] request-id charset.
    let resp = app(receipts)
        .oneshot(get("/v1/receipts/bad%20id"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "bad_id");
}

#[tokio::test]
async fn store_read_failure_returns_502_storage_error() {
    let receipts: Arc<dyn ReceiptStore> = Arc::new(FailingReceiptStore);

    let resp = app(receipts)
        .oneshot(get(&format!("/v1/receipts/{REQUEST_ID}")))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = json_body(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "storage_error");
}

#[tokio::test]
async fn audit_route_requires_ingress_key() {
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt()).await.unwrap();

    // No X-API-Key header ⇒ the shared ingress guard rejects before the handler.
    let resp = app(receipts)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/receipts/{REQUEST_ID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A receipt store whose reads always fail at the storage seam — exercises the
/// handler's `502 storage_error` fail path.
struct FailingReceiptStore;

#[async_trait]
impl ReceiptStore for FailingReceiptStore {
    async fn record(&self, _receipt: &MintReceipt) -> Result<(), AbacusError> {
        Err(AbacusError::Storage("receipt store unavailable".into()))
    }

    async fn get(&self, _hold_id: &str) -> Result<Option<MintReceipt>, AbacusError> {
        Err(AbacusError::Storage("receipt store unavailable".into()))
    }

    async fn get_by_request_id(
        &self,
        _request_id: &str,
    ) -> Result<Option<MintReceipt>, AbacusError> {
        Err(AbacusError::Storage("receipt store unavailable".into()))
    }
}
