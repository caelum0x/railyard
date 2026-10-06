//! Seller-own payments API (Wave P PY2.3, VC6; dispatch contract 5.5).
//!
//! A calling product (Scribase for its customers' apps, Menivor for MV13)
//! lets its customer sell through **their own** payment account:
//!
//! - `POST /mint/v1/sellers/{seller}/connect` `{return_url?}` → `{connect_url,
//!   expires_in, stripe}`: a one-time link to Mint's connect page. The calling
//!   product never shows a key field; the seller types their key on that page
//!   only (Dodo, under Advanced) or authorizes on Stripe's own page (Stripe
//!   Connect, when the operator has switched it on: OD-9).
//! - `GET  /mint/v1/sellers/{seller}` → connection state, products and the
//!   webhook address to paste in the seller's Dodo dashboard.
//! - `POST /mint/v1/sellers/{seller}/products` `{sku, name, price_cents,
//!   currency, interval?}` and `GET` the list: created on the seller's account.
//! - `POST /mint/v1/sellers/{seller}/checkout-sessions` `{sku, buyer_ref,
//!   success_url, cancel_url?}` → `{url, id}`: the buyer pays the seller's
//!   processor directly.
//!   `discount_code?` applies one of the seller's discount codes up front.
//! - `POST  /mint/v1/sellers/{seller}/discounts` and
//!   `PATCH /mint/v1/sellers/{seller}/discounts/{discount_id}`: discount codes
//!   on the seller's own Dodo account (see [`crate::seller_discounts`]).
//! - `GET  /mint/v1/sellers/{seller}/entitlements?limit=`: recent events.
//! - Seller webhooks (`/mint/webhooks/seller/{product}/{seller}/dodo`, and
//!   `/mint/webhooks/stripe-connect`) become `entitlement.granted` /
//!   `entitlement.revoked` events, delivered Standard Webhooks signed to the
//!   calling product's endpoint with retries.
//!
//! Auth: each calling product has a key and an endpoint in
//! `MINT_SELLER_PRODUCTS_JSON`. Credentials are sealed with
//! `MINT_SELLER_SEAL_KEY`. **This module has no handle on the ledger**: no
//! deposit, balance, hold or payout exists for seller payments; Mint never
//! receives or forwards a buyer's money (tested in tests/seller.rs).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use mint_payments::seller::seal::SealKey;
use mint_payments::seller::stripe::ConnectConfig;
use mint_payments::seller::{
    self, dodo, stripe, CheckoutMeta, EntitlementEvent, Interval, ProductSpec, SellerAccount, SellerError, SellerMode, SellerProvider,
};
use mint_payments::{HttpTransport, WebhookError};
use mint_store::{ConnectToken, EntitlementRecord, SellerProductRecord, SellerRecord, SellerStore};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::seller_page;

/// How long a connect link works.
pub const CONNECT_TTL_MS: i64 = 15 * 60 * 1000;

/// One calling product: its key and where its entitlement events go.
#[derive(Clone, Debug, Deserialize)]
pub struct ProductKey {
    pub product: String,
    pub key: String,
    pub webhook_url: String,
    pub webhook_secret: String,
}

/// Everything seller payments need; deliberately no ledger.
#[derive(Clone)]
pub struct SellerState {
    pub store: Arc<dyn SellerStore>,
    pub products: Arc<Vec<ProductKey>>,
    pub seal: Option<Arc<SealKey>>,
    pub transport: Arc<dyn HttpTransport>,
    /// Mint's public base (`MINT_PUBLIC_URL`), for connect links and webhook addresses.
    pub public_url: String,
    pub stripe: Option<ConnectConfig>,
    pub now_ms: fn() -> i64,
}

