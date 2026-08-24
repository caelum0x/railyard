//! Integration tests for the metered-proxy flow, driven entirely over the
//! `vend-testkit` fakes (no network, no database).
//!
//! Each test builds the real axum router with an [`AppState`] wired to a
//! [`FakeMint`] + a scriptable relay + a seeded [`InMemoryStore`], then drives a
//! `POST /v1/chat/completions` through `tower::ServiceExt::oneshot` and asserts on
//! the settlement side effects (Mint balance movement, settlement headers, HTTP
//! status).
//!
//! Covered:
//! - happy path: hold → proxy → capture the EXACT actual (cost + markup),
//! - upstream failure (5xx and transport error): release, end-user charged zero,
//! - insufficient funds on the hold: short-circuit, Relay never called,
//! - spend cap over-limit: 402 before any hold,
//! - bad key (missing / unknown / revoked): 401 before any Mint/Relay call,
//! - per-tenant markup isolation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use vend_core::error::VendError;
use vend_core::ports::{MeteredUsage, UsageOutcome, UsageRepo};
use vend_core::transport::{RelayRequest, RelayResponse, RelayTransport, Usage};
use vend_keys::KeyRepo;
use vend_server::pricing::{ModelRate, PricingCatalog};
use vend_server::{build_router, AppState};
use vend_settlement::PayoutConfig;
use vend_testkit::fixtures::{two_tenants, TenantFixture, TwoTenants};
use vend_testkit::{FakeMint, FakeRelay, InMemoryStore};

const RELAY_BASE_PATH: &str = "/openai/v1";
const RELAY_KEY: &str = "relay-test-key";

/// A relay that counts calls and returns a scripted response, so tests can assert
/// it was NOT called on short-circuit paths.
struct CountingRelay {
    calls: AtomicUsize,
    response: RelayResponse,
    fail_transport: bool,
}

impl CountingRelay {
    fn ok(response: RelayResponse) -> Self {
        CountingRelay {
            calls: AtomicUsize::new(0),
            response,
            fail_transport: false,
        }
    }
    fn transport_error() -> Self {
        CountingRelay {
            calls: AtomicUsize::new(0),
            response: FakeRelay::canned_error(500),
            fail_transport: true,
        }
    }
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RelayTransport for CountingRelay {
    async fn chat_completion(&self, _req: RelayRequest) -> Result<RelayResponse, VendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_transport {
            return Err(VendError::Transport("boom".into()));
        }
        Ok(self.response.clone())
    }
}

/// Wire an [`AppState`] from the shared store + injected mint/relay.
fn state_with(
    store: Arc<InMemoryStore>,
    mint: Arc<dyn vend_core::transport::MintTransport>,
    relay: Arc<dyn RelayTransport>,
) -> AppState {
    AppState {
        metrics: Arc::new(vend_server::observability::Metrics::default()),
        mint,
        relay,
        tenants: store.clone(),
        keys: store.clone(),
        usage: store.clone(),
        payouts: store.clone(),
        readiness: store.clone(),
        idempotency: store,
        pricing: Arc::new(
            PricingCatalog::new([(
                "gpt-4o-mini".to_string(),
                ModelRate {
                    input_micros_per_million_tokens: 10_000_000,
                    output_micros_per_million_tokens: 10_000_000,
                    max_input_tokens: 100_000,
                    max_output_tokens: 1_000_000,
                },
            )])
            .unwrap(),
        ),
        relay_base_path: RELAY_BASE_PATH.to_string(),
        relay_key: RELAY_KEY.to_string(),
        settlement_account: "acct_vend_settlement".to_string(),
        payout_config: PayoutConfig::new(
            "acct_vend_settlement",
            "acct_provider_payout",
            "acct_platform_payout",
        )
        .unwrap(),
        security_ops: None,
    }
}

/// A chat request body for a given model, with a `max_tokens` estimate ceiling.
fn chat_body(model: &str, max_tokens: i64) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": "hi"}],
    })
}

