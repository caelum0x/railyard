//! RY-MT1: shadow mode refuses money movement, and `GET /metrics` reports the
//! ledger gauges and the receipt hash-chain verification.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use mint_core::{AccountId, AccountKind, Micros};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::{Ed25519Signer, FixedSigner, Signer};
use mint_reserve::{FakeKycGate, KycTier, Ramp};
use mint_server::metrics::{self, MetricsState};
use mint_server::shadow::ShadowRamp;
use mint_server::{build_router, AppState};
use mint_store::{Clock, InMemoryStore, LedgerStore, SystemClock};
use serde_json::{json, Value};
use tower::ServiceExt;

const API_KEY: &str = "test-secret-key";

struct Fx {
    state: AppState<InMemoryStore>,
    alice: AccountId,
    bob: AccountId,
}

async fn fx_with(ramp: Arc<dyn Ramp>, receipt_signer: Arc<dyn Signer>) -> Fx {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
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
    let state = AppState::new(
        store,
        clock,
        ramp,
        // Even a permissive KYC gate cannot get money out in shadow mode.
        Arc::new(FakeKycGate::new(KycTier::Verified, i64::MAX)),
        Arc::new(InMemoryPayerKeys::new()),
        receipt_signer,
        API_KEY,
        "whsec_test",
    );
    Fx { state, alice, bob }
}

async fn shadow_fx() -> Fx {
    fx_with(Arc::new(ShadowRamp), Arc::new(FixedSigner::new())).await
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(serde_json::to_vec(&body).unwrap())
    };
    let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn available(app: &Router, id: &AccountId) -> i64 {
    let (s, b) = call(
        app,
        "GET",
        &format!("/v1/accounts/{}/balance", id.as_str()),
        &[("x-api-key", API_KEY)],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    serde_json::from_str::<Value>(&b).unwrap()["data"]["available_micros"]
        .as_i64()
        .unwrap()
}

fn metric(text: &str, name: &str) -> Option<String> {
    text.lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
}

fn metrics_app(state: AppState<InMemoryStore>, m: Arc<MetricsState>) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(metrics::metrics::<InMemoryStore>).layer(Extension(m)),
        )
        .with_state(state)
}

fn open_metrics(shadow: bool) -> Arc<MetricsState> {
    Arc::new(MetricsState::new(
        None,
        shadow,
        Duration::ZERO,
        Duration::from_secs(86_400),
    ))
}

#[tokio::test]
async fn shadow_refuses_a_deposit_backed_mint_and_credits_nothing() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    let (s, body) = call(
        &app,
        "POST",
        "/v1/mint",
        &[("x-api-key", API_KEY)],
        json!({"to": fx.bob.as_str(), "amount_micros": 5_000_000, "deposit_ref": "stripe:pi_123", "idem_key": "m1"}),
    )
    .await;
    assert!(
        s.is_client_error(),
        "mint must be refused in shadow mode, got {s}: {body}"
    );
    assert_eq!(available(&app, &fx.bob).await, 0);
    assert_eq!(
        fx.state
            .ledger
            .store()
            .try_reserve_micros()
            .await
            .unwrap()
            .get(),
        0
    );
}

#[tokio::test]
async fn shadow_refuses_a_redeem_before_burning_funds() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    let before = available(&app, &fx.alice).await;
    let (s, body) = call(
        &app,
        "POST",
        "/v1/redeem",
        &[("x-api-key", API_KEY)],
        json!({"from": fx.alice.as_str(), "amount_micros": 1_000_000, "payout_ref": "po_1",
               "kyc_tier": "verified", "payout_dest": {"kind": "paypal", "email": "a@example.com"}, "idem_key": "r1"}),
    )
    .await;
    assert!(
        !s.is_success(),
        "redeem must be refused in shadow mode, got {s}: {body}"
    );
    assert_eq!(
        available(&app, &fx.alice).await,
        before,
        "no funds may be burned"
    );
}

#[tokio::test]
async fn shadow_still_serves_the_ledger_and_readyz() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    let (s, _) = call(&app, "GET", "/readyz", &[], Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let (s, body) = call(
        &app,
        "POST",
        "/v1/transfers",
        &[("x-api-key", API_KEY)],
        json!({"from": fx.alice.as_str(), "to": fx.bob.as_str(), "amount_micros": 250_000, "idem_key": "t1"}),
    )
    .await;
    assert!(
        s.is_success(),
        "internal ledger moves still work in shadow: {s} {body}"
    );
    assert_eq!(available(&app, &fx.bob).await, 250_000);
}