fn valid_product(value: &str) -> bool {
    (2..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn https_or_local(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

/// Parses `MINT_SELLER_PRODUCTS_JSON` (`[{"product","key","webhook_url","webhook_secret"}]`).
///
/// # Errors
/// A plain reason for the first entry that cannot be used.
pub fn parse_products(raw: &str) -> Result<Vec<ProductKey>, String> {
    let keys: Vec<ProductKey> = serde_json::from_str(raw).map_err(|e| format!("MINT_SELLER_PRODUCTS_JSON: {e}"))?;
    for k in &keys {
        if !valid_product(&k.product) || k.key.len() < 24 || !https_or_local(&k.webhook_url) || !k.webhook_secret.starts_with("whsec_") {
            return Err(format!(
                "MINT_SELLER_PRODUCTS_JSON: product {:?} needs a lower-case name, a key of 24+ characters, an https webhook_url and a whsec_ secret",
                k.product
            ));
        }
    }
    Ok(keys)
}

/// The Stripe Connect settings when all three are set (OD-9), else `None`.
pub fn stripe_from_env(client_id: Option<String>, secret: Option<String>, webhook_secret: Option<String>) -> Option<ConnectConfig> {
    match (client_id, secret, webhook_secret) {
        (Some(client_id), Some(platform_secret), Some(webhook_secret)) if client_id.starts_with("ca_") => {
            Some(ConnectConfig { client_id, platform_secret, webhook_secret })
        }
        _ => None,
    }
}

pub(crate) fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": { "code": code, "message": detail } }))).into_response()
}

pub(crate) fn caller(state: &SellerState, headers: &HeaderMap) -> Option<ProductKey> {
    let given = headers.get("x-api-key")?.to_str().ok()?;
    state
        .products
        .iter()
        .find(|k| k.key.len() == given.len() && bool::from(k.key.as_bytes().ct_eq(given.as_bytes())))
        .cloned()
}

pub(crate) fn seller_key(product: &str, seller: &str) -> String {
    format!("{product}:{seller}")
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub(crate) fn seller_error(error: SellerError) -> Response {
    match error {
        SellerError::Invalid(msg) => problem(StatusCode::BAD_REQUEST, "invalid_request", &msg),
        SellerError::Refused(msg) => problem(StatusCode::UNPROCESSABLE_ENTITY, "refused_by_payment_account", &msg),
        SellerError::NotConnected => problem(StatusCode::CONFLICT, "not_connected", "this seller has not connected a payment account yet"),
        SellerError::NotFound => problem(StatusCode::NOT_FOUND, "unknown_object", "the payment account has no such object"),
        SellerError::Unavailable => problem(StatusCode::BAD_GATEWAY, "payment_account_unavailable", "the payment account did not answer; nothing was changed"),
    }
}

pub(crate) fn storage() -> Response {
    problem(StatusCode::SERVICE_UNAVAILABLE, "storage", "seller payments could not be read or written; retry")
}

/// The seller's account, opened for one call.
pub(crate) fn open_account(state: &SellerState, record: &SellerRecord) -> Result<SellerAccount, SellerError> {
    let seal = state.seal.as_ref().ok_or(SellerError::NotConnected)?;
    let context = seller_key(&record.product, &record.seller);
    let secret = seal.open(&context, &record.sealed_secret).map_err(|_| SellerError::NotConnected)?;
    let webhook_secret = match &record.sealed_webhook_secret {
        Some(sealed) => Some(seal.open(&context, sealed).map_err(|_| SellerError::NotConnected)?),
        None => None,
    };
    Ok(SellerAccount {
        provider: SellerProvider::parse(&record.provider).ok_or(SellerError::NotConnected)?,
        mode: SellerMode::parse(&record.mode).ok_or(SellerError::NotConnected)?,
        secret,
        webhook_secret,
    })
}

fn dodo_webhook_url(state: &SellerState, product: &str, seller: &str) -> String {
    format!("{}/mint/webhooks/seller/{product}/{seller}/dodo", state.public_url.trim_end_matches('/'))
}

// ─── the calling product's API ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct ConnectBody {
    return_url: Option<String>,
}

async fn connect(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>, body: Bytes) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    if state.seal.is_none() {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "not_configured", "seller payments are not set up on this deployment");
    }
    if !seller::valid_seller(&seller_id) {
        return problem(StatusCode::BAD_REQUEST, "invalid_seller", "the seller id must be letters, digits, dots, dashes or underscores");
    }
    let parsed: ConnectBody = if body.is_empty() { ConnectBody::default() } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return problem(StatusCode::BAD_REQUEST, "invalid_body", "the body must be JSON"),
        }
    };
    if parsed.return_url.as_deref().is_some_and(|u| !u.starts_with("https://") || u.len() > 500) {
        return problem(StatusCode::BAD_REQUEST, "invalid_return_url", "return_url must be an https address");
    }
    let token = new_token();
    let record = ConnectToken {
        token_hash: token_hash(&token),
        product: key.product.clone(),
        seller: seller_id,
        return_url: parsed.return_url,
        expires_at_ms: (state.now_ms)() + CONNECT_TTL_MS,
    };
    if state.store.put_token(record).await.is_err() {
        return storage();
    }
    let url = format!("{}/mint/connect/{token}", state.public_url.trim_end_matches('/'));
    (StatusCode::CREATED, Json(json!({ "connect_url": url, "expires_in": CONNECT_TTL_MS / 1000, "stripe": state.stripe.is_some() }))).into_response()
}

