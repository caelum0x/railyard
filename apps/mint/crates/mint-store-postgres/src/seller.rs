//! Postgres [`SellerStore`] (Wave P PY2.3), migration 0014. Runtime queries
//! with `.bind` only.

use async_trait::async_trait;
use mint_core::MintError;
use mint_store::{ConnectToken, EntitlementDelivery, EntitlementRecord, SellerProductRecord, SellerRecord, SellerStore};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

pub struct PgSeller {
    pool: PgPool,
}

impl PgSeller {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(error: sqlx::Error) -> MintError {
    tracing::error!(%error, "mint-store-postgres: seller query failed");
    MintError::Storage
}

const TOKEN_COLS: &str = "token_hash, product, seller, return_url, expires_at_ms";
const SELLER_COLS: &str = "product, seller, provider, mode, sealed_secret, sealed_webhook_secret, account_ref, connected_at_ms";
const PRODUCT_COLS: &str = "product, seller, sku, name, price_cents, currency, interval, provider_ref, created_at_ms";
const ENT_COLS: &str = "id, product, seller, provider, provider_event_id, kind, reason, buyer_ref, sku, occurred_at_ms, expires_at_ms, recorded_at_ms";

fn token(row: &PgRow) -> Result<ConnectToken, MintError> {
    Ok(ConnectToken {
        token_hash: row.try_get("token_hash").map_err(storage)?,
        product: row.try_get("product").map_err(storage)?,
        seller: row.try_get("seller").map_err(storage)?,
        return_url: row.try_get("return_url").map_err(storage)?,
        expires_at_ms: row.try_get("expires_at_ms").map_err(storage)?,
    })
}

fn seller(row: &PgRow) -> Result<SellerRecord, MintError> {
    Ok(SellerRecord {
        product: row.try_get("product").map_err(storage)?,
        seller: row.try_get("seller").map_err(storage)?,
        provider: row.try_get("provider").map_err(storage)?,
        mode: row.try_get("mode").map_err(storage)?,
        sealed_secret: row.try_get("sealed_secret").map_err(storage)?,
        sealed_webhook_secret: row.try_get("sealed_webhook_secret").map_err(storage)?,
        account_ref: row.try_get("account_ref").map_err(storage)?,
        connected_at_ms: row.try_get("connected_at_ms").map_err(storage)?,
    })
}

fn product(row: &PgRow) -> Result<SellerProductRecord, MintError> {
    Ok(SellerProductRecord {
        product: row.try_get("product").map_err(storage)?,
        seller: row.try_get("seller").map_err(storage)?,
        sku: row.try_get("sku").map_err(storage)?,
        name: row.try_get("name").map_err(storage)?,
        price_cents: row.try_get("price_cents").map_err(storage)?,
        currency: row.try_get("currency").map_err(storage)?,
        interval: row.try_get("interval").map_err(storage)?,
        provider_ref: row.try_get("provider_ref").map_err(storage)?,
        created_at_ms: row.try_get("created_at_ms").map_err(storage)?,
    })
}

fn entitlement(row: &PgRow) -> Result<EntitlementRecord, MintError> {
    Ok(EntitlementRecord {
        id: row.try_get("id").map_err(storage)?,
        product: row.try_get("product").map_err(storage)?,
        seller: row.try_get("seller").map_err(storage)?,
        provider: row.try_get("provider").map_err(storage)?,
        provider_event_id: row.try_get("provider_event_id").map_err(storage)?,
        kind: row.try_get("kind").map_err(storage)?,
        reason: row.try_get("reason").map_err(storage)?,
        buyer_ref: row.try_get("buyer_ref").map_err(storage)?,
        sku: row.try_get("sku").map_err(storage)?,
        occurred_at_ms: row.try_get("occurred_at_ms").map_err(storage)?,
        expires_at_ms: row.try_get("expires_at_ms").map_err(storage)?,
        recorded_at_ms: row.try_get("recorded_at_ms").map_err(storage)?,
    })
}

#[async_trait]
impl SellerStore for PgSeller {
    async fn put_token(&self, t: ConnectToken) -> Result<(), MintError> {
        sqlx::query("INSERT INTO mint_seller_connect_tokens (token_hash, product, seller, return_url, expires_at_ms) VALUES ($1, $2, $3, $4, $5)")
            .bind(&t.token_hash)
            .bind(&t.product)
            .bind(&t.seller)
            .bind(&t.return_url)
            .bind(t.expires_at_ms)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn take_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError> {
        let row = sqlx::query(&format!(
            "UPDATE mint_seller_connect_tokens SET used_at_ms = $2 \
             WHERE token_hash = $1 AND used_at_ms IS NULL AND expires_at_ms > $2 RETURNING {TOKEN_COLS}"
        ))
        .bind(token_hash)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.as_ref().map(token).transpose()
    }

    async fn peek_token(&self, token_hash: &str, now_ms: i64) -> Result<Option<ConnectToken>, MintError> {
        let row = sqlx::query(&format!(
            "SELECT {TOKEN_COLS} FROM mint_seller_connect_tokens WHERE token_hash = $1 AND used_at_ms IS NULL AND expires_at_ms > $2"
        ))
        .bind(token_hash)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.as_ref().map(token).transpose()
    }

    async fn upsert_seller(&self, r: SellerRecord) -> Result<(), MintError> {
        sqlx::query(&format!(
            "INSERT INTO mint_seller_accounts ({SELLER_COLS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (product, seller) DO UPDATE SET provider = excluded.provider, mode = excluded.mode, \
               sealed_secret = excluded.sealed_secret, sealed_webhook_secret = excluded.sealed_webhook_secret, \
               account_ref = excluded.account_ref, connected_at_ms = excluded.connected_at_ms"
        ))
        .bind(&r.product)
        .bind(&r.seller)
        .bind(&r.provider)
        .bind(&r.mode)
        .bind(&r.sealed_secret)
        .bind(&r.sealed_webhook_secret)
        .bind(&r.account_ref)
        .bind(r.connected_at_ms)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn get_seller(&self, p: &str, s: &str) -> Result<Option<SellerRecord>, MintError> {
        let row = sqlx::query(&format!("SELECT {SELLER_COLS} FROM mint_seller_accounts WHERE product = $1 AND seller = $2"))
            .bind(p)
            .bind(s)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(seller).transpose()
    }

    async fn seller_by_account(&self, provider: &str, account_ref: &str) -> Result<Option<SellerRecord>, MintError> {
        let row = sqlx::query(&format!("SELECT {SELLER_COLS} FROM mint_seller_accounts WHERE provider = $1 AND account_ref = $2"))
            .bind(provider)
            .bind(account_ref)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(seller).transpose()
    }

    async fn put_product(&self, r: SellerProductRecord) -> Result<SellerProductRecord, MintError> {
        sqlx::query(&format!(
            "INSERT INTO mint_seller_products ({PRODUCT_COLS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (product, seller, sku) DO NOTHING"
        ))
        .bind(&r.product)
        .bind(&r.seller)
        .bind(&r.sku)
        .bind(&r.name)
        .bind(r.price_cents)
        .bind(&r.currency)
        .bind(&r.interval)
        .bind(&r.provider_ref)
        .bind(r.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        self.get_product(&r.product, &r.seller, &r.sku).await?.ok_or(MintError::Storage)
    }

    async fn get_product(&self, p: &str, s: &str, sku: &str) -> Result<Option<SellerProductRecord>, MintError> {
        let row = sqlx::query(&format!("SELECT {PRODUCT_COLS} FROM mint_seller_products WHERE product = $1 AND seller = $2 AND sku = $3"))
            .bind(p)
            .bind(s)
            .bind(sku)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(product).transpose()
    }

    async fn list_products(&self, p: &str, s: &str) -> Result<Vec<SellerProductRecord>, MintError> {
        let rows = sqlx::query(&format!("SELECT {PRODUCT_COLS} FROM mint_seller_products WHERE product = $1 AND seller = $2 ORDER BY sku"))
            .bind(p)
            .bind(s)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(product).collect()
    }

    async fn record_entitlement(&self, r: EntitlementRecord) -> Result<bool, MintError> {
        let inserted = sqlx::query(&format!(
            "INSERT INTO mint_seller_entitlements ({ENT_COLS}, next_attempt_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12) \
             ON CONFLICT (provider, provider_event_id) DO NOTHING RETURNING id"
        ))
        .bind(&r.id)
        .bind(&r.product)
        .bind(&r.seller)
        .bind(&r.provider)
        .bind(&r.provider_event_id)
        .bind(&r.kind)
        .bind(&r.reason)
        .bind(&r.buyer_ref)
        .bind(&r.sku)
        .bind(r.occurred_at_ms)
        .bind(r.expires_at_ms)
        .bind(r.recorded_at_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(inserted.is_some())
    }

    async fn list_entitlements(&self, p: &str, s: &str, limit: usize) -> Result<Vec<EntitlementRecord>, MintError> {
        let rows = sqlx::query(&format!(
            "SELECT {ENT_COLS} FROM mint_seller_entitlements WHERE product = $1 AND seller = $2 ORDER BY recorded_at_ms DESC, id DESC LIMIT $3"
        ))
        .bind(p)
        .bind(s)
        .bind(i64::try_from(limit).unwrap_or(500))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(entitlement).collect()
    }

    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<EntitlementDelivery>, MintError> {
        let rows = sqlx::query(&format!(
            "SELECT {ENT_COLS}, attempts FROM mint_seller_entitlements \
             WHERE delivered_ms IS NULL AND next_attempt_ms <= $1 ORDER BY occurred_at_ms, recorded_at_ms LIMIT $2"
        ))
        .bind(now_ms)
        .bind(i64::try_from(limit).unwrap_or(100))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| Ok(EntitlementDelivery { record: entitlement(row)?, attempts: row.try_get("attempts").map_err(storage)? }))
            .collect()
    }

    async fn delivered(&self, id: &str, now_ms: i64) -> Result<(), MintError> {
        sqlx::query("UPDATE mint_seller_entitlements SET attempts = attempts + 1, delivered_ms = $2, last_error = NULL WHERE id = $1")
            .bind(id)
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn failed(&self, id: &str, next_ms: i64, error: &str) -> Result<(), MintError> {
        sqlx::query("UPDATE mint_seller_entitlements SET attempts = attempts + 1, next_attempt_ms = $2, last_error = $3 WHERE id = $1")
            .bind(id)
            .bind(next_ms)
            .bind(error.chars().take(300).collect::<String>())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }
}