#[tokio::test]
async fn metrics_report_ledger_gauges_and_a_verified_receipt_chain() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    for (i, amount) in [100_000, 200_000].iter().enumerate() {
        let (s, b) = call(
            &app,
            "POST",
            "/v1/transfers",
            &[("x-api-key", API_KEY)],
            json!({"from": fx.alice.as_str(), "to": fx.bob.as_str(), "amount_micros": amount, "idem_key": format!("t{i}")}),
        )
        .await;
        assert!(s.is_success(), "{b}");
    }
    let (s, b) = call(
        &app,
        "POST",
        "/v1/holds",
        &[("x-api-key", API_KEY)],
        json!({"from": fx.alice.as_str(), "amount_micros": 300_000, "idem_key": "h1"}),
    )
    .await;
    assert!(s.is_success(), "{b}");

    let m = open_metrics(true);
    let mapp = metrics_app(fx.state.clone(), m);
    let (s, text) = call(&mapp, "GET", "/metrics", &[], Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(metric(&text, "mint_up").as_deref(), Some("1"));
    assert_eq!(metric(&text, "mint_ready").as_deref(), Some("1"));
    assert_eq!(metric(&text, "mint_shadow_mode").as_deref(), Some("1"));
    assert_eq!(metric(&text, "mint_holds_open").as_deref(), Some("1"));
    assert_eq!(
        metric(&text, "mint_holds_open_micros").as_deref(),
        Some("300000")
    );
    assert_eq!(
        metric(&text, "mint_hold_sweep_lag_seconds").as_deref(),
        Some("0")
    );
    assert_eq!(metric(&text, "mint_payouts_pending").as_deref(), Some("0"));
    assert_eq!(
        metric(&text, "mint_receipt_chain_verify_ok").as_deref(),
        Some("1"),
        "{text}"
    );
    let checked: u64 = metric(&text, "mint_receipt_chain_verified_receipts")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        checked >= 4,
        "transfers and the hold wrote receipts, got {checked}"
    );
    assert!(text.contains("mint_metrics_check_failed{check=\"holds\"} 0"));
    assert!(text.contains("# TYPE mint_receipt_chain_verify_runs_total counter"));
}

#[tokio::test]
async fn chain_verify_reports_zero_when_signatures_do_not_match_the_published_key() {
    // Receipts are signed by the store's fixed signer, but the server publishes
    // (and verifies against) a different key: the check must fail, not pass.
    // (Mint itself refuses new transfers in this state, so the seed receipt is used.)
    let other: Arc<dyn Signer> = Arc::new(Ed25519Signer::from_seed([7u8; 32], "other-key"));
    let fx = fx_with(Arc::new(ShadowRamp), other).await;
    // The seeded balance already wrote a receipt with the store's signer.
    assert!(!fx
        .state
        .ledger
        .store()
        .try_snapshot_receipts()
        .await
        .unwrap()
        .is_empty());
    let mapp = metrics_app(fx.state.clone(), open_metrics(false));
    let (_, text) = call(&mapp, "GET", "/metrics", &[], Value::Null).await;
    assert_eq!(
        metric(&text, "mint_receipt_chain_verify_ok").as_deref(),
        Some("0"),
        "{text}"
    );
    assert_eq!(
        metric(&text, "mint_receipt_chain_verify_failures_total").as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn metrics_through_a_proxy_are_hidden_without_a_token() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    let (s, _) = call(
        &app,
        "GET",
        "/metrics",
        &[("x-forwarded-for", "203.0.113.9")],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        "GET",
        "/metrics",
        &[("forwarded", "for=203.0.113.9")],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, text) = call(&app, "GET", "/metrics", &[], Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert!(text.contains("mint_up 1"));
}

#[tokio::test]
async fn metrics_token_is_required_when_configured() {
    let fx = shadow_fx().await;
    let m = Arc::new(MetricsState::new(
        Some("scrape-token".into()),
        true,
        Duration::ZERO,
        Duration::from_secs(86_400),
    ));
    let app = metrics_app(fx.state.clone(), m);
    let (s, _) = call(&app, "GET", "/metrics", &[], Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        "GET",
        "/metrics",
        &[("authorization", "Bearer wrong")],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, text) = call(
        &app,
        "GET",
        "/metrics",
        &[("authorization", "Bearer scrape-token")],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(text.contains("mint_ready 1"));
}

#[tokio::test]
async fn expired_open_holds_show_as_sweep_lag() {
    let fx = shadow_fx().await;
    let app = build_router(fx.state.clone());
    let (s, b) = call(
        &app,
        "POST",
        "/v1/holds",
        &[("x-api-key", API_KEY)],
        json!({"from": fx.alice.as_str(), "amount_micros": 10_000, "idem_key": "h-exp", "expires_in_ms": 1_000}),
    )
    .await;
    assert!(s.is_success(), "{b}");
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    let mapp = metrics_app(fx.state.clone(), open_metrics(true));
    let (_, text) = call(&mapp, "GET", "/metrics", &[], Value::Null).await;
    assert_eq!(
        metric(&text, "mint_holds_expired_unreleased").as_deref(),
        Some("1"),
        "{text}"
    );
    let lag: i64 = metric(&text, "mint_hold_sweep_lag_seconds")
        .unwrap()
        .parse()
        .unwrap();
    assert!(lag >= 1, "lag {lag}");
}
