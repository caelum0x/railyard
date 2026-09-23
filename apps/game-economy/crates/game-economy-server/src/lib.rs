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

use game_economy_core::{
    AlwaysFulfill, CompletedPurchase, Fulfiller, GameError, GameStore, MintTransport, Product,
    ReverseOutcome,
};

mod admin;
pub mod auth;
pub mod fulfill;
mod storefront;

use auth::Principal;

/// Shared handler state: the store plus the auth signing key. Cheap to clone (the
/// store is behind an `Arc`, the key is `Copy`).
pub struct AppState<M: MintTransport, F: Fulfiller = AlwaysFulfill> {
    store: Arc<GameStore<M, F>>,
    auth_key: [u8; 32],
    /// Key pack-opening seeds are derived under (never client-supplied).
    seed_key: [u8; 32],
    /// Hashed game-backend credential for server-to-server routes.
    server_key: Option<blake3::Hash>,
    /// Whether legacy non-expiring player tokens are still accepted.
    allow_legacy_tokens: bool,
    /// Default lifetime of tokens issued by `POST /v1/tokens`.
    token_ttl_secs: u64,
    /// Hashed operator console token (`GAME_ADMIN_TOKEN`).
    admin_key: Option<blake3::Hash>,
}

impl<M: MintTransport, F: Fulfiller> Clone for AppState<M, F> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            auth_key: self.auth_key,
            seed_key: self.seed_key,
            server_key: self.server_key,
            allow_legacy_tokens: self.allow_legacy_tokens,
            token_ttl_secs: self.token_ttl_secs,
            admin_key: self.admin_key,
        }
    }
}

/// Default issued-token lifetime: 24 hours.
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
/// Longest lifetime a caller may request for an issued token: 30 days.
pub const MAX_TOKEN_TTL_SECS: u64 = 30 * 24 * 60 * 60;

impl<M: MintTransport + 'static, F: Fulfiller + 'static> AppState<M, F> {
    /// Build handler state from a store and the 32-byte auth key
    /// (see [`auth::derive_key`]). Server-to-server routes stay disabled until
    /// [`AppState::with_server_secret`] is applied.
    pub fn new(store: Arc<GameStore<M, F>>, auth_key: [u8; 32]) -> Self {
        Self {
            store,
            auth_key,
            seed_key: auth::pack_seed_key(&auth_key),
            server_key: None,
            allow_legacy_tokens: true,
            token_ttl_secs: DEFAULT_TOKEN_TTL_SECS,
            admin_key: None,
        }
    }

    /// Enable the operator console API (`/v1/admin/*`), authenticated by
    /// `Authorization: Bearer <token>`.
    pub fn with_admin_token(mut self, token: &[u8]) -> Self {
        self.admin_key = Some(auth::admin_credential(token));
        self
    }

    pub(crate) fn admin_key(&self) -> Option<&blake3::Hash> {
        self.admin_key.as_ref()
    }

    /// Enable the server-to-server routes, authenticated by `secret`.
    pub fn with_server_secret(mut self, secret: &[u8]) -> Self {
        self.server_key = Some(auth::server_credential(secret));
        self
    }

    /// Accept (or refuse) legacy non-expiring player tokens.
    pub fn with_legacy_tokens(mut self, allow: bool) -> Self {
        self.allow_legacy_tokens = allow;
        self
    }

    /// Default lifetime for issued tokens (clamped to `1..=MAX_TOKEN_TTL_SECS`).
    pub fn with_token_ttl(mut self, secs: u64) -> Self {
        self.token_ttl_secs = secs.clamp(1, MAX_TOKEN_TTL_SECS);
        self
    }

    pub(crate) fn server_key(&self) -> Option<&blake3::Hash> {
        self.server_key.as_ref()
    }

