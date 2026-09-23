//! Operator catalog editing: `/v1/admin/catalog*` (behind the admin guard).
//!
//! The live catalog is edited without a restart. Every edit is re-validated
//! as a whole catalog (prices, odds tables, pool coverage, identity rules),
//! persisted with an optimistic revision check, then swapped in atomically.
//! Products are archived rather than deleted so entitlements players already
//! own keep resolving.
//!
//! - `GET  /v1/admin/catalog`                          — full catalog incl. archived SKUs + revision.
//! - `PUT  /v1/admin/catalog/products/:sku`            — create or update a product.
//! - `POST /v1/admin/catalog/products/:sku/archive`    — archive / restore a product.
//! - `PUT  /v1/admin/catalog/pools/:rarity`            — replace a rarity's item pool.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use game_economy_core::{Catalog, Fulfiller, MintTransport, Product, Rarity};

use crate::{error, game_error, ok, AppState};

pub(crate) fn routes<M, F>() -> Router<AppState<M, F>>
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    Router::new()
        .route("/v1/admin/catalog", get(get_catalog::<M, F>))
        .route("/v1/admin/catalog/products/:sku", put(upsert_product::<M, F>))
        .route(
            "/v1/admin/catalog/products/:sku/archive",
            post(archive_product::<M, F>),
        )
        .route("/v1/admin/catalog/pools/:rarity", put(replace_pool::<M, F>))
}

/// JSON view of the whole catalog, archived SKUs included.
fn catalog_view(catalog: &Catalog, revision: u64) -> serde_json::Value {
    let products: Vec<_> = catalog
        .products()
        .map(|p| {
            let (kind, name) = match p {
                Product::CardPack(t) => ("card_pack", t.name.as_str()),
                Product::Cosmetic(c) => ("cosmetic", c.name.as_str()),
                Product::BattlePass(b) => ("battle_pass", b.name.as_str()),
            };
            json!({
                "sku": p.sku(),
                "kind": kind,
                "name": name,
                "price_micros": p.price_micros(),
                "archived": catalog.is_archived(p.sku()),
                "details": p,
            })
        })
        .collect();
    json!({
        "revision": revision,
        "products": products,
        "pools": catalog.pools(),
    })
}

/// `GET /v1/admin/catalog`
async fn get_catalog<M, F>(State(state): State<AppState<M, F>>) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let store = state.store();
    // Surface edits saved by other instances before showing the editor.
    if let Err(err) = store.refresh_catalog() {
        return game_error(&err);
    }
    ok(catalog_view(&store.catalog(), store.catalog_revision()))
}

#[derive(Deserialize)]
struct UpsertProductReq {
    /// Externally tagged product, e.g. `{"CardPack": {...}}`.
    product: Product,
    /// Revision the operator was editing; a stale one is rejected `409`.
    #[serde(default)]
    expected_revision: Option<u64>,
}

/// `PUT /v1/admin/catalog/products/:sku`
async fn upsert_product<M, F>(
    State(state): State<AppState<M, F>>,
    Path(sku): Path<String>,
    Json(req): Json<UpsertProductReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if req.product.sku() != sku {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the product sku must match the sku in the path",
        );
    }
    let product = req.product;
    tracing::info!(sku = %sku, "operator catalog upsert");
    match state
        .store()
        .edit_catalog(req.expected_revision, |c| c.with_upserted_product(product))
    {
        Ok((catalog, revision)) => ok(catalog_view(&catalog, revision)),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct ArchiveReq {
    archived: bool,
    #[serde(default)]
    expected_revision: Option<u64>,
}

/// `POST /v1/admin/catalog/products/:sku/archive`
async fn archive_product<M, F>(
    State(state): State<AppState<M, F>>,
    Path(sku): Path<String>,
    Json(req): Json<ArchiveReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    tracing::info!(sku = %sku, archived = req.archived, "operator catalog archive");
    match state
        .store()
        .edit_catalog(req.expected_revision, |c| c.with_archived(&sku, req.archived))
    {
        Ok((catalog, revision)) => ok(catalog_view(&catalog, revision)),
        Err(err) => game_error(&err),
    }
}

#[derive(Deserialize)]
struct PoolReq {
    items: Vec<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

/// Largest item pool an operator may set for one rarity.
const MAX_POOL_ITEMS: usize = 500;

/// `PUT /v1/admin/catalog/pools/:rarity` (`common` … `legendary`)
async fn replace_pool<M, F>(
    State(state): State<AppState<M, F>>,
    Path(rarity): Path<String>,
    Json(req): Json<PoolReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let Some(rarity) = parse_rarity(&rarity) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "rarity must be common, uncommon, rare, epic or legendary",
        );
    };
    if req.items.len() > MAX_POOL_ITEMS {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "a pool may hold at most 500 items",
        );
    }
    tracing::info!(rarity = rarity.tag(), items = req.items.len(), "operator pool edit");
    let items = req.items;
    match state
        .store()
        .edit_catalog(req.expected_revision, |c| c.with_pool_replaced(rarity, items))
    {
        Ok((catalog, revision)) => ok(catalog_view(&catalog, revision)),
        Err(err) => game_error(&err),
    }
}

fn parse_rarity(raw: &str) -> Option<Rarity> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "common" => Some(Rarity::Common),
        "uncommon" => Some(Rarity::Uncommon),
        "rare" => Some(Rarity::Rare),
        "epic" => Some(Rarity::Epic),
        "legendary" => Some(Rarity::Legendary),
        _ => None,
    }
}
