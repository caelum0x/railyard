//! Real delivery for the storefront: the [`Fulfiller`] the binary wires.
//!
//! The store captures money ONLY after the fulfiller says the good was
//! delivered. [`ServerFulfiller::Webhook`] makes that a real call into the game
//! backend: after the hold opens, the storefront `POST`s a signed delivery
//! request to `GAME_FULFILLMENT_URL`; a `2xx` means "delivered" (capture), any
//! other status or a transport error means "not delivered" (release the hold,
//! grant nothing).
//!
//! The request body is signed with a BLAKE3-keyed MAC over the raw JSON
//! (`X-Game-Economy-Signature: <hex>`), keyed by `GAME_FULFILLMENT_SECRET`, so
//! the game backend can reject forged delivery calls.
//!
//! [`ServerFulfiller::InventoryOnly`] is for purely server-side digital goods
//! where the storefront inventory (`GET /v1/inventory/:player`) IS the delivery
//! and the game client reads entitlements from it.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use game_economy_core::{Fulfiller, GameError, Product};

/// Header carrying the MAC over the delivery request body.
pub const SIGNATURE_HEADER: &str = "x-game-economy-signature";

/// The fulfiller the server binary runs with.
pub enum ServerFulfiller {
    /// Entitlements live in the storefront inventory only.
    InventoryOnly,
    /// Deliver through the game backend's webhook before capturing.
    Webhook(WebhookFulfiller),
}

#[async_trait]
impl Fulfiller for ServerFulfiller {
    async fn fulfill(&self, player: &str, product: &Product) -> Result<(), GameError> {
        match self {
            ServerFulfiller::InventoryOnly => Ok(()),
            ServerFulfiller::Webhook(w) => w.deliver(player, product).await,
        }
    }
}

/// Signed HTTP delivery into the game backend.
pub struct WebhookFulfiller {
    client: reqwest::Client,
    url: String,
    key: [u8; 32],
}

impl WebhookFulfiller {
    /// Build a webhook fulfiller. `secret` signs every delivery body.
    pub fn new(url: String, secret: &[u8], timeout: Duration) -> Result<Self, String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("GAME_FULFILLMENT_URL must be an http(s) URL".into());
        }
        if secret.len() < 16 {
            return Err("GAME_FULFILLMENT_SECRET must be at least 16 bytes".into());
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| format!("building fulfillment HTTP client: {e}"))?;
        Ok(Self {
            client,
            url,
            key: *blake3::hash(secret).as_bytes(),
        })
    }

    async fn deliver(&self, player: &str, product: &Product) -> Result<(), GameError> {
        let body = json!({
            "player": player,
            "sku": product.sku(),
            "price_micros": product.price_micros(),
            "product": product,
        });
        let raw = serde_json::to_vec(&body)
            .map_err(|e| GameError::FulfillmentFailed(format!("encode: {e}")))?;
        let signature = blake3::keyed_hash(&self.key, &raw).to_hex().to_string();
        let resp = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header(SIGNATURE_HEADER, signature)
            .body(raw)
            .send()
            .await
            .map_err(|e| GameError::FulfillmentFailed(format!("game backend unreachable: {e}")))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(GameError::FulfillmentFailed(format!(
                "game backend rejected delivery with HTTP {}",
                resp.status().as_u16()
            )))
        }
    }
}