fn product_json(p: &SellerProductRecord) -> Value {
    json!({ "sku": p.sku, "name": p.name, "price_cents": p.price_cents, "currency": p.currency, "interval": p.interval })
}

async fn get_seller(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    let (Ok(record), Ok(products)) = (state.store.get_seller(&key.product, &seller_id).await, state.store.list_products(&key.product, &seller_id).await) else {
        return storage();
    };
    let body = match record {
        None => json!({ "seller": seller_id, "connected": false, "products": [] }),
        Some(r) => json!({
            "seller": seller_id, "connected": true, "provider": r.provider, "mode": r.mode,
            "connected_at_ms": r.connected_at_ms,
            "webhook_url": (r.provider == "dodo").then(|| dodo_webhook_url(&state, &key.product, &seller_id)),
            "products": products.iter().map(product_json).collect::<Vec<_>>(),
        }),
    };
    Json(body).into_response()
}

#[derive(Deserialize)]
struct ProductBody {
    sku: String,
    name: String,
    price_cents: i64,
    currency: String,
    interval: Option<String>,
}

async fn create_product(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>, Json(body): Json<ProductBody>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    let interval = match body.interval.as_deref() {
        None | Some("") => None,
        Some("month") => Some(Interval::Month),
        Some("year") => Some(Interval::Year),
        Some(_) => return problem(StatusCode::BAD_REQUEST, "invalid_interval", "interval is month, year, or empty for one-time"),
    };
    let spec = ProductSpec { sku: body.sku, name: body.name.trim().to_owned(), price_cents: body.price_cents, currency: body.currency, interval };
    if let Err(msg) = spec.validate() {
        return problem(StatusCode::BAD_REQUEST, "invalid_product", &msg);
    }
    match state.store.get_product(&key.product, &seller_id, &spec.sku).await {
        Ok(Some(existing)) => return (StatusCode::OK, Json(json!({ "product": product_json(&existing), "created": false }))).into_response(),
        Ok(None) => {}
        Err(_) => return storage(),
    }
    let Ok(Some(record)) = state.store.get_seller(&key.product, &seller_id).await else {
        return seller_error(SellerError::NotConnected);
    };
    let account = match open_account(&state, &record) {
        Ok(a) => a,
        Err(e) => return seller_error(e),
    };
    let made = match account.provider {
        SellerProvider::Dodo => dodo::create_product(state.transport.as_ref(), &account, &spec).await,
        SellerProvider::Stripe => match &state.stripe {
            Some(cfg) => stripe::create_product(state.transport.as_ref(), cfg, &account.secret, &spec).await,
            None => Err(SellerError::NotConnected),
        },
    };
    let provider_ref = match made {
        Ok(r) => r,
        Err(e) => return seller_error(e),
    };
    let saved = state
        .store
        .put_product(SellerProductRecord {
            product: key.product,
            seller: seller_id,
            sku: spec.sku,
            name: spec.name,
            price_cents: spec.price_cents,
            currency: spec.currency,
            interval: spec.interval.map(|i| if i == Interval::Month { "month".to_owned() } else { "year".to_owned() }),
            provider_ref,
            created_at_ms: (state.now_ms)(),
        })
        .await;
    match saved {
        Ok(p) => (StatusCode::CREATED, Json(json!({ "product": product_json(&p), "created": true }))).into_response(),
        Err(_) => storage(),
    }
}