    pub(crate) fn allow_legacy_tokens(&self) -> bool {
        self.allow_legacy_tokens
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
        .route("/store", get(storefront::page))
        .route("/admin", get(admin::page))
        .route("/v1/catalog", get(catalog_handler::<M, F>));

    let guarded = Router::new()
        .route("/v1/purchase", post(purchase_handler::<M, F>))
        .route("/v1/packs/open", post(open_pack_handler::<M, F>))
        .route("/v1/inventory/:player", get(inventory_handler::<M, F>))
        .route("/v1/purchases/:player", get(history_handler::<M, F>))
        .route(
            "/v1/purchases/:player/:receipt_id",
            get(receipt_handler::<M, F>),
        )
        // `route_layer` runs the guard only when one of these routes matches.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth::<M, F>,
        ));

    // Server-to-server: only the game backend (GAME_SERVER_SECRET) may report
    // progression, mint player tokens, or revoke entitlements. A player bearer
    // token is never sufficient here.
    let server = Router::new()
        .route("/v1/pass/advance", post(advance_pass_handler::<M, F>))
        .route("/v1/tokens", post(issue_token_handler::<M, F>))
        .route(
            "/v1/entitlements/revoke",
            post(revoke_handler::<M, F>),
        )
        .route("/v1/refunds", post(refund_handler::<M, F>))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_server::<M, F>,
        ));

    // Operator console API (GAME_ADMIN_TOKEN).
    let admin = admin::routes::<M, F>().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_admin::<M, F>,
    ));

    public
        .merge(guarded)
        .merge(server)
        .merge(admin)
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
            kind: match p {
                Product::CardPack(_) => "card_pack",
                Product::Cosmetic(_) => "cosmetic",
                Product::BattlePass(_) => "battle_pass",
            },
            name: match p {
                Product::CardPack(t) => t.name.clone(),
                Product::Cosmetic(c) => c.name.clone(),
                Product::BattlePass(b) => b.name.clone(),
            },
            details: p.clone(),
        })
        .collect();
    ok(json!({ "products": items, "pools": state.store().catalog().pools() }))
}

#[derive(Serialize)]
struct CatalogItem {
    sku: String,
    price_micros: i64,
    kind: &'static str,
    name: String,
    /// Full product definition (odds table, slot, tier count).
    details: Product,
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
#[serde(deny_unknown_fields)]
struct OpenPackReq {
    player: String,
    entitlement_id: String,
}

/// `POST /v1/packs/open` — open an owned pack. Bound to the authenticated
/// principal (a player may only open their own packs). The roll seed is
/// server-committed ([`auth::pack_seed`], keyed by the server secret) — a
/// client-supplied `seed` is rejected — so drops cannot be ground offline. The
/// seed is revealed in the response so the opening stays auditable against the
/// pack's receipt id.
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
    let seed = auth::pack_seed(&state.seed_key, &req.player, &req.entitlement_id);
    match state
        .store()
        .open_pack(&req.player, &req.entitlement_id, seed)
    {
        Ok(items) => ok(json!({ "items": items, "seed": seed.to_string() })),
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

/// `POST /v1/pass/advance` — SERVER-TO-SERVER: the game backend reports the
/// tier `player` has earned through play (monotonic + clamped to the season's
/// tier count). Guarded by [`auth::require_server`], so players cannot grant
/// themselves tiers. Moves no money; the pass itself is bought via
/// `/v1/purchase`.
async fn advance_pass_handler<M, F>(
    State(state): State<AppState<M, F>>,
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

/// `GET /v1/purchases/:player` — the authenticated player's receipt history.
async fn history_handler<M, F>(
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
    match state.store().purchase_history(&player) {
        Ok(purchases) => {
            let items: Vec<_> = purchases.iter().map(purchase_view).collect();
            ok(json!({ "purchases": items }))
        }
        Err(err) => game_error(&err),
    }
}

/// `GET /v1/purchases/:player/:receipt_id` — one signed Mint receipt.
async fn receipt_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Path((player, receipt_id)): Path<(String, String)>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if player != principal.0 {
        return forbidden();
    }
    match state.store().receipt(&player, &receipt_id) {
        Ok(Some(p)) => ok(purchase_view(&p)),
        Ok(None) => error(StatusCode::NOT_FOUND, "receipt_not_found", "no such receipt"),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct IssueTokenReq {
    player: String,
    #[serde(default)]
    ttl_seconds: Option<u64>,
}

/// `POST /v1/tokens` — SERVER-TO-SERVER: the game backend, after authenticating
/// its own player, mints an expiring storefront bearer token for them.
async fn issue_token_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<IssueTokenReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let player = req.player.trim();
    if player.is_empty() || player.len() > 256 || player.chars().any(char::is_whitespace) {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "player must be a non-empty account id without whitespace",
        );
    }
    let ttl = req
        .ttl_seconds
        .unwrap_or(state.token_ttl_secs)
        .clamp(1, MAX_TOKEN_TTL_SECS);
    let expires_at = auth::now_unix().saturating_add(ttl);
    let token = auth::player_token_expiring(state.auth_key(), player, expires_at);
    ok(json!({ "player": player, "token": token, "expires_at": expires_at }))
}

#[derive(Deserialize)]
pub(crate) struct ReverseReq {
    pub(crate) player: String,
    pub(crate) receipt_id: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

impl ReverseReq {
    fn validate(&self) -> Result<(), Response> {
        if self.player.trim().is_empty() || self.receipt_id.trim().is_empty() {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "player and receipt_id are required",
            ));
        }
        if self.reason.as_deref().is_some_and(|r| r.len() > 500) {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "reason must be at most 500 characters",
            ));
        }
        Ok(())
    }

    fn reason(&self) -> Option<&str> {
        self.reason.as_deref().map(str::trim).filter(|r| !r.is_empty())
    }
}

