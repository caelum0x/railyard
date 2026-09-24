//! Route tests for the IAP credit hooks (`/v1/iap/*`) over the real router with
//! the in-memory FakeMint: the credit amount comes from config, replays never
//! credit twice, refunds reverse, and both auth paths fail closed.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use game_economy_core::{Catalog, GameStore};
use game_economy_server::{auth, AppState, IapConfig};
use game_economy_testkit::FakeMint;

const PLAYER: &str = "acct_player";
const FUNDING: &str = "acct_iap_funding";
const REVENUE: &str = "acct_revenue";
const SERVER_SECRET: &str = "test-game-server-secret";
const RC_AUTH: &str = "Bearer rc-webhook-secret-123";
const COINS: &str = "coins_550";
const COINS_MICROS: i64 = 4_990_000;

struct Fixture {
    app: Router,
    store: Arc<GameStore<FakeMint>>,
}

fn fixture(funding_balance: i64, with_iap: bool) -> Fixture {
    let mint = FakeMint::new().with_balance(FUNDING, funding_balance);
    let store = Arc::new(GameStore::new(mint, Catalog::demo(), REVENUE));
    let mut state = AppState::new(store.clone(), auth::derive_key(b"secret"))
        .with_server_secret(SERVER_SECRET.as_bytes());
    if with_iap {
        let products = IapConfig::parse_products("coins_100=990000, coins_550=4990000").unwrap();
        state = state.with_iap(
            IapConfig::new(FUNDING, products)
                .unwrap()
                .with_revenuecat_auth(RC_AUTH.as_bytes()),
        );
    }
    Fixture {
        app: game_economy_server::build_router(state),
        store,
    }
}

fn server_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(auth::SERVER_KEY_HEADER, SERVER_SECRET)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn rc_post(auth_value: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/v1/iap/revenuecat")
        .header("content-type", "application/json");
    if let Some(v) = auth_value {
        b = b.header("authorization", v);
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
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

fn credit_body(tx: &str) -> Value {
    json!({ "player": PLAYER, "platform": "app_store", "transaction_id": tx, "product_id": COINS })
}

#[tokio::test]
async fn credit_moves_configured_amount_once() {
    let fx = fixture(100_000_000, true);
    let (status, body) = send(&fx.app, server_post("/v1/iap/credit", credit_body("tx-1"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["amount_micros"], json!(COINS_MICROS));
    assert_eq!(body["data"]["idempotent_replay"], json!(false));
    assert_eq!(fx.store.mint().balance(PLAYER), COINS_MICROS);

    // Replay of the same store transaction never credits twice.
    let (status, body) = send(&fx.app, server_post("/v1/iap/credit", credit_body("tx-1"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["idempotent_replay"], json!(true));
    assert_eq!(fx.store.mint().balance(PLAYER), COINS_MICROS);
}

#[tokio::test]
async fn reverse_takes_currency_back_and_reports_spent_currency() {
    let fx = fixture(100_000_000, true);
    send(&fx.app, server_post("/v1/iap/credit", credit_body("tx-2"))).await;
    let (status, _) = send(&fx.app, server_post("/v1/iap/reverse", credit_body("tx-2"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.store.mint().balance(PLAYER), 0);
    assert_eq!(fx.store.mint().balance(FUNDING), 100_000_000);

    // Nothing left to reverse for a second, different transaction.
    let (status, body) = send(&fx.app, server_post("/v1/iap/reverse", credit_body("tx-3"))).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], json!("currency_already_spent"));
}

#[tokio::test]
async fn unknown_product_and_exhausted_funding_are_rejected() {
    let fx = fixture(1_000, true);
    let mut bad = credit_body("tx-4");
    bad["product_id"] = json!("not_a_product");
    let (status, body) = send(&fx.app, server_post("/v1/iap/credit", bad)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], json!("unknown_iap_product"));

    let (status, body) = send(&fx.app, server_post("/v1/iap/credit", credit_body("tx-5"))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], json!("iap_funding_exhausted"));
    assert_eq!(fx.store.mint().balance(PLAYER), 0);
}

#[tokio::test]
async fn credit_requires_server_key_and_config() {
    let fx = fixture(100_000_000, true);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/iap/credit")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&credit_body("tx-6")).unwrap(),
        ))
        .unwrap();
    let (status, _) = send(&fx.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let off = fixture(100_000_000, false);
    let (status, body) = send(&off.app, server_post("/v1/iap/credit", credit_body("tx-7"))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], json!("iap_not_configured"));
}

fn rc_event(kind: &str, user: &str, product: &str, tx: &str) -> Value {
    json!({ "api_version": "1.0", "event": {
        "type": kind, "app_user_id": user, "product_id": product,
        "transaction_id": tx, "store": "APP_STORE"
    }})
}

#[tokio::test]
async fn revenuecat_purchase_credits_and_refund_reverses() {
    let fx = fixture(100_000_000, true);
    let (status, body) = send(
        &fx.app,
        rc_post(
            Some(RC_AUTH),
            rc_event("NON_RENEWING_PURCHASE", PLAYER, COINS, "rc-1"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["platform"], json!("revenuecat-app_store"));
    assert_eq!(fx.store.mint().balance(PLAYER), COINS_MICROS);

    let (status, _) = send(
        &fx.app,
        rc_post(Some(RC_AUTH), rc_event("REFUND", PLAYER, COINS, "rc-1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.store.mint().balance(PLAYER), 0);
}

#[tokio::test]
async fn revenuecat_rejects_bad_auth_ignores_other_events_and_anonymous_users() {
    let fx = fixture(100_000_000, true);
    let ev = rc_event("INITIAL_PURCHASE", PLAYER, COINS, "rc-2");
    let (status, _) = send(&fx.app, rc_post(Some("Bearer wrong"), ev.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&fx.app, rc_post(None, ev)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = send(
        &fx.app,
        rc_post(
            Some(RC_AUTH),
            rc_event("CANCELLATION", PLAYER, COINS, "rc-3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["ignored"], json!(true));

    let (status, body) = send(
        &fx.app,
        rc_post(
            Some(RC_AUTH),
            rc_event("INITIAL_PURCHASE", PLAYER, "pro_monthly", "rc-4"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["ignored"], json!(true));

    let (status, body) = send(
        &fx.app,
        rc_post(
            Some(RC_AUTH),
            rc_event("INITIAL_PURCHASE", "$RCAnonymousID:abc", COINS, "rc-5"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], json!("anonymous_app_user"));
    assert_eq!(fx.store.mint().balance(PLAYER), 0);
}

#[test]
fn product_table_parsing_is_strict() {
    assert!(IapConfig::parse_products("a=1,b=2").is_ok());
    assert!(IapConfig::parse_products("a").is_err());
    assert!(IapConfig::parse_products("a=x").is_err());
    assert!(IapConfig::parse_products("a=1,a=2").is_err());
    let zero = IapConfig::parse_products("a=0").unwrap();
    assert!(IapConfig::new(FUNDING, zero).is_err());
}
