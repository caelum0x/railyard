//! Operator-plane integration tests: account freeze / close, account-scoped
//! DB-backed API keys, deposit reversals and provider-report reconciliation,
//! driven through the real router with an in-memory store.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mint_core::{AccountId, AccountKind, Micros};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::FixedSigner;
use mint_reserve::{Asset, FakeKycGate, FakeRamp, KycTier};
use mint_server::{build_router, AppState};
use mint_store::{Clock, FixedClock, InMemoryStore, LedgerStore};
use serde_json::{json, Value};
use tower::ServiceExt;

const API_KEY: &str = "test-secret-key";

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
    let bob = store.create_account(AccountKind::User, None).await.unwrap();
    let ramp =
        Arc::new(FakeRamp::new().with_confirmed_deposit("stripe:pi_1", 5_000_000, Asset::Usd));
    let state = AppState::new(
        store,
        clock,
        ramp,
        Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX)),
        Arc::new(InMemoryPayerKeys::new()),
        Arc::new(FixedSigner::new()),
        API_KEY,
        "whsec_test",
    );
    Fixture {
        app: build_router(state),
        alice,
        bob,
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-api-key", key)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn transfer(from: &AccountId, to: &AccountId, amount: i64, key: &str) -> Value {
    json!({ "from": from.as_str(), "to": to.as_str(), "amount_micros": amount, "idem_key": key })
}

#[tokio::test]
async fn freeze_blocks_outgoing_but_not_incoming_and_close_needs_zero_balance() {
    let f = fixture().await;
    let alice = f.alice.as_str();

    let (s, v) = send(
        &f.app,
        "GET",
        &format!("/v1/accounts/{alice}"),
        API_KEY,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["status"], "active");
    assert!(v["data"]["created_ms"].is_i64());

    let (s, v) = send(
        &f.app,
        "POST",
        &format!("/v1/accounts/{alice}/freeze"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["status"], "frozen");

    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/transfers",
        API_KEY,
        transfer(&f.alice, &f.bob, 1, "t1"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "account_frozen");

    // Incoming credits still land on a frozen account.
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/mint",
        API_KEY,
        json!({ "to": alice, "amount_micros": 5_000_000, "deposit_ref": "stripe:pi_1" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // Close refused while funded.
    let (s, v) = send(
        &f.app,
        "POST",
        &format!("/v1/accounts/{alice}/close"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "account_not_empty");

    // Bob (empty) closes; then nothing can reach him and he cannot reopen.
    let bob = f.bob.as_str();
    let (s, v) = send(
        &f.app,
        "POST",
        &format!("/v1/accounts/{bob}/close"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = send(
        &f.app,
        "POST",
        &format!("/v1/accounts/{alice}/unfreeze"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/transfers",
        API_KEY,
        transfer(&f.alice, &f.bob, 1, "t2"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "account_closed");
    let (s, v) = send(
        &f.app,
        "POST",
        &format!("/v1/accounts/{bob}/unfreeze"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "account_closed");
}

#[tokio::test]
async fn account_scoped_db_key_is_confined_and_revocable() {
    let f = fixture().await;
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/keys",
        API_KEY,
        json!({ "name": "alice-app", "scopes": ["read", "spend"], "accounts": [f.alice.as_str()] }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let secret = v["data"]["secret"].as_str().unwrap().to_string();
    let key_id = v["data"]["id"].as_str().unwrap().to_string();

    // Own account: allowed.
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/transfers",
        &secret,
        transfer(&f.alice, &f.bob, 1, "k1"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = send(
        &f.app,
        "GET",
        &format!("/v1/accounts/{}", f.alice.as_str()),
        &secret,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Someone else's account / global views / operator routes: refused.
    let (s, _) = send(
        &f.app,
        "POST",
        "/v1/transfers",
        &secret,
        transfer(&f.bob, &f.alice, 1, "k2"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(
        &f.app,
        "GET",
        &format!("/v1/accounts/{}", f.bob.as_str()),
        &secret,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&f.app, "GET", "/v1/accounts", &secret, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&f.app, "GET", "/v1/keys", &secret, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, _) = send(
        &f.app,
        "POST",
        &format!("/v1/keys/{key_id}/revoke"),
        API_KEY,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        &f.app,
        "GET",
        &format!("/v1/accounts/{}", f.alice.as_str()),
        &secret,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reversal_claws_back_and_report_matches() {
    let f = fixture().await;
    let bob = f.bob.as_str();
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/mint",
        API_KEY,
        json!({ "to": bob, "amount_micros": 5_000_000, "deposit_ref": "stripe:pi_1" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // Provider report: the deposit plus a refund we have not recorded yet.
    let csv = "Payment Intent ID,Reporting Category,Gross,Currency\npi_1,charge,5.00,usd\npi_1,refund,-2.00,usd\npi_9,charge,1.00,usd\n";
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/reconciliation/reports",
        API_KEY,
        json!({ "provider": "stripe", "content": csv }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let summary = &v["data"]["summary"];
    assert_eq!(summary["matched"]["count"], 1);
    assert_eq!(summary["reversal_unrecorded"]["count"], 1);
    assert_eq!(summary["missing_in_mint"]["count"], 1);
    let report_id = v["data"]["report"]["id"].as_str().unwrap().to_string();

    // Bob spends 4 of 5, then a 2 refund can only claw back 1: shortfall.
    let (s, _) = send(
        &f.app,
        "POST",
        "/v1/transfers",
        API_KEY,
        transfer(&f.bob, &f.alice, 4_000_000, "spend"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = send(
        &f.app,
        "POST",
        "/v1/reversals",
        API_KEY,
        json!({ "deposit_ref": "stripe:pi_1", "amount_micros": 2_000_000 }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["data"]["status"], "shortfall");
    assert_eq!(v["data"]["clawed_micros"], 1_000_000);

    // Re-matching now sees the recorded reversal.
    let (s, v) = send(
        &f.app,
        "GET",
        &format!("/v1/reconciliation/reports/{report_id}"),
        API_KEY,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["summary"]["reversal_unrecorded"]["count"], 0);

    // The shortfall is on the operator worklist.
    let (s, v) = send(&f.app, "GET", "/v1/reconciliation", API_KEY, Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["totals"]["reversal_shortfall_micros"], 1_000_000);
}
