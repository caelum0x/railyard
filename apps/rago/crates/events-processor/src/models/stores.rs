//! Port of `models/stores.go` — the `ApiStore` (Postgres), the `FlagStore`
//! (Redis sorted-set flag) and the `CacheStore` (Redis usage-cache expirer),
//! plus the `Flagger` / `Cacher` traits.

use async_trait::async_trait;
use redis::aio::ConnectionManager;

use crate::config::database::Database;
use crate::config::redis::RedisDb;
use crate::utils::result::Result;

/// `EXPIRATION_TIME` — 10 seconds.
pub const EXPIRATION_TIME_SECS: i64 = 10;
/// `SUBSCRIPTION_BUCKET_DURATION` — 10 seconds.
pub const SUBSCRIPTION_BUCKET_DURATION: i64 = 10;

/// `ApiStore` — Postgres-backed reads. Query methods live in `models::queries`.
#[derive(Clone)]
pub struct ApiStore {
    pub(crate) db: Database,
}

impl ApiStore {
    pub fn new(db: Database) -> ApiStore {
        ApiStore { db }
    }
}

/// `Flagger` interface.
#[async_trait]
pub trait Flagger: Send + Sync {
    async fn flag(&self, value: &str) -> anyhow::Result<()>;
}

/// `FlagStore` — adds a subscription to a Redis sorted set for delayed refresh.
pub struct FlagStore {
    name: String,
    conn: ConnectionManager,
}

impl FlagStore {
    pub fn new(redis: &RedisDb, name: impl Into<String>) -> FlagStore {
        FlagStore {
            name: name.into(),
            conn: redis.conn(),
        }
    }
}

#[async_trait]
impl Flagger for FlagStore {
    /// `(*FlagStore).Flag` — ZADD with a time-bucketed member so events in the
    /// same window share a member and the latest score wins.
    async fn flag(&self, value: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let bucket = (now / SUBSCRIPTION_BUCKET_DURATION) * SUBSCRIPTION_BUCKET_DURATION;
        let member = format!("{value}|{bucket}");

        let mut conn = self.conn.clone();
        redis::cmd("ZADD")
            .arg(&self.name)
            .arg(now as f64)
            .arg(member)
            .query_async::<()>(&mut conn)
            .await?;
        Ok(())
    }
}

/// `Cacher` interface.
#[async_trait]
pub trait Cacher: Send + Sync {
    async fn expire_key(&self, key: &str) -> Result<bool>;
}

/// `CacheStore` — schedules removal of a usage-cache key via EXPIRE.
#[derive(Clone)]
pub struct CacheStore {
    conn: ConnectionManager,
}

impl CacheStore {
    pub fn new(redis: &RedisDb) -> CacheStore {
        CacheStore { conn: redis.conn() }
    }
}

#[async_trait]
impl Cacher for CacheStore {
    /// `(*CacheStore).ExpireKey` — uses EXPIRE (not DEL) to account for
    /// ClickHouse propagation time.
    async fn expire_key(&self, key: &str) -> Result<bool> {
        let mut conn = self.conn.clone();
        let res = redis::cmd("EXPIRE")
            .arg(key)
            .arg(EXPIRATION_TIME_SECS)
            .query_async::<i64>(&mut conn)
            .await;
        match res {
            Ok(_) => Result::success(true),
            Err(e) => Result::failed(e),
        }
    }
}