async fn list_products(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    match state.store.list_products(&key.product, &seller_id).await {
        Ok(list) => Json(json!({ "products": list.iter().map(product_json).collect::<Vec<_>>() })).into_response(),
        Err(_) => storage(),
    }
}

#[derive(Deserialize)]
struct CheckoutBody {
    sku: String,
    buyer_ref: String,
    success_url: String,
    cancel_url: Option<String>,
    discount_code: Option<String>,
}

async fn checkout(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>, Json(body): Json<CheckoutBody>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    if !seller::valid_buyer_ref(&body.buyer_ref) {
        return problem(StatusCode::BAD_REQUEST, "invalid_buyer_ref", "buyer_ref must be the app's user id (1-200 printable characters)");
    }
    if body.discount_code.as_deref().is_some_and(|c| !seller::valid_discount_code(c)) {
        return problem(StatusCode::BAD_REQUEST, "invalid_discount_code", "discount_code must be 3-32 letters, digits, dashes or underscores");
    }
    let cancel = body.cancel_url.clone().unwrap_or_else(|| body.success_url.clone());
    if !body.success_url.starts_with("https://") || !cancel.starts_with("https://") || body.success_url.len() > 500 || cancel.len() > 500 {
        return problem(StatusCode::BAD_REQUEST, "invalid_url", "success_url and cancel_url must be https addresses");
    }
    let (record, product) = match (state.store.get_seller(&key.product, &seller_id).await, state.store.get_product(&key.product, &seller_id, &body.sku).await) {
        (Ok(Some(r)), Ok(Some(p))) => (r, p),
        (Ok(None), _) => return seller_error(SellerError::NotConnected),
        (Ok(Some(_)), Ok(None)) => return problem(StatusCode::NOT_FOUND, "unknown_sku", "create the product first"),
        _ => return storage(),
    };
    let account = match open_account(&state, &record) {
        Ok(a) => a,
        Err(e) => return seller_error(e),
    };
    let skey = seller_key(&key.product, &seller_id);
    let meta = CheckoutMeta { seller_key: &skey, buyer_ref: &body.buyer_ref, sku: &body.sku };
    let session = match account.provider {
        SellerProvider::Dodo => dodo::create_checkout_with(state.transport.as_ref(), &account, &product.provider_ref, &meta, &body.success_url, body.discount_code.as_deref()).await,
        SellerProvider::Stripe if body.discount_code.is_some() => {
            return problem(StatusCode::CONFLICT, "discounts_unsupported", "discount codes work on Dodo payment accounts; this seller connected Stripe");
        }
        SellerProvider::Stripe => match &state.stripe {
            Some(cfg) => {
                let request = stripe::CheckoutRequest { price_id: &product.provider_ref, recurring: product.interval.is_some(), success_url: &body.success_url, cancel_url: &cancel };
                stripe::create_checkout(state.transport.as_ref(), cfg, &account.secret, &request, &meta).await
            }
            None => Err(SellerError::NotConnected),
        },
    };
    match session {
        Ok(s) => (StatusCode::CREATED, Json(json!({ "url": s.url, "id": s.id }))).into_response(),
        Err(e) => seller_error(e),
    }
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

fn entitlement_json(e: &EntitlementRecord) -> Value {
    json!({
        "id": e.id, "seller": e.seller, "kind": e.kind, "reason": e.reason, "buyer_ref": e.buyer_ref, "sku": e.sku,
        "provider": e.provider, "provider_event_id": e.provider_event_id,
        "occurred_at_ms": e.occurred_at_ms, "expires_at_ms": e.expires_at_ms,
    })
}

async fn list_entitlements(State(state): State<SellerState>, headers: HeaderMap, Path(seller_id): Path<String>, Query(q): Query<LimitQuery>) -> Response {
    let Some(key) = caller(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product seller key is required");
    };
    match state.store.list_entitlements(&key.product, &seller_id, q.limit.unwrap_or(50).min(500)).await {
        Ok(list) => Json(json!({ "entitlements": list.iter().map(entitlement_json).collect::<Vec<_>>() })).into_response(),
        Err(_) => storage(),
    }
}

// ─── the connect page (the only place a seller types a key) ─────────────────

fn page(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' https://connect.stripe.com; frame-ancestors 'none'"),
        ],
        Html(body),
    )
        .into_response()
}

fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn connect_page(State(state): State<SellerState>, Path(token): Path<String>) -> Response {
    let found = if valid_token(&token) { state.store.peek_token(&token_hash(&token), (state.now_ms)()).await.ok().flatten() } else { None };
    match found {
        Some(t) => page(StatusCode::OK, seller_page::connect(&token, state.stripe.is_some(), None, Some(&dodo_webhook_url(&state, &t.product, &t.seller)))),
        None => page(StatusCode::NOT_FOUND, seller_page::expired()),
    }
}

fn form_fields(raw: &[u8]) -> HashMap<String, String> {
    let decode = |s: &str| {
        let s = s.replace('+', " ");
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    String::from_utf8_lossy(raw)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn clean_secret(value: Option<&String>, min: usize) -> Option<String> {
    let v = value?.trim();
    ((min..=400).contains(&v.len()) && !v.chars().any(char::is_whitespace)).then(|| v.to_owned())
}

fn connect_again(state: &SellerState, token: &str, pending: &ConnectToken, error: Option<&str>) -> String {
    seller_page::connect(token, state.stripe.is_some(), error, Some(&dodo_webhook_url(state, &pending.product, &pending.seller)))
}

async fn connect_submit(State(state): State<SellerState>, Path(token): Path<String>, body: Bytes) -> Response {
    let now = (state.now_ms)();
    let Some(seal) = state.seal.clone() else {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    };
    if !valid_token(&token) {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    }
    let hash = token_hash(&token);
    let Ok(Some(pending)) = state.store.peek_token(&hash, now).await else {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    };
    let fields = form_fields(&body);
    let (Some(api_key), Some(webhook_secret)) = (clean_secret(fields.get("api_key"), 10), clean_secret(fields.get("webhook_secret"), 10)) else {
        return page(StatusCode::BAD_REQUEST, connect_again(&state, &token, &pending, Some("Paste your API key and your webhook secret.")));
    };
    if !webhook_secret.starts_with("whsec_") {
        return page(StatusCode::BAD_REQUEST, connect_again(&state, &token, &pending, Some("The webhook secret starts with whsec_.")));
    }
    let mode = SellerMode::parse(fields.get("mode").map_or("test", String::as_str)).unwrap_or(SellerMode::Test);
    let account = SellerAccount { provider: SellerProvider::Dodo, mode, secret: api_key.clone(), webhook_secret: Some(webhook_secret.clone()) };
    if let Err(e) = dodo::validate(state.transport.as_ref(), &account).await {
        let msg = match e {
            SellerError::Refused(_) => "That key was not accepted in this mode. Check test or live and try again.",
            _ => "Your payment account did not answer. Try again in a minute.",
        };
        return page(StatusCode::UNPROCESSABLE_ENTITY, connect_again(&state, &token, &pending, Some(msg)));
    }
    let Ok(Some(token_row)) = state.store.take_token(&hash, now).await else {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    };
    let context = seller_key(&pending.product, &pending.seller);
    let (Ok(sealed), Ok(sealed_hook)) = (seal.seal(&context, &api_key), seal.seal(&context, &webhook_secret)) else {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    };
    let saved = state
        .store
        .upsert_seller(SellerRecord {
            product: token_row.product.clone(),
            seller: token_row.seller.clone(),
            provider: "dodo".into(),
            mode: mode.as_str().into(),
            sealed_secret: sealed,
            sealed_webhook_secret: Some(sealed_hook),
            account_ref: None,
            connected_at_ms: now,
        })
        .await;
    if saved.is_err() {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    }
    let hook = dodo_webhook_url(&state, &token_row.product, &token_row.seller);
    page(StatusCode::OK, seller_page::connected("Dodo Payments", Some(&hook), token_row.return_url.as_deref()))
}

async fn stripe_start(State(state): State<SellerState>, Path(token): Path<String>) -> Response {
    let Some(cfg) = state.stripe.clone() else {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    };
    if !valid_token(&token) || !matches!(state.store.peek_token(&token_hash(&token), (state.now_ms)()).await, Ok(Some(_))) {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    }
    let callback = format!("{}/mint/connect/stripe/callback", state.public_url.trim_end_matches('/'));
    Redirect::to(&stripe::authorize_url(&cfg, &token, &callback)).into_response()
}

#[derive(Deserialize)]
struct StripeCallback {
    code: Option<String>,
    state: Option<String>,
}

async fn stripe_callback(State(state): State<SellerState>, Query(q): Query<StripeCallback>) -> Response {
    let (Some(cfg), Some(code), Some(token)) = (state.stripe.clone(), q.code, q.state) else {
        return page(StatusCode::BAD_REQUEST, seller_page::expired());
    };
    let Some(seal) = state.seal.clone() else {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    };
    let now = (state.now_ms)();
    if !valid_token(&token) || code.len() > 200 {
        return page(StatusCode::BAD_REQUEST, seller_page::expired());
    }
    let Ok(Some(row)) = state.store.take_token(&token_hash(&token), now).await else {
        return page(StatusCode::NOT_FOUND, seller_page::expired());
    };
    let account = match stripe::exchange_code(state.transport.as_ref(), &cfg, &code).await {
        Ok(a) => a,
        Err(_) => return page(StatusCode::BAD_GATEWAY, seller_page::expired()),
    };
    let context = seller_key(&row.product, &row.seller);
    let Ok(sealed) = seal.seal(&context, &account) else {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    };
    let saved = state
        .store
        .upsert_seller(SellerRecord {
            product: row.product.clone(),
            seller: row.seller.clone(),
            provider: "stripe".into(),
            mode: if cfg.platform_secret.starts_with("sk_live_") { "live".into() } else { "test".into() },
            sealed_secret: sealed,
            sealed_webhook_secret: None,
            account_ref: Some(account),
            connected_at_ms: now,
        })
        .await;
    if saved.is_err() {
        return page(StatusCode::SERVICE_UNAVAILABLE, seller_page::expired());
    }
    page(StatusCode::OK, seller_page::connected("Stripe", None, row.return_url.as_deref()))
}

// ─── the sellers' processor webhooks ────────────────────────────────────────

fn record_from(product: &str, seller_id: &str, provider: &str, e: &EntitlementEvent, now: i64) -> EntitlementRecord {
    EntitlementRecord {
        id: format!("ent_{}", ulid::Ulid::new().to_string().to_lowercase()),
        product: product.to_owned(),
        seller: seller_id.to_owned(),
        provider: provider.to_owned(),
        provider_event_id: e.provider_event_id.clone(),
        kind: match e.kind {
            seller::EntitlementKind::Granted => "granted".into(),
            seller::EntitlementKind::Revoked => "revoked".into(),
        },
        reason: e.reason.map(|r| r.as_str().to_owned()),
        buyer_ref: e.buyer_ref.clone(),
        sku: e.sku.clone(),
        occurred_at_ms: e.occurred_at_ms,
        expires_at_ms: e.expires_at_ms,
        recorded_at_ms: now,
    }
}

fn webhook_problem(error: WebhookError) -> Response {
    match error {
        WebhookError::BadSignature => problem(StatusCode::UNAUTHORIZED, "bad_signature", "the webhook signature did not verify"),
        WebhookError::NotConfigured => problem(StatusCode::NOT_FOUND, "not_configured", "this seller has no webhook secret"),
        WebhookError::Ignored(_) => (StatusCode::OK, Json(json!({ "ignored": true }))).into_response(),
        // A signed event we cannot map yet: ask the processor to retry.
        WebhookError::Malformed(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "retry", "the event could not be mapped yet; retry"),
    }
}

async fn dodo_webhook(State(state): State<SellerState>, Path((product, seller_id)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> Response {
    let now = (state.now_ms)();
    let Ok(Some(record)) = state.store.get_seller(&product, &seller_id).await else {
        return problem(StatusCode::NOT_FOUND, "unknown_seller", "no seller is connected at this address");
    };
    if record.provider != "dodo" {
        return problem(StatusCode::NOT_FOUND, "unknown_seller", "no seller is connected at this address");
    }
    let account = match open_account(&state, &record) {
        Ok(a) => a,
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "retry", "the seller account could not be opened; retry"),
    };
    let lookup: HashMap<String, String> = headers.iter().filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned()))).collect();
    let skey = seller_key(&product, &seller_id);
    match dodo::verify_event(state.transport.as_ref(), &account, &skey, &lookup, &body, now / 1000).await {
        Ok(Some(event)) => match state.store.record_entitlement(record_from(&product, &seller_id, "dodo", &event, now)).await {
            Ok(fresh) => Json(json!({ "recorded": fresh })).into_response(),
            Err(_) => storage(),
        },
        Ok(None) => Json(json!({ "ignored": true })).into_response(),
        Err(e) => webhook_problem(e),
    }
}

