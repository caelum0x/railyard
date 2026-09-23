//! Operator console: `GET /admin` (static page) plus the `/v1/admin/*` JSON API
//! it drives. Every API route sits behind [`crate::auth::require_admin`]
//! (`Authorization: Bearer <GAME_ADMIN_TOKEN>`).
//!
//! Operators can see store-wide revenue / refund totals, browse recent
//! purchases across players, look up one player's inventory and receipts,
//! refund a purchase through Mint, revoke a charged-back purchase, and advance
//! a player's battle pass.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use game_economy_core::{Fulfiller, MintTransport, Product};

use crate::{
    error, game_error, ok, purchase_view, refund_response, revoke_response, AppState, ReverseReq,
};

/// Largest page the purchase list returns.
const MAX_LIST_LIMIT: usize = 500;
/// Default page size for the purchase list.
const DEFAULT_LIST_LIMIT: usize = 100;

pub(crate) async fn page() -> Html<&'static str> {
    Html(include_str!("../assets/admin.html"))
}

pub(crate) fn routes<M, F>() -> Router<AppState<M, F>>
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    Router::new()
        .route("/v1/admin/summary", get(summary::<M, F>))
        .route("/v1/admin/purchases", get(purchases::<M, F>))
        .route("/v1/admin/players/:player", get(player::<M, F>))
        .route("/v1/admin/refunds", post(refund::<M, F>))
        .route("/v1/admin/revoke", post(revoke::<M, F>))
        .route("/v1/admin/pass/advance", post(advance_pass::<M, F>))
        .merge(crate::admin_catalog::routes::<M, F>())
}

/// `GET /v1/admin/summary` — revenue, refunds and catalog totals.
async fn summary<M, F>(State(state): State<AppState<M, F>>) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let store = state.store();
    let catalog = store.catalog();
    let (mut packs, mut cosmetics, mut passes) = (0u32, 0u32, 0u32);
    for product in catalog.products() {
        match product {
            Product::CardPack(_) => packs += 1,
            Product::Cosmetic(_) => cosmetics += 1,
            Product::BattlePass(_) => passes += 1,
        }
    }
    match store.summary() {
        Ok(totals) => ok(json!({
            "totals": totals,
            "revenue_account": store.revenue_account(),
            "catalog": {
                "products": catalog.len(),
                "archived": catalog.archived().len(),
                "revision": store.catalog_revision(),
                "card_packs": packs,
                "cosmetics": cosmetics,
                "battle_passes": passes,
            },
        })),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// Restrict to one player's purchases.
    #[serde(default)]
    player: Option<String>,
    /// `completed`, `refunded` or `revoked`.
    #[serde(default)]
    status: Option<String>,
}

/// `GET /v1/admin/purchases?limit=&player=&status=` — newest purchases first.
async fn purchases<M, F>(
    State(state): State<AppState<M, F>>,
    Query(query): Query<ListQuery>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let limit = query
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let status = query.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = status {
        if !matches!(s, "completed" | "refunded" | "revoked") {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "status must be completed, refunded or revoked",
            );
        }
    }
    let player = query.player.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let listed = match player {
        Some(player) => state.store().purchase_history(player),
        // Over-fetch when filtering by status so a page is still useful.
        None => state
            .store()
            .recent_purchases(if status.is_some() { MAX_LIST_LIMIT } else { limit }),
    };
    match listed {
        Ok(list) => {
            let items: Vec<_> = list
                .iter()
                .filter(|p| status.map_or(true, |s| p.status() == s))
                .take(limit)
                .map(purchase_view)
                .collect();
            ok(json!({ "purchases": items }))
        }
        Err(err) => game_error(&err),
    }
}

/// `GET /v1/admin/players/:player` — one player's inventory and receipts.
async fn player<M, F>(
    State(state): State<AppState<M, F>>,
    Path(player): Path<String>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let player = player.trim().to_string();
    if player.is_empty() {
        return error(StatusCode::BAD_REQUEST, "bad_request", "player is required");
    }
    let inventory = match state.store().try_inventory(&player) {
        Ok(inventory) => inventory,
        Err(err) => return game_error(&err),
    };
    match state.store().purchase_history(&player) {
        Ok(history) => ok(json!({
            "player": player,
            "inventory": inventory,
            "purchases": history.iter().map(purchase_view).collect::<Vec<_>>(),
        })),
        Err(err) => game_error(&err),
    }
}

/// `POST /v1/admin/refunds` — refund through Mint and remove the grant.
async fn refund<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<ReverseReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    tracing::info!(player = %req.player, receipt_id = %req.receipt_id, "operator refund");
    refund_response(&state, &req).await
}

/// `POST /v1/admin/revoke` — remove the grant without moving money (chargeback).
async fn revoke<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<ReverseReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    tracing::info!(player = %req.player, receipt_id = %req.receipt_id, "operator revoke");
    revoke_response(&state, &req)
}

#[derive(Deserialize)]
struct AdvanceReq {
    player: String,
    pass_sku: String,
    tier: u32,
}

/// `POST /v1/admin/pass/advance` — set a player's earned battle-pass tier
/// (monotonic, clamped to the season's tier count).
async fn advance_pass<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<AdvanceReq>,
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
        .advance_battle_pass(req.player.trim(), req.pass_sku.trim(), req.tier)
    {
        Ok(tier) => ok(json!({ "player": req.player.trim(), "pass_sku": req.pass_sku, "tier": tier })),
        Err(err) => game_error(&err),
    }
}