/// Build a `POST /v1/chat/completions` request, optionally bearer-authenticated.
fn chat_request(bearer: Option<&str>, body: &serde_json::Value) -> Request<Body> {
    chat_request_with_key(bearer, body, Some("test-request-1"))
}

fn chat_request_with_key(
    bearer: Option<&str>,
    body: &serde_json::Value,
    idempotency_key: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        b = b.header("idempotency-key", key);
    }
    if let Some(secret) = bearer {
        b = b.header("authorization", format!("Bearer {secret}"));
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn header_i64(resp: &axum::response::Response, name: &str) -> Option<i64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// The app account that backs a tenant's holds (TenantReserve funding).
fn funding_account(f: &TenantFixture) -> String {
    f.app.mint_account_id.clone()
}

// --------------------------------------------------------------------------
// Happy path: hold → proxy → capture the EXACT actual (cost + markup).
// --------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_holds_then_captures_exact_actual() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    // Fund the app account generously.
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    // Relay reports EXACT usage of 3 in + 2 out = 5 tokens.
    let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_success(Usage {
        input_tokens: 3,
        output_tokens: 2,
        provider_cost_micros: Some(50),
    })));

    let start = mint.balance(&account);
    let app = build_router(state_with(store.clone(), mint.clone(), relay));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // The hold conservatively covers the rate card's full input bound plus the
    // requested output ceiling. Terminal capture uses Relay's authoritative
    // tensorzero_cost instead of that estimate.
    // Actual: 5 tok * 10 = 50 provider → markup max(ceil(50*30%)=15, 1000)=1000 →
    // charge 1_050; all unused hold value is released.
    assert_eq!(header_i64(&resp, "Vend-Price-Micros"), Some(1_050));
    assert_eq!(header_i64(&resp, "Vend-Base-Cost-Micros"), Some(50));
    let charge_id = resp.headers()["Vend-Charge-Id"].to_str().unwrap();
    let usage = store.usage_by_charge_id(charge_id).unwrap();
    assert_eq!(usage.provider_cost_micros, 50);
    assert_eq!(usage.charge_micros, 1_050);
    assert_eq!(usage.platform_fee_micros, 100);
    assert_eq!(usage.tenant_net_micros, 900);

    // Balance dropped by exactly the captured price (not the estimate ceiling).
    assert_eq!(mint.balance(&account), start - 1_050);
    assert_eq!(mint.balance("acct_provider_payout"), 50);
    assert_eq!(mint.balance(&a.tenant.mint_account_id), 900);
    assert_eq!(mint.balance("acct_platform_payout"), 100);
    assert_eq!(mint.balance("acct_vend_settlement"), 0);
    let payout = store.payout_by_charge_id(charge_id).unwrap();
    assert_eq!(payout.state, vend_core::PayoutState::Complete);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value, serde_json::json!({"choices": []}));
    assert!(
        value.get("success").is_none(),
        "OpenAI responses are not wrapped"
    );
}

// --------------------------------------------------------------------------
// Upstream failure releases the hold; end-user charged exactly zero.
// --------------------------------------------------------------------------

#[tokio::test]
async fn upstream_5xx_releases_and_charges_zero() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_error(500)));

    let start = mint.balance(&account);
    let app = build_router(state_with(store, mint.clone(), relay));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    // Price is zero and the hold is fully released — balance is untouched.
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_i64(&resp, "Vend-Price-Micros"), Some(0));
    assert_eq!(mint.balance(&account), start);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"error": "upstream"})
    );
}

#[tokio::test]
async fn upstream_transport_error_releases_and_charges_zero() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::transport_error());

    let start = mint.balance(&account);
    let app = build_router(state_with(store, mint.clone(), relay.clone()));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    // Transport error surfaces as an upstream 5xx, and the hold is released.
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(relay.count(), 1);
    assert_eq!(mint.balance(&account), start);
}

#[tokio::test]
async fn relay_without_streaming_support_releases_the_hold() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));
    let start = mint.balance(&account);
    let mut body = chat_body("gpt-4o-mini", 100);
    body["stream"] = serde_json::Value::Bool(true);

    let resp = build_router(state_with(store, mint.clone(), relay.clone()))
        .oneshot(chat_request(Some(&a.key_secret), &body))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // This fake implements only the non-streaming method; the additive default
    // reports streaming_unsupported. The real Relay client overrides it with a
    // live byte stream.
    assert_eq!(relay.count(), 0);
    assert_eq!(mint.balance(&account), start);
}

