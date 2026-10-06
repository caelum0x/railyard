//! Vector stores: in-memory and pgvector.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::{cosine, CacheError};

/// One stored answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry {
    pub tenant: String,
    pub scope: String,
    pub embedding: Vec<f32>,
    pub response: Value,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Where answers live. Every call is scoped to one tenant and one scope.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// The most similar live entry `(cosine similarity, response)`.
    async fn nearest(&self, tenant: &str, scope: &str, embedding: &[f32], now_ms: i64) -> Result<Option<(f32, Value)>, CacheError>;
    async fn insert(&self, entry: CacheEntry) -> Result<(), CacheError>;
    /// Drop expired entries; returns how many.
    async fn purge_expired(&self, now_ms: i64) -> Result<u64, CacheError>;
}

/// A bounded in-process store (oldest entries evicted first).
#[derive(Debug)]
pub struct MemoryVectorStore {
    entries: Mutex<Vec<CacheEntry>>,
    capacity: usize,
}

impl MemoryVectorStore {
    pub fn new(capacity: usize) -> Self {
        Self { entries: Mutex::new(Vec::new()), capacity: capacity.max(1) }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<CacheEntry>>, CacheError> {
        self.entries.lock().map_err(|_| CacheError::Store("memory store lock poisoned".into()))
    }
}

#[async_trait]
impl VectorStore for MemoryVectorStore {
    async fn nearest(&self, tenant: &str, scope: &str, embedding: &[f32], now_ms: i64) -> Result<Option<(f32, Value)>, CacheError> {
        let entries = self.lock()?;
        Ok(entries
            .iter()
            .filter(|e| e.tenant == tenant && e.scope == scope && e.expires_at_ms > now_ms)
            .map(|e| (cosine(&e.embedding, embedding), &e.response))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(s, r)| (s, r.clone())))
    }

    async fn insert(&self, entry: CacheEntry) -> Result<(), CacheError> {
        let mut entries = self.lock()?;
        let now = entry.created_at_ms;
        entries.retain(|e| e.expires_at_ms > now);
        if entries.len() >= self.capacity {
            let excess = entries.len() + 1 - self.capacity;
            entries.drain(..excess);
        }
        entries.push(entry);
        Ok(())
    }

    async fn purge_expired(&self, now_ms: i64) -> Result<u64, CacheError> {
        let mut entries = self.lock()?;
        let before = entries.len();
        entries.retain(|e| e.expires_at_ms > now_ms);
        Ok((before - entries.len()) as u64)
    }
}

/// pgvector-backed store (`CREATE EXTENSION vector`). Similarity is pgvector's
/// cosine distance (`<=>`), searched within one tenant and scope.
#[derive(Debug, Clone)]
pub struct PgVectorStore {
    pub pool: PgPool,
}

/// The schema [`PgVectorStore::migrate`] applies (idempotent).
pub const SCHEMA: &str = "
CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS relay_semantic_cache (
    id BIGSERIAL PRIMARY KEY,
    tenant TEXT NOT NULL,
    scope TEXT NOT NULL,
    embedding vector NOT NULL,
    response JSONB NOT NULL,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS relay_semantic_cache_lookup ON relay_semantic_cache (tenant, scope, expires_at_ms);
";

/// pgvector's text form of a vector.
pub fn vector_literal(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| if x.is_finite() { x.to_string() } else { "0".into() }).collect();
    format!("[{}]", parts.join(","))
}

fn store_err(e: sqlx::Error) -> CacheError {
    CacheError::Store(e.to_string())
}

