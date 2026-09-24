//! Route tests for `GET /v1/wallet/:player` (player-auth balance + inventory)
//! and `POST /v1/players` (server-auth Mint account provisioning), over the real
//! router with the in-memory FakeMint.

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
const OTHER: &str = "acct_other";
const FUNDING: &str = "acct_iap_funding";
const REVENUE: &str = "acct_revenue";
const SERVER_SECRET: &str = "test-game-server-secret";

struct Fixture {
    app: Router,
    key: [u8; 32],
}

fn fixture(player_balance: i64) -> Fixture {
    let mint = FakeMint::new()
        .with_balance(PLAYER, player_balance)
        .with_balance(FUNDING, 100_000_000);
    let store = Arc::new(GameStore::new(mint, Catalog::demo(), REVENUE));
    let key = auth::derive_key(b"secret");
    let products = IapConfig::parse_products("gems_80=800000").unwrap();
    let state = AppState::new(store, key)
        .with_server_secret(SERVER_SECRET.as_bytes())
        .with_iap(IapConfig::new(FUNDING, products).unwrap());
    Fixture {
        app: game_economy_server::build_router(state),
        key,
    }
}

fn get_as(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

fn server_post(uri: &str, body: Value, secret: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(auth::SERVER_KEY_HEADER, secret)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn token(fx: &Fixture, player: &str) -> String {
    auth::player_token_expiring(&fx.key, player, auth::now_unix() + 600)
}

#[tokio::test]
async fn player_reads_own_balance_and_inventory() {
    let fx = fixture(2_500_000);
    let t = token(&fx, PLAYER);
    let (status, body) = send(&fx.app, get_as("/v1/wallet/acct_player", Some(&t))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["data"]["player"], json!(PLAYER));
    assert_eq!(body["data"]["available_micros"], json!(2_500_000));
    assert_eq!(body["data"]["held_micros"], json!(0));
    assert!(body["data"]["inventory"].is_object());
}

#[tokio::test]
async fn balance_reflects_server_iap_credit() {
    let fx = fixture(0);
    let credit = json!({
        "player": PLAYER, "platform": "app_store",
        "transaction_id": "tx-w1", "product_id": "gems_80"
    });
    let (status, _) = send(&fx.app, server_post("/v1/iap/credit", credit, SERVER_SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    let t = token(&fx, PLAYER);
    let (_, body) = send(&fx.app, get_as("/v1/wallet/acct_player", Some(&t))).await;
    assert_eq!(body["data"]["available_micros"], json!(800_000));
}

#[tokio::test]
async fn unknown_player_reads_zero() {
    let fx = fixture(0);
    let t = token(&fx, "acct_brand_new");
    let (status, body) = send(&fx.app, get_as("/v1/wallet/acct_brand_new", Some(&t))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["available_micros"], json!(0));
}

#[tokio::test]
async fn wallet_rejects_other_players_and_missing_tokens() {
    let fx = fixture(1_000);
    let other = token(&fx, OTHER);
    let (status, body) = send(&fx.app, get_as("/v1/wallet/acct_player", Some(&other))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("forbidden"));

    let (status, _) = send(&fx.app, get_as("/v1/wallet/acct_player", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(&fx.app, get_as("/v1/wallet/acct_player", Some("acct_player.00"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn server_opens_player_accounts() {
    let fx = fixture(0);
    let (status, body) = send(
        &fx.app,
        server_post("/v1/players", json!({ "owner_ref": "flowblast:p1" }), SERVER_SECRET),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["data"]["account_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("acct_"));

    // A second call opens a distinct account (caller persists the first).
    let (_, again) = send(
        &fx.app,
        server_post("/v1/players", json!({ "owner_ref": "flowblast:p2" }), SERVER_SECRET),
    )
    .await;
    assert_ne!(again["data"]["account_id"], json!(id));
}

#[tokio::test]
async fn player_provisioning_requires_server_key_and_owner_ref() {
    let fx = fixture(0);
    let (status, _) = send(
        &fx.app,
        server_post("/v1/players", json!({ "owner_ref": "x" }), "wrong-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = send(
        &fx.app,
        server_post("/v1/players", json!({ "owner_ref": "  " }), SERVER_SECRET),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("bad_request"));
}
