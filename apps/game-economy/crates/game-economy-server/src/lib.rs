//! `game-economy-server` library: the HTTP surface over [`GameStore`], factored
//! out of the binary so the routes are testable with an injected FakeMint (the
//! binary in `main.rs` wires the real `HttpMintTransport`).
//!
//! Generic over the Mint transport `M` and fulfiller `F` — the SAME
//! router/handlers/auth run over `GameStore<HttpMintTransport>` in production and
//! `GameStore<FakeMint>` in tests.
//!
//! - [`auth`] — the BLAKE3-keyed bearer-token guard that binds each money-path
//!   request to the player it authenticates (closes the unauthorized-charge hole).
//! - [`build_router`] — wires every route; the money-path routes sit behind
//!   [`auth::require_auth`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use game_economy_core::{AlwaysFulfill, Fulfiller, GameError, GameStore, MintTransport};

pub mod auth;

use auth::Principal;

/// Shared handler state: the store plus the auth signing key. Cheap to clone (the
/// store is behind an `Arc`, the key is `Copy`).
pub struct AppState<M: MintTransport, F: Fulfiller = AlwaysFulfill> {
    store: Arc<GameStore<M, F>>,
    auth_key: [u8; 32],
}

impl<M: MintTransport, F: Fulfiller> Clone for AppState<M, F> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            auth_key: self.auth_key,
        }
    }
}

impl<M: MintTransport + 'static, F: Fulfiller + 'static> AppState<M, F> {
    /// Build handler state from a store and the 32-byte auth key
    /// (see [`auth::derive_key`]).
    pub fn new(store: Arc<GameStore<M, F>>, auth_key: [u8; 32]) -> Self {
        Self { store, auth_key }
    }

    pub(crate) fn store(&self) -> &GameStore<M, F> {
        &self.store
    }

    pub(crate) fn auth_key(&self) -> &[u8; 32] {
        &self.auth_key
    }
}

/// Wire the routes. `/healthz` and `/v1/catalog` are public (liveness + product
/// discovery); the money-path routes require a valid bearer token via
/// [`auth::require_auth`], which also binds the request to its principal.
pub fn build_router<M, F>(state: AppState<M, F>) -> Router
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/catalog", get(catalog_handler::<M, F>));

    let guarded = Router::new()
        .route("/v1/purchase", post(purchase_handler::<M, F>))
        .route("/v1/packs/open", post(open_pack_handler::<M, F>))
        .route("/v1/pass/advance", post(advance_pass_handler::<M, F>))
        .route("/v1/inventory/:player", get(inventory_handler::<M, F>))
        // `route_layer` runs the guard only when one of these routes matches.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth::<M, F>,
        ));

    public
        .merge(guarded)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// `GET /v1/catalog` — list every product's SKU and µ$ price.
async fn catalog_handler<M, F>(State(state): State<AppState<M, F>>) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let items: Vec<CatalogItem> = state
        .store()
        .catalog()
        .products()
        .map(|p| CatalogItem {
            sku: p.sku().to_string(),
            price_micros: p.price_micros(),
        })
        .collect();
    ok(json!({ "products": items }))
}

#[derive(Serialize)]
struct CatalogItem {
    sku: String,
    price_micros: i64,
}

#[derive(Deserialize)]
struct PurchaseReq {
    /// The buyer's Mint account id (the hold is opened against it). MUST equal the
    /// authenticated principal, or the request is rejected `403`.
    player: String,
    /// The SKU to buy.
    product_sku: String,
    /// Caller-supplied idempotency key: replays return the original outcome.
    idempotency_key: String,
}