impl PgVectorStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create the extension, table and index if missing.
    pub async fn migrate(&self) -> Result<(), CacheError> {
        sqlx::raw_sql(SCHEMA).execute(&self.pool).await.map_err(store_err)?;
        Ok(())
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn nearest(&self, tenant: &str, scope: &str, embedding: &[f32], now_ms: i64) -> Result<Option<(f32, Value)>, CacheError> {
        let row = sqlx::query(
            "SELECT response, (1 - (embedding <=> $3::vector))::real AS similarity FROM relay_semantic_cache \
             WHERE tenant = $1 AND scope = $2 AND expires_at_ms > $4 AND vector_dims(embedding) = $5 \
             ORDER BY embedding <=> $3::vector LIMIT 1",
        )
        .bind(tenant)
        .bind(scope)
        .bind(vector_literal(embedding))
        .bind(now_ms)
        .bind(i32::try_from(embedding.len()).unwrap_or(i32::MAX))
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(row.map(|r| (r.get::<f32, _>("similarity"), r.get::<Value, _>("response"))))
    }

    async fn insert(&self, entry: CacheEntry) -> Result<(), CacheError> {
        sqlx::query(
            "INSERT INTO relay_semantic_cache (tenant, scope, embedding, response, created_at_ms, expires_at_ms) \
             VALUES ($1, $2, $3::vector, $4, $5, $6)",
        )
        .bind(&entry.tenant)
        .bind(&entry.scope)
        .bind(vector_literal(&entry.embedding))
        .bind(&entry.response)
        .bind(entry.created_at_ms)
        .bind(entry.expires_at_ms)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn purge_expired(&self, now_ms: i64) -> Result<u64, CacheError> {
        let done = sqlx::query("DELETE FROM relay_semantic_cache WHERE expires_at_ms <= $1")
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(tenant: &str, v: Vec<f32>, response: Value, expires: i64) -> CacheEntry {
        CacheEntry { tenant: tenant.into(), scope: "s".into(), embedding: v, response, created_at_ms: 0, expires_at_ms: expires }
    }

    #[tokio::test]
    async fn memory_store_picks_the_nearest_and_evicts_oldest() {
        let s = MemoryVectorStore::new(2);
        s.insert(entry("t", vec![1.0, 0.0], json!("x"), 100)).await.unwrap();
        s.insert(entry("t", vec![0.0, 1.0], json!("y"), 100)).await.unwrap();
        let (sim, r) = s.nearest("t", "s", &[0.1, 0.9], 1).await.unwrap().unwrap();
        assert_eq!(r, json!("y"));
        assert!(sim > 0.9);
        s.insert(entry("t", vec![0.7, 0.7], json!("z"), 100)).await.unwrap();
        // "x" was evicted; the nearest to [1,0] is now "z".
        assert_eq!(s.nearest("t", "s", &[1.0, 0.0], 1).await.unwrap().unwrap().1, json!("z"));
        assert_eq!(s.purge_expired(100).await.unwrap(), 2);
    }

    #[test]
    fn vector_literals_are_pgvector_text() {
        assert_eq!(vector_literal(&[1.0, -0.5, f32::NAN]), "[1,-0.5,0]");
    }

    fn db_url() -> Option<String> {
        std::env::var("RELAY_TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .filter(|v| !v.trim().is_empty())
    }

    /// Live pgvector proof; skipped (reported as passing) without a database URL.
    #[tokio::test]
    async fn pgvector_store_isolates_tenants_and_honours_ttl() {
        let Some(url) = db_url() else { return };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        let store = PgVectorStore::new(pool.clone());
        store.migrate().await.unwrap();
        store.migrate().await.unwrap();
        let tenant = format!("t-{}", crate::now_ms());
        store.insert(entry(&tenant, vec![1.0, 0.0, 0.0], json!({"a": 1}), 1_000)).await.unwrap();
        store.insert(entry(&tenant, vec![0.0, 1.0, 0.0], json!({"a": 2}), 1_000)).await.unwrap();
        let (sim, r) = store.nearest(&tenant, "s", &[0.0, 0.95, 0.05], 10).await.unwrap().unwrap();
        assert_eq!(r, json!({"a": 2}));
        assert!(sim > 0.9, "{sim}");
        assert!(store.nearest("someone-else", "s", &[0.0, 1.0, 0.0], 10).await.unwrap().is_none());
        assert!(store.nearest(&tenant, "other", &[0.0, 1.0, 0.0], 10).await.unwrap().is_none());
        assert!(store.nearest(&tenant, "s", &[0.0, 1.0], 10).await.unwrap().is_none(), "other dimension");
        assert!(store.nearest(&tenant, "s", &[0.0, 1.0, 0.0], 1_000).await.unwrap().is_none(), "expired");
        assert!(store.purge_expired(1_000).await.unwrap() >= 2);
        sqlx::query("DELETE FROM relay_semantic_cache WHERE tenant = $1").bind(&tenant).execute(&pool).await.unwrap();
    }
}
