//! Seller-own payments storage (Wave P PY2.3): connected sellers (their
//! credentials sealed), their products, one-time connect tokens, and the
//! entitlement events forwarded to the calling product with an outbox. Like
//! the portfolio mirror this sits beside the ledger and never in it: no
//! balance, no hold, no payout.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use mint_core::MintError;

/// A connected seller of one calling product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SellerRecord {
    pub product: String,
    pub seller: String,
    /// `dodo` or `stripe`.
    pub provider: String,
    /// `test` or `live`.
    pub mode: String,
    /// The seller's credential, sealed (`mint-payments::seller::seal`).
    pub sealed_secret: String,
    pub sealed_webhook_secret: Option<String>,
    /// Stripe: the connected account id (not a secret), to route Connect events.
    pub account_ref: Option<String>,
    pub connected_at_ms: i64,
}

/// A product created on the seller's own account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SellerProductRecord {
    pub product: String,
    pub seller: String,
    pub sku: String,
    pub name: String,
    pub price_cents: i64,
    pub currency: String,
    /// `month`, `year`, or `None` for one-time.
    pub interval: Option<String>,
    /// The processor's product (Dodo) or price (Stripe) id.
    pub provider_ref: String,
    pub created_at_ms: i64,
}

/// A one-time connect link (stored as a hash of its token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectToken {
    pub token_hash: String,
    pub product: String,
    pub seller: String,
    pub return_url: Option<String>,
    pub expires_at_ms: i64,
}

/// One entitlement change forwarded to the calling product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementRecord {
    /// Mint's id (`ent_<ulid>`), also the delivery's webhook id.
    pub id: String,
    pub product: String,
    pub seller: String,
    pub provider: String,
    pub provider_event_id: String,
    /// `granted` or `revoked`.
    pub kind: String,
    pub reason: Option<String>,
    pub buyer_ref: String,
    pub sku: String,
    pub occurred_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub recorded_at_ms: i64,
}

/// A due delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementDelivery {
    pub record: EntitlementRecord,
    pub attempts: i32,
}

#[async_trait]
pub trait SellerStore: Send + Sync {
    async fn put_token(&self, token: ConnectToken) -> Result<(), MintError>;
    /// Returns the token and marks it used, only if unused and unexpired.
    async fn take_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError>;
    /// Reads a token without using it (the connect page's first view).
    async fn peek_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError>;
    async fn upsert_seller(&self, record: SellerRecord) -> Result<(), MintError>;
    async fn get_seller(&self, product: &str, seller: &str) -> Result<Option<SellerRecord>, MintError>;
    async fn seller_by_account(&self, provider: &str, account_ref: &str) -> Result<Option<SellerRecord>, MintError>;
    /// Inserts; returns the existing record when the sku already exists.
    async fn put_product(&self, record: SellerProductRecord) -> Result<SellerProductRecord, MintError>;
    async fn get_product(&self, product: &str, seller: &str, sku: &str) -> Result<Option<SellerProductRecord>, MintError>;
    async fn list_products(&self, product: &str, seller: &str) -> Result<Vec<SellerProductRecord>, MintError>;
    /// Records an entitlement event and its delivery; `false` for a replay.
    async fn record_entitlement(&self, record: EntitlementRecord) -> Result<bool, MintError>;
    async fn list_entitlements(&self, product: &str, seller: &str, limit: usize) -> Result<Vec<EntitlementRecord>, MintError>;
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<EntitlementDelivery>, MintError>;
    async fn delivered(&self, id: &str, now_ms: i64) -> Result<(), MintError>;
    async fn failed(&self, id: &str, next_ms: i64, error: &str) -> Result<(), MintError>;
}

#[derive(Default)]
struct Inner {
    tokens: BTreeMap<String, (ConnectToken, bool)>,
    sellers: BTreeMap<(String, String), SellerRecord>,
    products: BTreeMap<(String, String, String), SellerProductRecord>,
    events: Vec<EntitlementRecord>,
    /// id -> (attempts, next ms, delivered ms, last error)
    deliveries: BTreeMap<String, (i32, i64, Option<i64>, Option<String>)>,
}

/// In-memory seller store (tests and `MINT_ALLOW_INMEMORY`).
#[derive(Default)]
pub struct MemorySellerStore {
    inner: Mutex<Inner>,
}

impl MemorySellerStore {
    /// Delivered timestamp of one entitlement event (tests).
    pub fn delivered_at(&self, id: &str) -> Option<i64> {
        self.inner.lock().ok()?.deliveries.get(id).and_then(|d| d.2)
    }
}

fn lock(inner: &Mutex<Inner>) -> Result<std::sync::MutexGuard<'_, Inner>, MintError> {
    inner.lock().map_err(|_| MintError::Storage)
}