/// `POST /v1/purchase` — the money path: hold → capture → receipt → grant. Only
/// ever charges the AUTHENTICATED principal; a `player` that is not the token's
/// principal is `403` (this is the fix for the unauthorized-charge vector).
async fn purchase_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<PurchaseReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if req.player.trim().is_empty()
        || req.product_sku.trim().is_empty()
        || req.idempotency_key.trim().is_empty()
    {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "player, product_sku and idempotency_key are required",
        );
    }
    if req.player != principal.0 {
        return forbidden();
    }
    match state
        .store()
        .purchase(&req.player, &req.product_sku, &req.idempotency_key)
        .await
    {
        Ok(outcome) => ok(serde_json::to_value(outcome).unwrap_or_else(|_| json!({}))),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct OpenPackReq {
    player: String,
    entitlement_id: String,
    #[serde(default)]
    seed: u64,
}

/// `POST /v1/packs/open` — deterministically open an owned pack. Bound to the
/// authenticated principal (a player may only open their own packs).
async fn open_pack_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<OpenPackReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if req.player.trim().is_empty() || req.entitlement_id.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "player and entitlement_id are required",
        );
    }
    if req.player != principal.0 {
        return forbidden();
    }
    match state
        .store()
        .open_pack(&req.player, &req.entitlement_id, req.seed)
    {
        Ok(items) => ok(json!({ "items": items })),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct AdvancePassReq {
    player: String,
    pass_sku: String,
    /// The tier the player has earned; clamped to `[current, season tiers]`.
    tier: u32,
}

/// `POST /v1/pass/advance` — advance the authenticated player's owned battle-pass
/// to `tier` (monotonic + clamped to the season's tier count). Moves no money;
/// the pass itself is bought via `/v1/purchase`.
async fn advance_pass_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<AdvancePassReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if req.player.trim().is_empty() || req.pass_sku.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "player and pass_sku are required",
        );
    }
    if req.player != principal.0 {
        return forbidden();
    }
    match state
        .store()
        .advance_battle_pass(&req.player, &req.pass_sku, req.tier)
    {
        Ok(tier) => ok(json!({ "pass_sku": req.pass_sku, "tier": tier })),
        Err(err) => game_error(&err),
    }
}

/// `GET /v1/inventory/:player` — a player's granted entitlements. A player may
/// only read their own inventory.
async fn inventory_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Path(player): Path<String>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if player != principal.0 {
        return forbidden();
    }
    match state.store().try_inventory(&player) {
        Ok(inventory) => ok(serde_json::to_value(inventory).unwrap_or_else(|_| json!({}))),
        Err(err) => game_error(&err),
    }
}

pub(crate) fn ok(data: serde_json::Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "success": true, "data": data })),
    )
        .into_response()
}

pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "success": false, "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// `401` for a missing/invalid bearer token.
pub(crate) fn unauthorized() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "missing or invalid bearer token",
    )
}

/// `403` when an authenticated caller tries to act for a different player.
pub(crate) fn forbidden() -> Response {
    error(
        StatusCode::FORBIDDEN,
        "forbidden",
        "authenticated principal may not act for another player",
    )
}

/// Map a [`GameError`] to a stable HTTP status + code, mirroring the money-path
/// error taxonomy Mint and Abacus use.
pub(crate) fn game_error(err: &GameError) -> Response {
    let (status, code) = match err {
        GameError::UnknownProduct(_) => (StatusCode::NOT_FOUND, "unknown_product"),
        GameError::InsufficientFunds => (StatusCode::PAYMENT_REQUIRED, "insufficient_funds"),
        GameError::HoldNotFound => (StatusCode::NOT_FOUND, "hold_not_found"),
        GameError::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        GameError::CaptureExceedsHold => (StatusCode::UNPROCESSABLE_ENTITY, "capture_exceeds_hold"),
        GameError::AlreadyOwned(_) => (StatusCode::CONFLICT, "already_owned"),
        GameError::BattlePassNotOwned(_) => (StatusCode::NOT_FOUND, "battle_pass_not_owned"),
        GameError::PackNotOwned(_) => (StatusCode::NOT_FOUND, "pack_not_owned"),
        GameError::PackAlreadyOpened(_) => (StatusCode::CONFLICT, "pack_already_opened"),
        GameError::FulfillmentFailed(_) => (StatusCode::BAD_GATEWAY, "fulfillment_failed"),
        GameError::Mint(_) => (StatusCode::BAD_GATEWAY, "mint_error"),
        GameError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),
    };
    // Log the detailed error server-side; return a client-safe message.
    tracing::warn!(error = %err, code, "purchase-path error");
    error(status, code, &err.to_string())
}
