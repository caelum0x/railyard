//! Route tests for `game-economy-server`, driven over the real axum router with a
//! contract-faithful in-memory Mint (`FakeMint`). These lock in the money-path
//! HTTP contract the audit found untested: the JSON envelope, the auth guard that
//! binds a charge to the authenticated principal (401 no-token / 403 wrong-player),
//! the error→status mapping, idempotent replay, and — the load-bearing one — that
//! two concurrent purchases of a single-owner good yield EXACTLY ONE winner.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt; // oneshot

use game_economy_core::{Catalog, GameStore};
use game_economy_server::{auth, AppState};
use game_economy_testkit::{Call, FakeMint};

const PLAYER: &str = "acct_player";
const OTHER: &str = "acct_intruder";
const REVENUE: &str = "acct_revenue";
const SECRET: &[u8] = b"test-shared-secret";
const SERVER_SECRET: &[u8] = b"test-game-server-secret";

const SKIN_SKU: &str = "skin.aeon.blade"; // single-owner, $4.99
const SKIN_PRICE: i64 = 4_990_000;
const PACK_SKU: &str = "pack.standard"; // consumable, $0.99

/// A store + router bound to a FakeMint we retain a handle on for money assertions.
struct Fixture {
    app: Router,
    store: Arc<GameStore<FakeMint>>,
    key: [u8; 32],
}

fn fixture(player_balance: i64) -> Fixture {
    let mint = FakeMint::new().with_balance(PLAYER, player_balance);
    let store = Arc::new(GameStore::new(mint, Catalog::demo(), REVENUE));
    let key = auth::derive_key(SECRET);
    let app = game_economy_server::build_router(
        AppState::new(store.clone(), key).with_server_secret(SERVER_SECRET),
    );
    Fixture { app, store, key }
}

fn token_for(key: &[u8; 32], player: &str) -> String {
    auth::player_token(key, player)
}

/// A server-to-server request authenticated with the game-backend credential.
fn server_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(
            auth::SERVER_KEY_HEADER,
            std::str::from_utf8(SERVER_SECRET).unwrap(),
        )
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    builder.body(Body::empty()).unwrap()
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn is_capture(c: &Call) -> bool {
    matches!(c, Call::Capture { .. })
}

// --- happy path -----------------------------------------------------------

#[tokio::test]
async fn purchase_with_valid_token_succeeds_and_charges_once() {
    let fx = fixture(10_000_000);
    let token = token_for(&fx.key, PLAYER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&token),
            json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "buy-1" }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["data"]["charged_micros"], json!(SKIN_PRICE));
    assert_eq!(body["data"]["idempotent_replay"], json!(false));
    // Money moved exactly once, to revenue.
    assert_eq!(fx.store.mint().balance(REVENUE), SKIN_PRICE);
    assert_eq!(fx.store.mint().count_calls(is_capture), 1);
}

#[tokio::test]
async fn catalog_is_public_no_auth_required() {
    let fx = fixture(0);
    let resp = fx
        .app
        .clone()
        .oneshot(get("/v1/catalog", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["success"], json!(true));
    assert!(body["data"]["products"].as_array().unwrap().len() >= 3);
}

// --- auth / authz ---------------------------------------------------------

#[tokio::test]
async fn purchase_without_token_is_unauthorized_and_charges_nothing() {
    let fx = fixture(10_000_000);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            None,
            json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "no-auth" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], json!("unauthorized"));
    // Nothing was charged.
    assert_eq!(fx.store.mint().count_calls(is_capture), 0);
    assert_eq!(fx.store.mint().balance(REVENUE), 0);
}

#[tokio::test]
async fn cannot_charge_a_player_you_are_not() {
    // The unauthorized-charge vector: a token authenticating OTHER must not be
    // able to charge PLAYER's account.
    let fx = fixture(10_000_000);
    let intruder_token = token_for(&fx.key, OTHER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&intruder_token),
            json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "grief-1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], json!("forbidden"));
    // PLAYER was never touched.
    assert_eq!(fx.store.mint().count_calls(is_capture), 0);
    assert_eq!(fx.store.mint().balance(PLAYER), 10_000_000);
    assert_eq!(fx.store.mint().balance(REVENUE), 0);
}

#[tokio::test]
async fn forged_token_is_rejected() {
    let fx = fixture(10_000_000);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some("acct_player.deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "forge-1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fx.store.mint().count_calls(is_capture), 0);
}