async fn stripe_webhook(State(state): State<SellerState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(cfg) = state.stripe.clone() else {
        return problem(StatusCode::NOT_FOUND, "not_configured", "Stripe Connect is not set up on this deployment");
    };
    let now = (state.now_ms)();
    let lookup: HashMap<String, String> = headers.iter().filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned()))).collect();
    let verified = match stripe::verify_event(state.transport.as_ref(), &cfg, &lookup, &body, now / 1000).await {
        Ok(v) => v,
        Err(e) => return webhook_problem(e),
    };
    if verified.deauthorized {
        // The seller disconnected us in Stripe: the account can sell nothing
        // through Mint anymore, so the connection and its products go.
        return match state.store.disconnect_account("stripe", &verified.account).await {
            Ok(Some(r)) => {
                tracing::info!(product = %r.product, seller = %r.seller, "seller: Stripe account deauthorized");
                Json(json!({ "disconnected": true })).into_response()
            }
            Ok(None) => Json(json!({ "ignored": true })).into_response(),
            Err(_) => storage(),
        };
    }
    let Ok(Some(record)) = state.store.seller_by_account("stripe", &verified.account).await else {
        return Json(json!({ "ignored": true })).into_response();
    };
    let Some(event) = verified.event else {
        return Json(json!({ "ignored": true })).into_response();
    };
    // The metadata must name the seller this account belongs to.
    if event.seller_key != seller_key(&record.product, &record.seller) {
        return Json(json!({ "ignored": true })).into_response();
    }
    match state.store.record_entitlement(record_from(&record.product, &record.seller, "stripe", &event, now)).await {
        Ok(fresh) => Json(json!({ "recorded": fresh })).into_response(),
        Err(_) => storage(),
    }
}

