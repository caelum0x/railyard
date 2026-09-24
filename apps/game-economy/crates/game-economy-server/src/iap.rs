//! In-app purchase (IAP) hooks: turn a store-verified App Store / Google Play
//! purchase into premium currency on the player's Mint account.
//!
//! game-economy does NOT parse Apple/Google receipts itself. Receipt validation
//! is done upstream by whoever already holds the store credentials:
//!
//! - RevenueCat (validates with Apple/Google, then calls
//!   `POST /v1/iap/revenuecat` with the webhook `Authorization` value set in
//!   the RevenueCat dashboard), or
//! - the game's own backend (validates the receipt, then calls
//!   `POST /v1/iap/credit` with `X-Game-Server-Key`).
//!
//! Either way the credit is a Mint transfer from the IAP funding account
//! (`GAME_IAP_FUNDING_ACCOUNT`) to the player's Mint account. The amount comes
//! from the operator's product table (`GAME_IAP_PRODUCTS`), never from the
//! request, and the Mint idempotency key is `iap:<platform>:<transaction_id>`,
//! so a retried webhook or a replayed call can never credit twice. A store
//! refund reverses the credit with the key `iap:<platform>:<transaction_id>:reverse`.
//!
//! The Mint journal is the ledger of record: no local IAP table is kept.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use game_economy_core::{Fulfiller, GameError, MintTransport, Refund, RefundReceipt};

use crate::{error, game_error, ok, AppState};

/// Longest accepted store transaction id / product id / platform label.
const MAX_ID_LEN: usize = 256;

/// Operator IAP configuration.
#[derive(Debug, Clone)]
pub struct IapConfig {
    funding_account: String,
    /// Store product id -> µ$ credited to the player.
    products: HashMap<String, i64>,
    /// Hashed RevenueCat webhook `Authorization` value.
    revenuecat_auth: Option<blake3::Hash>,
}

impl IapConfig {
    /// Build a config. `funding_account` is the Mint account credits are paid
    /// from; every product must credit a positive amount.
    pub fn new(
        funding_account: impl Into<String>,
        products: HashMap<String, i64>,
    ) -> Result<Self, String> {
        let funding_account = funding_account.into().trim().to_string();
        if funding_account.is_empty() {
            return Err("IAP funding account must be set".into());
        }
        if products.is_empty() {
            return Err("at least one IAP product must be configured".into());
        }
        if let Some((id, micros)) = products.iter().find(|(_, m)| **m <= 0) {
            return Err(format!(
                "IAP product {id} must credit a positive amount, got {micros}"
            ));
        }
        Ok(Self {
            funding_account,
            products,
            revenuecat_auth: None,
        })
    }

    /// Parse `GAME_IAP_PRODUCTS`: `store_product_id=micros` pairs separated by
    /// commas, e.g. `coins_100=990000,coins_550=4990000`.
    pub fn parse_products(spec: &str) -> Result<HashMap<String, i64>, String> {
        let mut out = HashMap::new();
        for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (id, micros) = pair
                .split_once('=')
                .ok_or_else(|| format!("IAP product entry {pair:?} must be id=micros"))?;
            let id = id.trim();
            if id.is_empty() || id.len() > MAX_ID_LEN {
                return Err(format!("IAP product entry {pair:?} has an invalid id"));
            }
            let micros: i64 = micros
                .trim()
                .parse()
                .map_err(|_| format!("IAP product {id} amount must be an integer (µ$)"))?;
            if out.insert(id.to_string(), micros).is_some() {
                return Err(format!("IAP product {id} is listed twice"));
            }
        }
        Ok(out)
    }

    /// Enable `POST /v1/iap/revenuecat`, authenticated by the exact
    /// `Authorization` header value configured in RevenueCat.
    pub fn with_revenuecat_auth(mut self, value: &[u8]) -> Self {
        self.revenuecat_auth = Some(blake3::hash(value));
        self
    }

    fn credit_for(&self, product_id: &str) -> Option<i64> {
        self.products.get(product_id).copied()
    }
}