#[tokio::test]
async fn inventory_is_scoped_to_the_authenticated_player() {
    let fx = fixture(10_000_000);
    let player_token = token_for(&fx.key, PLAYER);
    // Own inventory: OK.
    let ok = fx
        .app
        .clone()
        .oneshot(get(&format!("/v1/inventory/{PLAYER}"), Some(&player_token)))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    // Someone else's inventory with my token: forbidden.
    let denied = fx
        .app
        .clone()
        .oneshot(get(&format!("/v1/inventory/{OTHER}"), Some(&player_token)))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

// --- money-path error mapping --------------------------------------------

#[tokio::test]
async fn insufficient_funds_maps_to_402() {
    let fx = fixture(100_000); // < $0.99
    let token = token_for(&fx.key, PLAYER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&token),
            json!({ "player": PLAYER, "product_sku": PACK_SKU, "idempotency_key": "poor-1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], json!("insufficient_funds"));
    assert_eq!(fx.store.mint().count_calls(is_capture), 0);
}

#[tokio::test]
async fn unknown_product_maps_to_404() {
    let fx = fixture(10_000_000);
    let token = token_for(&fx.key, PLAYER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&token),
            json!({ "player": PLAYER, "product_sku": "no.such.sku", "idempotency_key": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(resp).await["error"]["code"],
        json!("unknown_product")
    );
}

#[tokio::test]
async fn missing_fields_map_to_400() {
    let fx = fixture(10_000_000);
    let token = token_for(&fx.key, PLAYER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&token),
            json!({ "player": PLAYER, "product_sku": "", "idempotency_key": "y" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(resp).await["error"]["code"], json!("bad_request"));
}

// --- idempotency ----------------------------------------------------------

#[tokio::test]
async fn idempotent_replay_returns_same_result_and_charges_once() {
    let fx = fixture(10_000_000);
    let token = token_for(&fx.key, PLAYER);
    let body = json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "replay-1" });

    let first = fx
        .app
        .clone()
        .oneshot(post("/v1/purchase", Some(&token), body.clone()))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = json_body(first).await;

    let second = fx
        .app
        .clone()
        .oneshot(post("/v1/purchase", Some(&token), body))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second = json_body(second).await;

    assert_eq!(second["data"]["idempotent_replay"], json!(true));
    assert_eq!(
        first["data"]["entitlement_id"],
        second["data"]["entitlement_id"]
    );
    assert_eq!(first["data"]["receipt_id"], second["data"]["receipt_id"]);
    // Captured exactly once despite two calls.
    assert_eq!(fx.store.mint().count_calls(is_capture), 1);
    assert_eq!(fx.store.mint().balance(REVENUE), SKIN_PRICE);
}

// --- battle-pass progression ---------------------------------------------

const PASS_SKU: &str = "pass.season1"; // single-owner, $9.99

#[tokio::test]
async fn battle_pass_advances_for_its_owner_and_moves_no_money() {
    let fx = fixture(20_000_000);
    let token = token_for(&fx.key, PLAYER);

    // Buy the pass first.
    let bought = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/purchase",
            Some(&token),
            json!({ "player": PLAYER, "product_sku": PASS_SKU, "idempotency_key": "buy-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(bought.status(), StatusCode::OK);
    let revenue_after_buy = fx.store.mint().balance(REVENUE);

    // Advance to tier 25.
    let advanced = fx
        .app
        .clone()
        .oneshot(server_post(
            "/v1/pass/advance",
            json!({ "player": PLAYER, "pass_sku": PASS_SKU, "tier": 25 }),
        ))
        .await
        .unwrap();
    assert_eq!(advanced.status(), StatusCode::OK);
    let body = json_body(advanced).await;
    assert_eq!(body["data"]["tier"], json!(25));

    // Progression moved no money — the pass was charged once, at purchase.
    assert_eq!(fx.store.mint().balance(REVENUE), revenue_after_buy);
    assert_eq!(fx.store.inventory(PLAYER).battle_pass.unwrap().tier, 25);
}

#[tokio::test]
async fn advancing_an_unowned_pass_maps_to_404() {
    let fx = fixture(20_000_000);
    let resp = fx
        .app
        .clone()
        .oneshot(server_post(
            "/v1/pass/advance",
            json!({ "player": PLAYER, "pass_sku": PASS_SKU, "tier": 5 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(resp).await["error"]["code"],
        json!("battle_pass_not_owned")
    );
}

#[tokio::test]
async fn players_cannot_advance_any_pass_even_their_own() {
    let fx = fixture(20_000_000);
    // A PLAYER bearer token (even the owner's) is never enough: progression is
    // reported only by the game backend's server credential.
    let own = token_for(&fx.key, PLAYER);
    let resp = fx
        .app
        .clone()
        .oneshot(post(
            "/v1/pass/advance",
            Some(&own),
            json!({ "player": PLAYER, "pass_sku": PASS_SKU, "tier": 100 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- the concurrency guard: exactly one winner ---------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_single_owner_claims_yield_exactly_one_winner() {
    let fx = fixture(100_000_000);
    let token = token_for(&fx.key, PLAYER);
    let app = fx.app.clone();

    // Two DISTINCT idempotency keys for the SAME single-owner cosmetic, fired
    // concurrently. Without the atomic reserve both would pass the read-then-act
    // ownership guard and double-charge; with it, exactly one wins.
    let a = {
        let (app, token) = (app.clone(), token.clone());
        tokio::spawn(async move {
            app.oneshot(post(
                "/v1/purchase",
                Some(&token),
                json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "race-A" }),
            ))
            .await
            .unwrap()
            .status()
        })
    };
    let b = {
        let (app, token) = (app.clone(), token.clone());
        tokio::spawn(async move {
            app.oneshot(post(
                "/v1/purchase",
                Some(&token),
                json!({ "player": PLAYER, "product_sku": SKIN_SKU, "idempotency_key": "race-B" }),
            ))
            .await
            .unwrap()
            .status()
        })
    };

    let (sa, sb) = (a.await.unwrap(), b.await.unwrap());
    let mut statuses = [sa, sb];
    statuses.sort_by_key(|s| s.as_u16());
    // Exactly one 200 OK and one 409 Conflict (already_owned).
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "expected exactly one winner + one already_owned, got {statuses:?}"
    );

    // The good was charged EXACTLY ONCE — no double charge.
    assert_eq!(fx.store.mint().count_calls(is_capture), 1);
    assert_eq!(fx.store.mint().balance(REVENUE), SKIN_PRICE);
    // And the player owns exactly one copy.
    assert_eq!(fx.store.inventory(PLAYER).cosmetics.len(), 1);
}