/// `POST /v1/entitlements/revoke` — SERVER-TO-SERVER: remove what a purchase
/// granted WITHOUT moving money (card chargeback: the payment was already
/// reversed outside Mint). The purchase is marked `revoked` and can no longer
/// be refunded. To return money through Mint use `POST /v1/refunds`.
async fn revoke_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<ReverseReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    revoke_response(&state, &req)
}

pub(crate) fn revoke_response<M, F>(state: &AppState<M, F>, req: &ReverseReq) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if let Err(response) = req.validate() {
        return response;
    }
    match state
        .store()
        .revoke_entitlement(&req.player, &req.receipt_id, req.reason())
    {
        Ok(ReverseOutcome::Reversed(p)) => ok(json!({
            "revoked": true,
            "already_reversed": false,
            "purchase": purchase_view(&p),
        })),
        Ok(ReverseOutcome::AlreadyReversed(p)) => ok(json!({
            "revoked": false,
            "already_reversed": true,
            "purchase": purchase_view(&p),
        })),
        Ok(ReverseOutcome::NotFound) => {
            error(StatusCode::NOT_FOUND, "receipt_not_found", "no such receipt")
        }
        Err(err) => game_error(&err),
    }
}

/// `POST /v1/refunds` — SERVER-TO-SERVER: refund a purchase through Mint (the
/// captured amount moves from the revenue account back to the player) and
/// remove what it granted. Idempotent per receipt: a repeated call returns the
/// original refund and never pays out twice.
async fn refund_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<ReverseReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    refund_response(&state, &req).await
}

pub(crate) async fn refund_response<M, F>(state: &AppState<M, F>, req: &ReverseReq) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if let Err(response) = req.validate() {
        return response;
    }
    match state
        .store()
        .refund(&req.player, &req.receipt_id, req.reason())
        .await
    {
        Ok(outcome) => ok(json!({
            "refunded": true,
            "idempotent_replay": outcome.idempotent_replay,
            "purchase": purchase_view(&outcome.purchase),
        })),
        Err(err) => game_error(&err),
    }
}

/// JSON view of a completed purchase: the Mint outcome plus its `status`
/// (`completed` / `refunded` / `revoked`) and reversal record.
pub(crate) fn purchase_view(purchase: &CompletedPurchase) -> serde_json::Value {
    let mut value = serde_json::to_value(&purchase.outcome).unwrap_or_else(|_| json!({}));
    if let Some(map) = value.as_object_mut() {
        map.insert("player".into(), json!(purchase.player));
        map.insert("status".into(), json!(purchase.status()));
        map.insert(
            "reversal".into(),
            serde_json::to_value(&purchase.reversal).unwrap_or(serde_json::Value::Null),
        );
    }
    value
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
        GameError::PurchaseNotFound(_) => (StatusCode::NOT_FOUND, "receipt_not_found"),
        GameError::AlreadyReversed(_) => (StatusCode::CONFLICT, "already_reversed"),
        GameError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),
    };
    // Log the detailed error server-side; return a client-safe message.
    tracing::warn!(error = %err, code, "purchase-path error");
    error(status, code, &err.to_string())
}