#[tokio::test]
async fn exact_idempotent_replay_does_not_call_relay_or_charge_twice() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 3,
        output_tokens: 2,
        provider_cost_micros: Some(50),
    })));
    let app = build_router(state_with(store, mint.clone(), relay.clone()));
    let body = chat_body("gpt-4o-mini", 100);
    let start = mint.balance(&account);

    let first = app
        .clone()
        .oneshot(chat_request_with_key(
            Some(&a.key_secret),
            &body,
            Some("stable-request"),
        ))
        .await
        .unwrap();
    let charge_id = first.headers()["Vend-Charge-Id"].clone();
    let second = app
        .oneshot(chat_request_with_key(
            Some(&a.key_secret),
            &body,
            Some("stable-request"),
        ))
        .await
        .unwrap();

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["Vend-Charge-Id"], charge_id);
    assert_eq!(relay.count(), 1);
    assert_eq!(mint.balance(&account), start - 1_050);
}

#[tokio::test]
async fn reused_idempotency_key_with_different_body_conflicts() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));
    let app = build_router(state_with(store, mint, relay.clone()));

    let first = app
        .clone()
        .oneshot(chat_request_with_key(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
            Some("conflicting-request"),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let conflict = app
        .oneshot(chat_request_with_key(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 101),
            Some("conflicting-request"),
        ))
        .await
        .unwrap();

    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(relay.count(), 1);
}

#[tokio::test]
async fn missing_idempotency_key_is_rejected_before_hold() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));
    let start = mint.balance(&account);
    let resp = build_router(state_with(store, mint.clone(), relay.clone()))
        .oneshot(chat_request_with_key(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(relay.count(), 0);
    assert_eq!(mint.balance(&account), start);
}

#[tokio::test]
async fn success_without_exact_provider_cost_is_released_and_returns_502() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: None,
    })));
    let start = mint.balance(&account);
    let resp = build_router(state_with(store, mint.clone(), relay))
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(header_i64(&resp, "Vend-Price-Micros"), Some(0));
    assert_eq!(mint.balance(&account), start);
}

// --------------------------------------------------------------------------
// Insufficient funds on the hold short-circuits — Relay is never called.
// --------------------------------------------------------------------------

#[tokio::test]
async fn insufficient_funds_short_circuits_before_relay() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    // Force the hold to fail; relay must never be reached.
    let mint = Arc::new(
        FakeMint::new()
            .with_balance(&account, 100_000_000)
            .forcing_insufficient(),
    );
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));

    let app = build_router(state_with(store, mint, relay.clone()));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        relay.count(),
        0,
        "Relay must not be called after a failed hold"
    );
}

// --------------------------------------------------------------------------
// Spend cap over-limit is rejected BEFORE any hold.
// --------------------------------------------------------------------------

#[tokio::test]
async fn spend_cap_over_limit_rejected_before_hold() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    // Seed prior captured usage that fills the key's daily cap (50_000_000).
    let utc_day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prior = MeteredUsage {
        charge_id: vend_core::ids::ChargeId::generate(),
        sub_key_id: a.key.id.clone(),
        tenant_id: a.tenant.id.clone(),
        app_id: a.app.id.clone(),
        end_user_id: None,
        idem_key: "seed".into(),
        hold_id: "seed_hold".into(),
        provider_cost_micros: 49_999_000,
        charge_micros: 50_000_000,
        platform_fee_micros: 0,
        tenant_net_micros: 0,
        outcome: UsageOutcome::Captured,
        utc_day,
    };
    store.record_pending(&prior).await.unwrap();

    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));
    let start = mint.balance(&account);

    let app = build_router(state_with(store, mint.clone(), relay.clone()));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        relay.count(),
        0,
        "Relay must not be called when the cap is exceeded"
    );
    assert_eq!(
        mint.balance(&account),
        start,
        "no hold may be taken over the cap"
    );
}