/// The seller routes (own auth: product seller keys and processor signatures).
pub fn router(state: SellerState) -> Router {
    Router::new()
        .route("/mint/v1/sellers/:seller", get(get_seller))
        .route("/mint/v1/sellers/:seller/connect", post(connect))
        .route("/mint/v1/sellers/:seller/products", post(create_product).get(list_products))
        .route("/mint/v1/sellers/:seller/checkout-sessions", post(checkout))
        .route("/mint/v1/sellers/:seller/discounts", post(crate::seller_discounts::create_discount))
        .route("/mint/v1/sellers/:seller/discounts/:discount", patch(crate::seller_discounts::update_discount))
        .route("/mint/v1/sellers/:seller/entitlements", get(list_entitlements))
        .route("/mint/connect/stripe/callback", get(stripe_callback))
        .route("/mint/connect/:token", get(connect_page).post(connect_submit))
        .route("/mint/connect/:token/stripe", get(stripe_start))
        .route("/mint/webhooks/seller/:product/:seller/dodo", post(dodo_webhook))
        .route("/mint/webhooks/stripe-connect", post(stripe_webhook))
        .with_state(state)
}

// ─── delivery to the calling product ────────────────────────────────────────

/// The signed body a calling product receives.
pub fn delivery_body(e: &EntitlementRecord) -> Vec<u8> {
    let kind = if e.kind == "granted" { "entitlement.granted" } else { "entitlement.revoked" };
    serde_json::to_vec(&json!({ "type": kind, "data": entitlement_json(e) })).unwrap_or_default()
}

