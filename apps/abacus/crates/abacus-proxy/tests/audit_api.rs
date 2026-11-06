//! Route tests for the receipt audit API — `GET /v1/receipts/:request_id`.
//!
//! The audit endpoint reads Abacus's receipt system-of-record back by the stable
//! `x-request-id` a caller supplied on the proxied request. It sits behind the
//! SAME per-agent derived-key binding as `/v1/proxy`/`/v1/budget` (see
//! `auth::require_ingress_or_admin_key` and `audit::get_receipt`'s ownership
//! check, fixed 2026-11-06 — this route used to accept the bare ingress key
//! directly, with no agent binding at all). These tests drive the REAL
//! `build_router` with `tower::ServiceExt::oneshot` over the in-memory fakes,
//! asserting the documented edges:
//! - `200` `{success:true,data:<receipt>}` for the owning agent's persisted
//!   receipt, or for the operator (admin key) reading ANY agent's receipt;
//! - `404` `receipt_not_found` when none has settled for that id, OR when a
//!   different agent than the owner asks (not distinguishable from "never
//!   existed" — no cross-agent enumeration);
//! - `401` when the bare master ingress key is presented directly (no longer
//!   accepted on this route, same as every other route);
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
use abacus_proxy::auth::{derive_agent_key, API_KEY_HEADER};
use abacus_proxy::handler::AGENT_HEADER;
use abacus_proxy::mem_hold_store::InMemoryHoldStore;
use abacus_proxy::{build_router, AppState, PricingTable};
use abacus_testkit::{FakeBudgetRepo, FakeReceiptStore, LedgerFakeMint, ScriptedUpstream};

const INGRESS_KEY: &str = "test-ingress-key";
const ADMIN_KEY: &str = "test-admin-key-at-least-24-chars";
const REQUEST_ID: &str = "request_audit_1";

/// A captured receipt fixture keyed on `REQUEST_ID`, owned by `owner`.
fn receipt(owner: AgentId) -> MintReceipt {
    MintReceipt {
        agent_id: owner,
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
        admin_api_key: Some(ADMIN_KEY.into()),
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };
    build_router(state)
}

/// A request authenticated as `agent`'s own correctly-derived key.
fn get_as_agent(uri: &str, agent: &AgentId) -> Request<Body> {
    let key = derive_agent_key(INGRESS_KEY.as_bytes(), agent.as_str());
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(API_KEY_HEADER, key)
        .header(AGENT_HEADER, agent.as_str())
        .body(Body::empty())
        .unwrap()
}

/// A request authenticated as the operator (admin key, no agent header).
fn get_as_admin(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(API_KEY_HEADER, ADMIN_KEY)
        .body(Body::empty())
        .unwrap()
}

/// The bare master ingress key, no agent header — no longer accepted anywhere.
fn get_with_bare_master_key(uri: &str) -> Request<Body> {
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
async fn owning_agent_can_read_its_own_receipt() {
    let owner = AgentId::generate();
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt(owner.clone())).await.unwrap();

    let resp = app(receipts)
        .oneshot(get_as_agent(&format!("/v1/receipts/{REQUEST_ID}"), &owner))
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
async fn admin_key_can_read_any_agents_receipt() {
    let owner = AgentId::generate();
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt(owner)).await.unwrap();

    let resp = app(receipts)
        .oneshot(get_as_admin(&format!("/v1/receipts/{REQUEST_ID}")))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["data"]["request_id"], REQUEST_ID);
}

/// Regression test for the real gap found+fixed 2026-11-06: a DIFFERENT agent
/// than the receipt's owner — authenticated with its OWN genuinely-derived key,
/// not a forged one — must not be able to read it, and the response must be
/// indistinguishable from "never existed" (no cross-agent enumeration).
#[tokio::test]
async fn a_different_agent_cannot_read_another_agents_receipt() {
    let owner = AgentId::generate();
    let other = AgentId::generate();
    assert_ne!(owner, other);
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt(owner)).await.unwrap();

    let resp = app(receipts)
        .oneshot(get_as_agent(&format!("/v1/receipts/{REQUEST_ID}"), &other))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "receipt_not_found");
}

/// Regression test for the real gap found+fixed 2026-11-06: the bare
/// `ABACUS_API_KEY` used to authenticate this route directly, with no agent
/// binding at all. It must no longer work, on its own, for anything.
#[tokio::test]
async fn bare_master_key_alone_no_longer_authenticates_this_route() {
    let owner = AgentId::generate();
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt(owner)).await.unwrap();

    let resp = app(receipts)
        .oneshot(get_with_bare_master_key(&format!("/v1/receipts/{REQUEST_ID}")))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_receipt_returns_404_receipt_not_found() {
    let receipts = Arc::new(FakeReceiptStore::new());
    let caller = AgentId::generate();

    let resp = app(receipts)
        .oneshot(get_as_agent("/v1/receipts/request_never_settled", &caller))
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
    let caller = AgentId::generate();

    // A space is outside the accepted [A-Za-z0-9_-] request-id charset.
    let resp = app(receipts)
        .oneshot(get_as_agent("/v1/receipts/bad%20id", &caller))
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
    let caller = AgentId::generate();

    let resp = app(receipts)
        .oneshot(get_as_agent(&format!("/v1/receipts/{REQUEST_ID}"), &caller))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = json_body(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "storage_error");
}

#[tokio::test]
async fn audit_route_requires_ingress_key() {
    let owner = AgentId::generate();
    let receipts = Arc::new(FakeReceiptStore::new());
    receipts.record(&receipt(owner)).await.unwrap();

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