/// A store-verified purchase reported by the game backend.
#[derive(Debug, Deserialize)]
pub(crate) struct IapReq {
    /// The player's Mint account id.
    player: String,
    /// Store label (`app_store`, `play_store`, `revenuecat`, ...). Part of the
    /// idempotency key so transaction ids from different stores never collide.
    platform: String,
    /// The store's transaction id (Apple `transactionId`, Google `orderId`).
    transaction_id: String,
    /// The store product id, looked up in `GAME_IAP_PRODUCTS`.
    product_id: String,
}

impl IapReq {
    fn validate(&self) -> Result<(), Response> {
        let bad = |v: &str| {
            let v = v.trim();
            v.is_empty() || v.len() > MAX_ID_LEN || v.chars().any(char::is_whitespace)
        };
        for (field, value) in [
            ("player", &self.player),
            ("platform", &self.platform),
            ("transaction_id", &self.transaction_id),
            ("product_id", &self.product_id),
        ] {
            if bad(value) {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    &format!("{field} must be a non-empty id without whitespace (max {MAX_ID_LEN} chars)"),
                ));
            }
        }
        Ok(())
    }

    fn idem_key(&self) -> String {
        format!(
            "iap:{}:{}",
            self.platform.trim(),
            self.transaction_id.trim()
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Credit,
    Reverse,
}

fn not_configured() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "iap_not_configured",
        "GAME_IAP_FUNDING_ACCOUNT and GAME_IAP_PRODUCTS are not configured; IAP routes are disabled",
    )
}

/// `POST /v1/iap/credit` — SERVER-TO-SERVER (`X-Game-Server-Key`): credit a
/// store-verified purchase. Idempotent per `(platform, transaction_id)`.
pub(crate) async fn credit_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<IapReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    apply(&state, &req, Direction::Credit).await
}

/// `POST /v1/iap/reverse` — SERVER-TO-SERVER: the store refunded a purchase;
/// move the credited amount back from the player to the funding account.
pub(crate) async fn reverse_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<IapReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    apply(&state, &req, Direction::Reverse).await
}

async fn apply<M, F>(state: &AppState<M, F>, req: &IapReq, dir: Direction) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let Some(cfg) = state.iap() else {
        return not_configured();
    };
    if let Err(response) = req.validate() {
        return response;
    }
    let Some(amount) = cfg.credit_for(req.product_id.trim()) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_iap_product",
            "product_id is not listed in GAME_IAP_PRODUCTS",
        );
    };
    match transfer(state, cfg, req, amount, dir).await {
        Ok(receipt) => ok(receipt_view(req, dir, &receipt)),
        Err(err) => transfer_error(&err, dir),
    }
}

async fn transfer<M, F>(
    state: &AppState<M, F>,
    cfg: &IapConfig,
    req: &IapReq,
    amount: i64,
    dir: Direction,
) -> Result<RefundReceipt, GameError>
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let player = req.player.trim().to_string();
    let (from, to, idem_key) = match dir {
        Direction::Credit => (cfg.funding_account.clone(), player, req.idem_key()),
        Direction::Reverse => (
            player,
            cfg.funding_account.clone(),
            format!("{}:reverse", req.idem_key()),
        ),
    };
    state
        .store()
        .mint()
        .refund(Refund {
            from,
            to,
            amount_micros: amount,
            idem_key,
            reference: Some(format!(
                "iap {} {} {}",
                req.platform.trim(),
                req.product_id.trim(),
                req.transaction_id.trim()
            )),
        })
        .await
}

fn receipt_view(req: &IapReq, dir: Direction, r: &RefundReceipt) -> serde_json::Value {
    json!({
        "player": req.player.trim(),
        "platform": req.platform.trim(),
        "transaction_id": req.transaction_id.trim(),
        "product_id": req.product_id.trim(),
        "direction": if dir == Direction::Credit { "credit" } else { "reverse" },
        "amount_micros": r.refunded_micros,
        "settlement_seq": r.settlement_seq,
        "receipt_hash": r.receipt_hash,
        "idempotent_replay": r.idempotent_replay,
    })
}