#[async_trait]
impl SellerStore for MemorySellerStore {
    async fn put_token(&self, token: ConnectToken) -> Result<(), MintError> {
        lock(&self.inner)?.tokens.insert(token.token_hash.clone(), (token, false));
        Ok(())
    }
    async fn take_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError> {
        let mut inner = lock(&self.inner)?;
        match inner.tokens.get_mut(token_hash) {
            Some((token, used)) if !*used && token.expires_at_ms > now_ms => {
                *used = true;
                Ok(Some(token.clone()))
            }
            _ => Ok(None),
        }
    }
    async fn peek_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError> {
        let inner = lock(&self.inner)?;
        Ok(inner.tokens.get(token_hash).filter(|(t, used)| !*used && t.expires_at_ms > now_ms).map(|(t, _)| t.clone()))
    }
    async fn upsert_seller(&self, record: SellerRecord) -> Result<(), MintError> {
        lock(&self.inner)?.sellers.insert((record.product.clone(), record.seller.clone()), record);
        Ok(())
    }
    async fn get_seller(&self, product: &str, seller: &str) -> Result<Option<SellerRecord>, MintError> {
        Ok(lock(&self.inner)?.sellers.get(&(product.to_owned(), seller.to_owned())).cloned())
    }
    async fn seller_by_account(&self, provider: &str, account_ref: &str) -> Result<Option<SellerRecord>, MintError> {
        Ok(lock(&self.inner)?.sellers.values().find(|s| s.provider == provider && s.account_ref.as_deref() == Some(account_ref)).cloned())
    }
    async fn put_product(&self, record: SellerProductRecord) -> Result<SellerProductRecord, MintError> {
        let mut inner = lock(&self.inner)?;
        let key = (record.product.clone(), record.seller.clone(), record.sku.clone());
        Ok(inner.products.entry(key).or_insert(record).clone())
    }
    async fn get_product(&self, product: &str, seller: &str, sku: &str) -> Result<Option<SellerProductRecord>, MintError> {
        Ok(lock(&self.inner)?.products.get(&(product.to_owned(), seller.to_owned(), sku.to_owned())).cloned())
    }
    async fn list_products(&self, product: &str, seller: &str) -> Result<Vec<SellerProductRecord>, MintError> {
        Ok(lock(&self.inner)?.products.values().filter(|p| p.product == product && p.seller == seller).cloned().collect())
    }
    async fn record_entitlement(&self, record: EntitlementRecord) -> Result<bool, MintError> {
        let mut inner = lock(&self.inner)?;
        if inner.events.iter().any(|e| e.provider == record.provider && e.provider_event_id == record.provider_event_id) {
            return Ok(false);
        }
        inner.deliveries.insert(record.id.clone(), (0, record.recorded_at_ms, None, None));
        inner.events.push(record);
        Ok(true)
    }
    async fn list_entitlements(&self, product: &str, seller: &str, limit: usize) -> Result<Vec<EntitlementRecord>, MintError> {
        let inner = lock(&self.inner)?;
        let mut out: Vec<EntitlementRecord> = inner.events.iter().filter(|e| e.product == product && e.seller == seller).cloned().collect();
        out.sort_by(|a, b| b.recorded_at_ms.cmp(&a.recorded_at_ms).then(b.id.cmp(&a.id)));
        out.truncate(limit);
        Ok(out)
    }
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<EntitlementDelivery>, MintError> {
        let inner = lock(&self.inner)?;
        let mut out: Vec<EntitlementDelivery> = inner
            .deliveries
            .iter()
            .filter(|(_, d)| d.2.is_none() && d.1 <= now_ms)
            .filter_map(|(id, d)| inner.events.iter().find(|e| &e.id == id).map(|e| EntitlementDelivery { record: e.clone(), attempts: d.0 }))
            .collect();
        out.sort_by_key(|d| (d.record.occurred_at_ms, d.record.recorded_at_ms));
        out.truncate(limit);
        Ok(out)
    }
    async fn delivered(&self, id: &str, now_ms: i64) -> Result<(), MintError> {
        if let Some(d) = lock(&self.inner)?.deliveries.get_mut(id) {
            d.0 += 1;
            d.2 = Some(now_ms);
            d.3 = None;
        }
        Ok(())
    }
    async fn failed(&self, id: &str, next_ms: i64, error: &str) -> Result<(), MintError> {
        if let Some(d) = lock(&self.inner)?.deliveries.get_mut(id) {
            d.0 += 1;
            d.1 = next_ms;
            d.3 = Some(error.chars().take(300).collect());
        }
        Ok(())
    }
}