#[tokio::test]
async fn spend_cap_within_limit_proceeds() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_success(Usage {
        input_tokens: 1,
        output_tokens: 1,
        provider_cost_micros: Some(20),
    })));

    let app = build_router(state_with(store, mint.clone(), relay));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(header_i64(&resp, "Vend-Price-Micros").unwrap() > 0);
}

// --------------------------------------------------------------------------
// Bad tenant key → 401, before any Mint or Relay call.
// --------------------------------------------------------------------------

#[tokio::test]
async fn missing_bearer_is_401_before_upstreams() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(
        Usage::default(),
    )));
    let start = mint.balance(&account);

    let app = build_router(state_with(store, mint.clone(), relay.clone()));
    let resp = app
        .oneshot(chat_request(None, &chat_body("gpt-4o-mini", 100)))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(relay.count(), 0);
    assert_eq!(mint.balance(&account), start);
}

#[tokio::test]
async fn unknown_key_is_401() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(
        Usage::default(),
    )));

    let app = build_router(state_with(store, mint, relay.clone()));
    let resp = app
        .oneshot(chat_request(
            Some("vk_test_deadbeefdeadbeefdeadbeef"),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(relay.count(), 0);
}

#[tokio::test]
async fn revoked_key_is_401() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);

    // Revoke the seeded key; new requests must be rejected.
    store.revoke(&a.key.id).await.unwrap();

    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(
        Usage::default(),
    )));

    let app = build_router(state_with(store, mint, relay.clone()));
    let resp = app
        .oneshot(chat_request(
            Some(&a.key_secret),
            &chat_body("gpt-4o-mini", 100),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(relay.count(), 0);
}

// --------------------------------------------------------------------------
// A model outside the app's allowlist is rejected (400), before any hold.
// --------------------------------------------------------------------------

#[tokio::test]
async fn model_not_in_allowlist_is_rejected() {
    let TwoTenants { store, a, .. } = two_tenants();
    let store = Arc::new(store);
    let account = funding_account(&a);
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(CountingRelay::ok(FakeRelay::canned_success(
        Usage::default(),
    )));
    let start = mint.balance(&account);

    let app = build_router(state_with(store, mint.clone(), relay.clone()));
    let resp = app
        .oneshot(chat_request(Some(&a.key_secret), &chat_body("gpt-4o", 100)))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(relay.count(), 0);
    assert_eq!(mint.balance(&account), start);
}

// --------------------------------------------------------------------------
// Per-tenant markup isolation: distinct policies → distinct captured price.
// --------------------------------------------------------------------------

#[tokio::test]
async fn markup_is_applied_per_tenant() {
    // Tenant A: 30% markup, min fee 1_000. Tenant B: 50% markup, min fee 0.
    // With a large provider cost the min fee never binds, so the markups differ.
    async fn price_for(f: &TenantFixture, store: Arc<InMemoryStore>) -> i64 {
        let account = f.app.mint_account_id.clone();
        let mint = Arc::new(FakeMint::new().with_balance(&account, 1_000_000_000));
        // 100_000 in + 100_000 out = 200_000 tok * 10 = 2_000_000 provider µ$.
        let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_success(Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            provider_cost_micros: Some(2_000_000),
        })));
        let app = build_router(state_with(store, mint, relay));
        let resp = app
            .oneshot(chat_request(
                Some(&f.key_secret),
                &chat_body("gpt-4o-mini", 1_000_000),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        header_i64(&resp, "Vend-Price-Micros").unwrap()
    }

    let TwoTenants { store, a, b } = two_tenants();
    let store = Arc::new(store);
    let price_a = price_for(&a, store.clone()).await;
    let price_b = price_for(&b, store).await;

    // provider 2_000_000: A charge = 2_000_000 + 30% = 2_600_000;
    //                     B charge = 2_000_000 + 50% = 3_000_000.
    assert_eq!(price_a, 2_600_000);
    assert_eq!(price_b, 3_000_000);
    assert_ne!(price_a, price_b);
}