/// Delivers every due event once; returns (delivered, failed).
pub async fn deliver_due(state: &SellerState) -> (usize, usize) {
    let now = (state.now_ms)();
    let Ok(due) = state.store.due(now, 100).await else {
        tracing::error!("seller: could not read the entitlement outbox");
        return (0, 0);
    };
    let (mut ok, mut failed) = (0, 0);
    for d in due {
        let Some(endpoint) = state.products.iter().find(|p| p.product == d.record.product) else {
            let _ = state.store.failed(&d.record.id, now + crate::mirror::backoff_ms(d.attempts), "product no longer configured").await;
            failed += 1;
            continue;
        };
        let ts = (now / 1000).to_string();
        match mint_payments::outbound::deliver(state.transport.as_ref(), &endpoint.webhook_url, &endpoint.webhook_secret, &d.record.id, &ts, &delivery_body(&d.record)).await {
            Ok(status) if (200..300).contains(&status) => {
                let _ = state.store.delivered(&d.record.id, now).await;
                ok += 1;
            }
            Ok(status) => {
                let _ = state.store.failed(&d.record.id, now + crate::mirror::backoff_ms(d.attempts), &format!("HTTP {status}")).await;
                failed += 1;
            }
            Err(e) => {
                let _ = state.store.failed(&d.record.id, now + crate::mirror::backoff_ms(d.attempts), &e.0).await;
                failed += 1;
            }
        }
    }
    if failed > 0 {
        tracing::warn!(delivered = ok, failed, "seller: some entitlement deliveries will be retried");
    }
    (ok, failed)
}

/// Delivers the outbox every 10 s in the background.
pub fn spawn_delivery(state: SellerState) {
    if state.products.is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            deliver_due(&state).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{form_fields, parse_products, stripe_from_env};

    #[test]
    fn product_keys_need_a_name_a_long_key_an_https_endpoint_and_a_whsec_secret() {
        assert!(parse_products(r#"[{"product":"scribase","key":"k-0123456789abcdef0123456789","webhook_url":"https://api.scribase.com/v1/railyard/billing/webhooks","webhook_secret":"whsec_abc"}]"#).is_ok());
        assert!(parse_products(r#"[{"product":"scribase","key":"short","webhook_url":"https://x","webhook_secret":"whsec_abc"}]"#).is_err());
        assert!(parse_products(r#"[{"product":"scribase","key":"k-0123456789abcdef0123456789","webhook_url":"http://x","webhook_secret":"whsec_abc"}]"#).is_err());
        assert!(stripe_from_env(Some("ca_1".into()), Some("sk".into()), None).is_none(), "OD-9: all three or off");
    }

    #[test]
    fn form_fields_decode() {
        let f = form_fields(b"api_key=abc%2Bdef&webhook_secret=whsec_x%3D&mode=test&x=a+b");
        assert_eq!(f["api_key"], "abc+def");
        assert_eq!(f["webhook_secret"], "whsec_x=");
        assert_eq!(f["x"], "a b");
        assert_eq!(form_fields(b"k=%zz")["k"], "%zz");
    }
}
