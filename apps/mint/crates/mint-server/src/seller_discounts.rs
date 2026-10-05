//! Seller discount codes (Wave Q, for Menivor QM1.3 creator coupons), made
//! and changed on the seller's **own** Dodo account with the seller's key,
//! which stays in Mint.
//!
//! - `POST /mint/v1/sellers/{seller}/discounts` `{code, type, amount,
//!   expires_at?, name?, currency_options? (or currency), restricted_to?}`
//!   (Dodo's `POST /discounts` shape; `restricted_to` holds the seller's
//!   skus) → `201 {discount:{discount_id, id, code, type, amount, currency,
//!   expires_at, restricted_to}}`. A retry of the same code with the same
//!   terms answers `200` with the discount already made (`created: false`);
//!   the same code with other terms is `409 code_in_use`.
//! - `PATCH /mint/v1/sellers/{seller}/discounts/{discount_id}`: a partial
//!   Dodo `PATCH /discounts/{discount_id}` (any of `amount` with `type`,
//!   `code`, `currency_options` or `currency`, `expires_at`, `name`,
//!   `restricted_to`) → `200 {discount:{discount_id, id, code, type, amount,
//!   expires_at}}`. Stopping a code is `{"expires_at": <now>}`; switching it
//!   back on sends a later `expires_at` (or `null` to clear it) with the new
//!   terms, on the same id. The same patch sent twice leaves the same
//!   discount, so a retry is safe.
//!
//! Only which seller made which processor discount id is stored, so a seller
//! can only change its own discounts: any other id is `404 unknown_discount`
//! (also when the code was deleted on the processor's dashboard). Refusals
//! use the seller error shape: `400 invalid_*`, `401`, `404 unknown_sku`,
//! `409 not_connected` / `discounts_unsupported` (Stripe-connected sellers),
//! `422` when the processor refuses (a code in use).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mint_payments::seller::{self, dodo, DiscountKind, DiscountPatch, DiscountSpec, DiscountState, SellerAccount, SellerError, SellerProvider};
use mint_store::{SellerDiscountRecord, SellerRecord};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::seller::{caller, open_account, problem, seller_error, storage, ProductKey, SellerState};

const UNSUPPORTED: &str = "discount codes work on Dodo payment accounts; this seller connected Stripe";

#[derive(Deserialize)]
pub(crate) struct CurrencyOption {
    currency: String,
    max_amount_possible: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct DiscountBody {
    code: String,
    #[serde(rename = "type")]
    kind: String,
    amount: i64,
    expires_at: Option<String>,
    name: Option<String>,
    currency: Option<String>,
    currency_options: Option<Vec<CurrencyOption>>,
    /// The seller's skus (Mint maps them to the processor's product ids).
    restricted_to: Option<Vec<String>>,
}

/// `Some(None)` for an explicit `null`, `None` (via `default`) when absent.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    code: Option<String>,
    name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    amount: Option<i64>,
    currency: Option<String>,
    currency_options: Option<Vec<CurrencyOption>>,
    #[serde(default, deserialize_with = "present")]
    expires_at: Option<Option<String>>,
    restricted_to: Option<Vec<String>>,
}

fn parse_kind(value: &str) -> Option<DiscountKind> {
    match value {
        "percentage" => Some(DiscountKind::Percentage),
        "flat" => Some(DiscountKind::Flat),
        _ => None,
    }
}

const fn kind_name(kind: DiscountKind) -> &'static str {
    match kind {
        DiscountKind::Percentage => "percentage",
        DiscountKind::Flat => "flat",
    }
}

/// The one currency of a flat amount, from `currency` or the single
/// `currency_options` row (whose `max_amount_possible`, when sent, must be
/// the amount itself: Mint makes one default row per flat code).
fn one_currency(currency: Option<&str>, options: Option<&[CurrencyOption]>, amount: Option<i64>) -> Result<Option<String>, &'static str> {
    let rows = options.unwrap_or_default();
    if rows.len() > 1 {
        return Err("send one currency row: the flat amount's currency");
    }
    if rows.first().is_some_and(|row| row.max_amount_possible.is_some_and(|m| Some(m) != amount)) {
        return Err("max_amount_possible is the flat amount itself");
    }
    Ok(currency.map(str::to_owned).or_else(|| rows.first().map(|r| r.currency.clone())).map(|c| c.to_ascii_uppercase()))
}

