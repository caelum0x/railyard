//! White-label kit end to end over the testkit fakes: a founder's backend
//! creates an end-user wallet, gets a hosted top-up link, the user's wallet is
//! funded, and an `end_user_reserve` app charges each request to THAT wallet at
//! the app's per-model markup, never touching the app account.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use vend_core::money::{FundingMode, ModelMarkup};
use vend_core::transport::Usage;
use vend_server::pricing::{ModelRate, PricingCatalog};
use vend_server::whitelabel::{build_wallet_router, WalletApiState};
use vend_server::{build_router, AppState};
use vend_settlement::PayoutConfig;
use vend_testkit::fixtures::{two_tenants, TwoTenants};
use vend_testkit::{FakeMint, FakeRelay, InMemoryStore};

const MODEL: &str = "gpt-4o-mini";

fn data_plane(store: Arc<InMemoryStore>, mint: Arc<FakeMint>, relay: Arc<FakeRelay>) -> Router {
    build_router(AppState {
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
                MODEL.to_string(),
                ModelRate {
                    input_micros_per_million_tokens: 10_000_000,
                    output_micros_per_million_tokens: 10_000_000,
                    max_input_tokens: 100_000,
                    max_output_tokens: 1_000_000,
                },
            )])
            .unwrap(),
        ),
        relay_base_path: "/openai/v1".into(),
        relay_key: "relay-test-key".into(),
        settlement_account: "acct_vend_settlement".into(),
        payout_config: PayoutConfig::new(
            "acct_vend_settlement",
            "acct_provider_payout",
            "acct_platform_payout",
        )
        .unwrap(),
        security_ops: None,
    })
}

fn wallets(store: Arc<InMemoryStore>, mint: Arc<FakeMint>) -> Router {
    build_wallet_router(WalletApiState {
        keys: store.clone(),
        tenants: store.clone(),
        usage: store,
        mint,
        security: None,
    })
}

fn json_request(method: &str, uri: &str, key: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Tenant A's app switched to end-user wallets with a 100% markup on MODEL.
fn wallet_first_fixture() -> (Arc<InMemoryStore>, String, String) {
    let TwoTenants { store, a, .. } = two_tenants();
    let mut app = a.app.clone();
    app.markup_policy.funding_mode = FundingMode::EndUserReserve;
    app.markup_policy.min_fee_micros = 0;
    app.model_markups.insert(
        MODEL.to_string(),
        ModelMarkup {
            markup_bps: 10_000,
            min_fee_micros: None,
        },
    );
    store.add_app(app.clone());
    (
        Arc::new(store),
        a.key_secret.clone(),
        app.mint_account_id.clone(),
    )
}

#[tokio::test]
async fn wallet_api_creates_idempotent_wallets_and_top_up_links() {
    let (store, key, _) = wallet_first_fixture();
    let mint = Arc::new(FakeMint::new());
    let router = wallets(store.clone(), mint.clone());

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/v1/wallets",
            &key,
            serde_json::json!({"external_ref": "user-1"}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = body_json(created).await;
    assert_eq!(created["data"]["created"], true);
    assert_eq!(created["data"]["funding_mode"], "end_user_reserve");
    let account = created["data"]["mint_account_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(mint.created_accounts().len(), 1);
    assert_eq!(mint.created_accounts()[0].1.kind, "user");

    // Creating again returns the same wallet without opening another account.
    let again = body_json(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/v1/wallets",
                &key,
                serde_json::json!({"external_ref": "user-1"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(again["data"]["created"], false);
    assert_eq!(again["data"]["mint_account_id"], account.as_str());
    assert_eq!(mint.created_accounts().len(), 1);

    let top_up = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/v1/wallets/user-1/top-ups",
            &key,
            serde_json::json!({"provider": "creem", "amount_micros": 20_000_000}),
        ))
        .await
        .unwrap();
    assert_eq!(top_up.status(), StatusCode::OK);
    assert_eq!(top_up.headers()["cache-control"], "no-store");
    let top_up = body_json(top_up).await;
    assert_eq!(top_up["data"]["account_id"], account.as_str());
    assert!(top_up["data"]["checkout_url"]
        .as_str()
        .unwrap()
        .starts_with("https://"));

    let bad_provider = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/v1/wallets/user-1/top-ups",
            &key,
            serde_json::json!({"provider": "venmo", "amount_micros": 20_000_000}),
        ))
        .await
        .unwrap();
    assert_eq!(bad_provider.status(), StatusCode::BAD_REQUEST);

    let unauthenticated = router
        .oneshot(json_request(
            "GET",
            "/v1/wallets/user-1",
            "vk_test_nope_nope",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn end_user_reserve_charges_the_callers_wallet_at_the_model_markup() {
    let (store, key, app_account) = wallet_first_fixture();
    let mint = Arc::new(FakeMint::new());
    let created = body_json(
        wallets(store.clone(), mint.clone())
            .oneshot(json_request(
                "POST",
                "/v1/wallets",
                &key,
                serde_json::json!({"external_ref": "user-7"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let wallet = created["data"]["mint_account_id"]
        .as_str()
        .unwrap()
        .to_string();
    mint.credit(&wallet, 100_000_000);

    let relay = Arc::new(FakeRelay::new().push(FakeRelay::canned_success(Usage {
        input_tokens: 3,
        output_tokens: 2,
        provider_cost_micros: Some(50),
    })));
    let router = data_plane(store.clone(), mint.clone(), relay);
    let body = serde_json::json!({"model": MODEL, "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}]});

    let mut request = json_request("POST", "/v1/chat/completions", &key, body.clone());
    request
        .headers_mut()
        .insert("idempotency-key", "wl-1".parse().unwrap());
    request
        .headers_mut()
        .insert("vend-end-user", "user-7".parse().unwrap());
    let resp = router.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 50 µ$ provider cost at the 100% model override = 100 µ$ charged; the
    // platform keeps ceil(10% of the 50 µ$ margin) = 5, the founder 45.
    let charge_id = resp.headers()["Vend-Charge-Id"]
        .to_str()
        .unwrap()
        .to_string();
    let usage = store.usage_by_charge_id(&charge_id).unwrap();
    assert_eq!(usage.charge_micros, 100);
    assert_eq!(usage.platform_fee_micros, 5);
    assert_eq!(usage.tenant_net_micros, 45);
    assert_eq!(mint.balance(&wallet), 100_000_000 - 100);
    assert_eq!(
        mint.balance(&app_account),
        0,
        "app account is never charged"
    );
    assert_eq!(mint.balance("acct_provider_payout"), 50);
    assert_eq!(mint.balance("acct_platform_payout"), 5);

    // Without a wallet reference the wallet-funded app refuses to charge anyone.
    let mut anonymous = json_request("POST", "/v1/chat/completions", &key, body);
    anonymous
        .headers_mut()
        .insert("idempotency-key", "wl-2".parse().unwrap());
    let refused = router.clone().oneshot(anonymous).await.unwrap();
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    // /v1/models advertises the marked-up (2x) per-model rate.
    let models = router
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let models = body_json(models).await;
    assert_eq!(
        models["data"][0]["pricing"]["input_micros_per_million_tokens"],
        20_000_000
    );
}
