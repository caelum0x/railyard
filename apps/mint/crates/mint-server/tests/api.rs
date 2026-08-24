//! Server integration tests: the REAL router / handlers / auth / error logic,
//! with an `InMemoryStore` test-double injected as the storage backend (no DB).
//! The Postgres store path is live-tested at deploy (see `mint-store-postgres`).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mint_core::{AccountId, AccountKind, Micros};
use mint_protocol::{build_authorization, Challenge, InMemoryPayerKeys, Scheme};
use mint_receipts::{Ed25519Signer, Signer};
use mint_reserve::{FakeKycGate, FakeRamp, KycTier};
use mint_server::{build_router, AppState};
use mint_store::{Clock, FixedClock, InMemoryStore, LedgerStore};
use serde_json::{json, Value};
use tower::ServiceExt;

const API_KEY: &str = "test-secret-key";

/// A funded fixture: alice (10 AIT available) and bob (empty), plus the router.
struct Fixture {
    app: Router,
    alice: AccountId,
    bob: AccountId,
}

async fn fixture() -> Fixture {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(1_000_000));
    let store = InMemoryStore::with_clock(clock.clone());

    let alice = store.create_account(AccountKind::User, None).await.unwrap();
    store
        .seed_account(&alice, Micros::new(10_000_000))
        .await
        .unwrap();
    let bob = store
        .create_account(AccountKind::Model, None)
        .await
        .unwrap();

    let ramp = Arc::new(FakeRamp::new());
    let kyc = Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX));
    let keys = Arc::new(InMemoryPayerKeys::new());
    let state = AppState::new(store, clock, ramp, kyc, keys, API_KEY, "whsec_test");
    let app = build_router(state);

    Fixture { app, alice, bob }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(k) = key {
        builder = builder.header("x-api-key", k);
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn healthz_and_readyz_need_no_auth() {
    let fx = fixture().await;
    let (s1, _) = send(&fx.app, "GET", "/healthz", None, Value::Null).await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, _) = send(&fx.app, "GET", "/readyz", None, Value::Null).await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn missing_or_wrong_api_key_is_401() {
    let fx = fixture().await;
    let (s1, _) = send(&fx.app, "GET", "/v1/reserve", None, Value::Null).await;
    assert_eq!(s1, StatusCode::UNAUTHORIZED);
    let (s2, _) = send(&fx.app, "GET", "/v1/reserve", Some("nope"), Value::Null).await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
    let (s3, _) = send(&fx.app, "GET", "/v1/reserve", Some(API_KEY), Value::Null).await;
    assert_eq!(s3, StatusCode::OK);
}

#[tokio::test]
async fn create_account_and_zero_balance() {
    let fx = fixture().await;
    let (status, body) = send(
        &fx.app,
        "POST",
        "/v1/accounts",
        Some(API_KEY),
        json!({ "kind": "User" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["data"]["account_id"].as_str().unwrap().to_string();

    let (s, b) = send(
        &fx.app,
        "GET",
        &format!("/v1/accounts/{id}/balance"),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["available_micros"], 0);
    assert_eq!(b["data"]["held_micros"], 0);
}

#[tokio::test]
async fn address_round_trips_and_unknown_is_404() {
    let fx = fixture().await;
    let (s, b) = send(
        &fx.app,
        "POST",
        &format!("/v1/accounts/{}/addresses", fx.alice.as_str()),
        Some(API_KEY),
        json!({ "purpose": "payout" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let addr = b["data"]["address"].as_str().unwrap().to_string();
    assert!(addr.starts_with("ait1"));

    let (s2, b2) = send(
        &fx.app,
        "GET",
        &format!("/v1/addresses/{addr}"),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["data"]["account_id"], fx.alice.as_str());
}

#[tokio::test]
async fn transfer_happy_overspend_and_idempotency() {
    let fx = fixture().await;
    let body = |amt: i64, key: &str| {
        json!({
            "from": fx.alice.as_str(),
            "to": fx.bob.as_str(),
            "amount_micros": amt,
            "idem_key": key,
        })
    };

    // Happy: moves money.
    let (s, b) = send(
        &fx.app,
        "POST",
        "/v1/transfers",
        Some(API_KEY),
        body(4_000_000, "k1"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["idempotent_replay"], false);

    // Idempotent replay: same key + body.
    let (s2, b2) = send(
        &fx.app,
        "POST",
        "/v1/transfers",
        Some(API_KEY),
        body(4_000_000, "k1"),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["data"]["idempotent_replay"], true);

    // Conflict: same key, different amount → 409.
    let (s3, b3) = send(
        &fx.app,
        "POST",
        "/v1/transfers",
        Some(API_KEY),
        body(9_000_000, "k1"),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT);
    assert_eq!(b3["error"]["code"], "idempotency_conflict");

    // Overspend from bob: bob holds only the 4 AIT received above; 9 AIT → 402.
    let over = json!({
        "from": fx.bob.as_str(),
        "to": fx.alice.as_str(),
        "amount_micros": 9_000_000,
        "idem_key": "k2",
    });
    let (s4, b4) = send(&fx.app, "POST", "/v1/transfers", Some(API_KEY), over).await;
    assert_eq!(s4, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(b4["error"]["code"], "insufficient_funds");
}

#[tokio::test]
async fn hold_create_capture_and_get() {
    let fx = fixture().await;
    let (s, b) = send(
        &fx.app,
        "POST",
        "/v1/holds",
        Some(API_KEY),
        json!({ "from": fx.alice.as_str(), "amount_micros": 4_000_000, "idem_key": "h1" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let hold_id = b["data"]["hold_id"].as_str().unwrap().to_string();

    let (s2, b2) = send(
        &fx.app,
        "GET",
        &format!("/v1/holds/{hold_id}"),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["data"]["status"], "Open");

    let (s3, b3) = send(
        &fx.app,
        "POST",
        &format!("/v1/holds/{hold_id}/capture"),
        Some(API_KEY),
        json!({ "amount_micros": 2_500_000, "to": fx.bob.as_str(), "idem_key": "c1" }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(b3["data"]["status"], "Captured");
    assert_eq!(b3["data"]["captured_micros"], 2_500_000);
}

#[tokio::test]
async fn hold_release_restores_available_and_is_released() {
    let fx = fixture().await;
    // Reserve 3 AIT from alice's 10 AIT.
    let (s, b) = send(
        &fx.app,
        "POST",
        "/v1/holds",
        Some(API_KEY),
        json!({ "from": fx.alice.as_str(), "amount_micros": 3_000_000, "idem_key": "hr1" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let hold_id = b["data"]["hold_id"].as_str().unwrap().to_string();

    // Available dropped to 7 AIT, 3 AIT held.
    let (sb, bb) = send(
        &fx.app,
        "GET",
        &format!("/v1/accounts/{}/balance", fx.alice.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(sb, StatusCode::OK);
    assert_eq!(bb["data"]["available_micros"], 7_000_000);
    assert_eq!(bb["data"]["held_micros"], 3_000_000);

    // Release the whole hold — the success-only failure path (no settlement).
    let (s2, b2) = send(
        &fx.app,
        "POST",
        &format!("/v1/holds/{hold_id}/release"),
        Some(API_KEY),
        json!({ "idem_key": "rel1" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["data"]["status"], "Released");
    assert_eq!(b2["data"]["captured_micros"], 0);

    // GET reflects the terminal Released state.
    let (s3, b3) = send(
        &fx.app,
        "GET",
        &format!("/v1/holds/{hold_id}"),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(b3["data"]["status"], "Released");

    // Funds fully restored: back to 10 AIT available, 0 held.
    let (s4, b4) = send(
        &fx.app,
        "GET",
        &format!("/v1/accounts/{}/balance", fx.alice.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s4, StatusCode::OK);
    assert_eq!(b4["data"]["available_micros"], 10_000_000);
    assert_eq!(b4["data"]["held_micros"], 0);
}

#[tokio::test]
async fn aitp_verify_settles_transfer_over_http() {
    // A dedicated fixture that registers the payer's signing key so the real
    // `Aitp402` state machine can Ed25519-verify the authorization.
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(1_000_000));
    let store = InMemoryStore::with_clock(clock.clone());

    let alice = store.create_account(AccountKind::User, None).await.unwrap();
    store
        .seed_account(&alice, Micros::new(10_000_000))
        .await
        .unwrap();
    let bob = store
        .create_account(AccountKind::Model, None)
        .await
        .unwrap();
    let bob_addr = store.create_address(&bob, None).await.unwrap();

    // The payer's real Ed25519 key, registered for (payer, pubkey_id).
    let signer = Ed25519Signer::generate("payer-key-1");
    let mut keys = InMemoryPayerKeys::new();
    keys.insert(
        alice.as_str(),
        signer.key_id().as_str(),
        signer.verifying_key(),
    );

    let ramp = Arc::new(FakeRamp::new());
    let kyc = Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX));
    let state = AppState::new(
        store,
        clock,
        ramp,
        kyc,
        Arc::new(keys),
        API_KEY,
        "whsec_test",
    );
    let app = build_router(state);

    // (1) Issue the 402 challenge over HTTP.
    let resource = "POST /v1/chat/completions#llama-3.1-70b";
    let amount: i64 = 2_000_000;
    let (cs, cb) = send(
        &app,
        "POST",
        "/v1/aitp/challenge",
        Some(API_KEY),
        json!({
            "payee": bob_addr.as_str(),
            "amount_micros": amount,
            "resource": resource,
            "scheme": "aitp-transfer",
            "ttl_ms": 60_000,
        }),
    )
    .await;
    assert_eq!(cs, StatusCode::PAYMENT_REQUIRED);
    let nonce = cb["nonce"].as_str().unwrap().to_string();
    let expiry = cb["expiry"].as_i64().unwrap();

    // (2) The payer signs the matching authorization.
    let challenge = Challenge {
        payee: bob_addr.as_str().to_string(),
        amount_micros: Micros::new(amount),
        currency: "AIT".to_string(),
        resource: resource.to_string(),
        nonce,
        expiry,
        scheme: Scheme::Transfer,
    };
    let auth = build_authorization(alice.as_str(), &challenge, "idem-aitp-1", &signer);
    let auth_json = serde_json::to_value(&auth).unwrap();

    // (3) Verify + settle over HTTP → transfer settlement.
    let (vs, vb) = send(&app, "POST", "/v1/aitp/verify", Some(API_KEY), auth_json).await;
    assert_eq!(vs, StatusCode::OK);
    assert_eq!(vb["success"], true);
    assert_eq!(vb["data"]["scheme"], "transfer");
    assert_eq!(vb["data"]["receipt"]["idempotent_replay"], false);

    // Money actually moved: bob now holds 2 AIT, alice 8 AIT.
    let (_, bob_bal) = send(
        &app,
        "GET",
        &format!("/v1/accounts/{}/balance", bob.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(bob_bal["data"]["available_micros"], amount);

    let (_, alice_bal) = send(
        &app,
        "GET",
        &format!("/v1/accounts/{}/balance", alice.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(alice_bal["data"]["available_micros"], 8_000_000);
}

#[tokio::test]
async fn aitp_verify_rejects_expired_authorization() {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(1_000_000));
    let store = InMemoryStore::with_clock(clock.clone());
    let alice = store.create_account(AccountKind::User, None).await.unwrap();
    store
        .seed_account(&alice, Micros::new(10_000_000))
        .await
        .unwrap();
    let bob = store
        .create_account(AccountKind::Model, None)
        .await
        .unwrap();
    let bob_addr = store.create_address(&bob, None).await.unwrap();

    let signer = Ed25519Signer::generate("payer-key-1");
    let mut keys = InMemoryPayerKeys::new();
    keys.insert(
        alice.as_str(),
        signer.key_id().as_str(),
        signer.verifying_key(),
    );

    let ramp = Arc::new(FakeRamp::new());
    let kyc = Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX));
    let state = AppState::new(
        store,
        clock,
        ramp,
        kyc,
        Arc::new(keys),
        API_KEY,
        "whsec_test",
    );
    let app = build_router(state);

    // Sign a challenge whose expiry is already in the past (never issued via the
    // server, but the expiry gate fires before the issued-challenge lookup).
    let challenge = Challenge {
        payee: bob_addr.as_str().to_string(),
        amount_micros: Micros::new(1_000_000),
        currency: "AIT".to_string(),
        resource: "r".to_string(),
        nonce: "n-expired".to_string(),
        expiry: 500_000, // < clock now (1_000_000)
        scheme: Scheme::Transfer,
    };
    let auth = build_authorization(alice.as_str(), &challenge, "idem-x", &signer);
    let (s, b) = send(
        &app,
        "POST",
        "/v1/aitp/verify",
        Some(API_KEY),
        serde_json::to_value(&auth).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"]["code"], "authorization_expired");
}

#[tokio::test]
async fn unknown_account_balance_is_404_with_envelope() {
    let fx = fixture().await;
    let ghost = AccountId::new(AccountKind::User, ulid::Ulid::new());
    let (s, b) = send(
        &fx.app,
        "GET",
        &format!("/v1/accounts/{}/balance", ghost.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b["success"], false);
    assert_eq!(b["error"]["code"], "account_not_found");
}

#[tokio::test]
async fn receipts_chain_and_audit_root() {
    let fx = fixture().await;
    // Make a transfer so there are receipts.
    send(
        &fx.app,
        "POST",
        "/v1/transfers",
        Some(API_KEY),
        json!({ "from": fx.alice.as_str(), "to": fx.bob.as_str(), "amount_micros": 1_000_000, "idem_key": "r1" }),
    )
    .await;

    let (s, b) = send(
        &fx.app,
        "GET",
        &format!("/v1/accounts/{}/receipts", fx.bob.as_str()),
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(b["data"].as_array().unwrap().len() >= 1);

    let (s2, b2) = send(
        &fx.app,
        "GET",
        "/v1/receipts/audit",
        Some(API_KEY),
        Value::Null,
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert!(b2["data"]["merkle_root"].as_str().unwrap().len() == 64);
}