/// The seller's processor product ids for `skus` (the seller's own skus only).
async fn product_ids(state: &SellerState, key: &ProductKey, seller_id: &str, skus: &[String]) -> Result<Vec<String>, Response> {
    if skus.len() > 50 || !skus.iter().all(|s| seller::valid_sku(s)) {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_restricted_to", "restricted_to lists up to 50 of the seller's skus"));
    }
    let mut ids = Vec::with_capacity(skus.len());
    for sku in skus {
        match state.store.get_product(&key.product, seller_id, sku).await {
            Ok(Some(p)) => ids.push(p.provider_ref),
            Ok(None) => return Err(problem(StatusCode::NOT_FOUND, "unknown_sku", "create the product first")),
            Err(_) => return Err(storage()),
        }
    }
    Ok(ids)
}

/// The seller's connected Dodo account, opened for one call.
async fn dodo_account(state: &SellerState, key: &ProductKey, seller_id: &str) -> Result<(SellerRecord, SellerAccount), Response> {
    let record = match state.store.get_seller(&key.product, seller_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(seller_error(SellerError::NotConnected)),
        Err(_) => return Err(storage()),
    };
    let account = open_account(state, &record).map_err(seller_error)?;
    if account.provider != SellerProvider::Dodo {
        return Err(problem(StatusCode::CONFLICT, "discounts_unsupported", UNSUPPORTED));
    }
    Ok((record, account))
}

fn unknown_discount() -> Response {
    problem(StatusCode::NOT_FOUND, "unknown_discount", "this seller has no such discount")
}

fn state_json(d: &DiscountState) -> Value {
    json!({ "discount_id": d.id, "id": d.id, "code": d.code, "type": kind_name(d.kind), "amount": d.amount, "expires_at": d.expires_at })
}

pub(crate) async fn create_discount(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>, Json(body): Json<DiscountBody>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    let Some(kind) = parse_kind(&body.kind) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_type", "type is percentage or flat");
    };
    let currency = match one_currency(body.currency.as_deref(), body.currency_options.as_deref(), Some(body.amount)) {
        Ok(c) => c,
        Err(msg) => return problem(StatusCode::BAD_REQUEST, "invalid_currency_options", msg),
    };
    let skus = body.restricted_to.clone().unwrap_or_default();
    let (_, account) = match dodo_account(&state, &key, &seller_id).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let restricted_to = match product_ids(&state, &key, &seller_id, &skus).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let spec = DiscountSpec {
        code: body.code.trim().to_ascii_uppercase(),
        name: body.name.as_deref().map_or_else(|| body.code.trim().to_owned(), |n| n.trim().to_owned()),
        kind,
        amount: body.amount,
        currency,
        expires_at: body.expires_at.clone(),
        restricted_to,
    };
    if let Err(msg) = spec.validate() {
        return problem(StatusCode::BAD_REQUEST, "invalid_discount", &msg);
    }
    let answer = |id: &str, code: &str| {
        json!({ "discount_id": id, "id": id, "code": code, "type": kind_name(kind), "amount": spec.amount, "currency": spec.currency, "expires_at": spec.expires_at, "restricted_to": skus })
    };
    // A retry (the first answer lost on the way back) finds the code already made.
    match state.store.discount_by_code(&key.product, &seller_id, &spec.code).await {
        Ok(Some(known)) => match dodo::get_discount(state.transport.as_ref(), &account, &known.discount_id).await {
            Ok(d) if d.kind == spec.kind && d.amount == spec.amount => {
                return (StatusCode::OK, Json(json!({ "discount": answer(&d.id, &d.code), "created": false }))).into_response();
            }
            Ok(d) => {
                let msg = format!("{} already exists with other terms; change it with PATCH on discount {}", d.code, d.id);
                return problem(StatusCode::CONFLICT, "code_in_use", &msg);
            }
            Err(SellerError::NotFound) => {}
            Err(e) => return seller_error(e),
        },
        Ok(None) => {}
        Err(_) => return storage(),
    }
    let made = match dodo::create_discount(state.transport.as_ref(), &account, &spec).await {
        Ok(d) => d,
        Err(e) => return seller_error(e),
    };
    let record = SellerDiscountRecord { product: key.product.clone(), seller: seller_id.clone(), discount_id: made.id.clone(), code: made.code.clone(), created_at_ms: (state.now_ms)() };
    if state.store.put_discount(record).await.is_err() {
        tracing::error!(product = %key.product, seller = %seller_id, discount_id = %made.id, "seller: discount made on the payment account but its owner was not recorded");
        return storage();
    }
    (StatusCode::CREATED, Json(json!({ "discount": answer(&made.id, &made.code), "created": true }))).into_response()
}