/// Mint-transfer errors in IAP terms. The HTTP Mint client words transfer
/// failures for the refund path, so they are re-worded here.
fn transfer_error(err: &GameError, dir: Direction) -> Response {
    let text = err.to_string();
    let insufficient =
        matches!(err, GameError::InsufficientFunds) || text.contains("insufficient funds");
    if insufficient {
        tracing::warn!(error = %err, "iap transfer: insufficient funds");
        return match dir {
            Direction::Credit => error(
                StatusCode::BAD_GATEWAY,
                "iap_funding_exhausted",
                "the IAP funding account cannot cover this credit; top it up in Mint",
            ),
            Direction::Reverse => error(
                StatusCode::PAYMENT_REQUIRED,
                "currency_already_spent",
                "the player no longer holds enough currency to reverse this purchase",
            ),
        };
    }
    if text.contains("account not found") {
        tracing::warn!(error = %err, "iap transfer: unknown Mint account");
        return error(
            StatusCode::NOT_FOUND,
            "mint_account_not_found",
            "the player or funding account does not exist in Mint",
        );
    }
    game_error(err)
}

// --- RevenueCat webhook ----------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct RevenueCatWebhook {
    event: RevenueCatEvent,
}

#[derive(Debug, Deserialize)]
struct RevenueCatEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    app_user_id: Option<String>,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    transaction_id: Option<String>,
    #[serde(default)]
    store: Option<String>,
}

/// `POST /v1/iap/revenuecat` — RevenueCat webhook. RevenueCat has already
/// validated the receipt with Apple/Google. Purchase events for products in
/// `GAME_IAP_PRODUCTS` credit the player (RevenueCat `app_user_id` must be the
/// player's Mint account id); `REFUND` reverses. Anything else is acknowledged
/// with `200` and ignored so RevenueCat does not retry it.
pub(crate) async fn revenuecat_handler<M, F>(
    State(state): State<AppState<M, F>>,
    headers: HeaderMap,
    Json(hook): Json<RevenueCatWebhook>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let Some(cfg) = state.iap() else {
        return not_configured();
    };
    let Some(expected) = cfg.revenuecat_auth.as_ref() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "revenuecat_not_configured",
            "GAME_REVENUECAT_WEBHOOK_AUTH is not configured",
        );
    };
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| blake3::hash(v.trim().as_bytes()));
    if presented.as_ref() != Some(expected) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid webhook authorization",
        );
    }

    let ev = hook.event;
    let dir = match ev.kind.as_str() {
        "INITIAL_PURCHASE" | "NON_RENEWING_PURCHASE" | "RENEWAL" => Direction::Credit,
        "REFUND" => Direction::Reverse,
        _ => return ignored(&ev.kind, "event type does not move currency"),
    };
    let product_id = ev.product_id.unwrap_or_default();
    if cfg.credit_for(product_id.trim()).is_none() {
        return ignored(&ev.kind, "product is not a currency product");
    }
    let player = ev.app_user_id.unwrap_or_default();
    if player.starts_with("$RCAnonymousID") {
        // Visible as a failed delivery in the RevenueCat dashboard: the game
        // must call Purchases.logIn(<mint account id>) before selling currency.
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "anonymous_app_user",
            "app_user_id is anonymous; log the player in with their Mint account id first",
        );
    }
    let req = IapReq {
        player,
        platform: format!(
            "revenuecat-{}",
            ev.store
                .unwrap_or_else(|| "unknown".into())
                .to_ascii_lowercase()
        ),
        transaction_id: ev.transaction_id.unwrap_or_default(),
        product_id,
    };
    apply(&state, &req, dir).await
}

fn ignored(kind: &str, reason: &str) -> Response {
    ok(json!({ "ignored": true, "event_type": kind, "reason": reason }))
}

/// Shared handle type stored on [`AppState`].
pub(crate) type SharedIap = Arc<IapConfig>;