/// Turns a PATCH body into the checked change (before any lookup or call).
async fn parse_patch(state: &SellerState, key: &ProductKey, seller_id: &str, raw: &[u8]) -> Result<DiscountPatch, Response> {
    let body: PatchBody = serde_json::from_slice(raw).map_err(|e| {
        problem(StatusCode::BAD_REQUEST, "invalid_body", &format!("the body must be a JSON object of discount fields ({})", e.to_string().chars().take(120).collect::<String>()))
    })?;
    let kind = match body.kind.as_deref() {
        None => None,
        Some(k) => Some(parse_kind(k).ok_or_else(|| problem(StatusCode::BAD_REQUEST, "invalid_type", "type is percentage or flat"))?),
    };
    let currency = one_currency(body.currency.as_deref(), body.currency_options.as_deref(), body.amount)
        .map_err(|msg| problem(StatusCode::BAD_REQUEST, "invalid_currency_options", msg))?;
    let restricted_to = match &body.restricted_to {
        None => None,
        Some(skus) => Some(product_ids(state, key, seller_id, skus).await?),
    };
    let patch = DiscountPatch {
        code: body.code.map(|c| c.trim().to_ascii_uppercase()),
        name: body.name.map(|n| n.trim().to_owned()),
        kind,
        amount: body.amount,
        currency,
        expires_at: body.expires_at,
        restricted_to,
    };
    patch.validate().map_err(|msg| problem(StatusCode::BAD_REQUEST, "invalid_discount", &msg))?;
    Ok(patch)
}

pub(crate) async fn update_discount(State(state): State<SellerState>, headers: HeaderMap, Path((seller_id, discount_id)): Path<(String, String)>, raw: Bytes) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    if !seller::valid_discount_id(&discount_id) {
        return problem(StatusCode::BAD_REQUEST, "invalid_discount_id", "the discount id must be letters, digits or underscores");
    }
    let patch = match parse_patch(&state, &key, &seller_id, &raw).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (_, account) = match dodo_account(&state, &key, &seller_id).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    // Ownership: only a discount this seller of this product made.
    match state.store.get_discount(&key.product, &seller_id, &discount_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return unknown_discount(),
        Err(_) => return storage(),
    }
    let changed = match dodo::update_discount(state.transport.as_ref(), &account, &discount_id, &patch).await {
        Ok(d) => d,
        Err(SellerError::NotFound) => return unknown_discount(),
        Err(e) => return seller_error(e),
    };
    if patch.code.is_some() {
        let record = SellerDiscountRecord { product: key.product.clone(), seller: seller_id.clone(), discount_id: changed.id.clone(), code: changed.code.clone(), created_at_ms: (state.now_ms)() };
        if state.store.put_discount(record).await.is_err() {
            tracing::warn!(product = %key.product, seller = %seller_id, discount_id = %changed.id, "seller: discount re-coded but the stored code is stale");
        }
    }
    (StatusCode::OK, Json(json!({ "discount": state_json(&changed) }))).into_response()
}
