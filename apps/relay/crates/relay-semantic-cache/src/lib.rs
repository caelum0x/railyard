//! # relay-semantic-cache
//!
//! A response cache keyed by meaning rather than bytes. A chat request is
//! split into a **scope** (a hash of everything that shapes the answer except
//! the conversation: model, sampling parameters, tools, response format) and a
//! **prompt** (the conversation text). The prompt is embedded; a stored answer
//! is reused when it has the same tenant and scope and its prompt embedding is
//! at least `threshold` cosine-similar, and it has not expired.
//!
//! * [`Embedder`]: [`LocalEmbedder`] (deterministic hashed n-grams, no network,
//!   no model download) or [`HttpEmbedder`] (any OpenAI-compatible
//!   `/embeddings` route, Relay's own included).
//! * [`VectorStore`]: [`MemoryVectorStore`] or [`PgVectorStore`] (pgvector on
//!   Scribase or any Postgres with the `vector` extension).
//! * Tenancy: every read and write carries the tenant; nothing crosses it.
//! * TTL: every entry expires; expired entries never match and are purged.
//!
//! [`middleware`] is the axum layer the gateway mounts.

pub mod embed;
pub mod middleware;
pub mod request;
pub mod setup;
pub mod store;

use std::sync::Arc;

use serde_json::Value;

pub use embed::{Embedder, HttpEmbedder, LocalEmbedder};
pub use request::{cacheable, CacheableRequest};
pub use store::{CacheEntry, MemoryVectorStore, PgVectorStore, VectorStore};

/// Why a cache step failed (the middleware fails open on any of these).
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("embedding failed: {0}")]
    Embedding(String),
    #[error("vector store failed: {0}")]
    Store(String),
    #[error("invalid semantic cache configuration: {0}")]
    Config(String),
}

/// Tunables.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheConfig {
    /// Minimum cosine similarity for a hit (0..=1).
    pub threshold: f32,
    /// How long an answer may be reused, in milliseconds.
    pub ttl_ms: i64,
    /// Largest request or response body the layer buffers.
    pub max_body_bytes: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { threshold: 0.95, ttl_ms: 3_600_000, max_body_bytes: 2 * 1024 * 1024 }
    }
}

impl CacheConfig {
    /// `RELAY_SEMANTIC_CACHE_THRESHOLD` and `RELAY_SEMANTIC_CACHE_TTL_S` over the defaults.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, CacheError> {
        let mut cfg = Self::default();
        if let Some(raw) = get("RELAY_SEMANTIC_CACHE_THRESHOLD") {
            let t: f32 = raw.trim().parse().map_err(|_| CacheError::Config(format!("threshold {raw:?} is not a number")))?;
            if !(0.5..=1.0).contains(&t) {
                return Err(CacheError::Config("threshold must be between 0.5 and 1.0".into()));
            }
            cfg.threshold = t;
        }
        if let Some(raw) = get("RELAY_SEMANTIC_CACHE_TTL_S") {
            let s: i64 = raw.trim().parse().map_err(|_| CacheError::Config(format!("ttl {raw:?} is not a whole number of seconds")))?;
            if s <= 0 {
                return Err(CacheError::Config("ttl must be positive".into()));
            }
            cfg.ttl_ms = s.saturating_mul(1000);
        }
        Ok(cfg)
    }
}

/// A reused answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub response: Value,
    pub similarity: f32,
}

/// The cache: an embedder, a store and the tunables.
#[derive(Clone)]
pub struct SemanticCache {
    pub embedder: Arc<dyn Embedder>,
    pub store: Arc<dyn VectorStore>,
    pub config: CacheConfig,
}

impl SemanticCache {
    pub fn new(embedder: Arc<dyn Embedder>, store: Arc<dyn VectorStore>, config: CacheConfig) -> Self {
        Self { embedder, store, config }
    }

    /// Embed a prompt once (reused for the lookup and the later insert).
    pub async fn embed(&self, prompt: &str) -> Result<Vec<f32>, CacheError> {
        let v = self.embedder.embed(prompt).await?;
        Ok(normalize(v))
    }

    /// The closest live answer of this tenant and scope, if similar enough.
    pub async fn lookup(&self, tenant: &str, scope: &str, embedding: &[f32], now_ms: i64) -> Result<Option<Hit>, CacheError> {
        let best = self.store.nearest(tenant, scope, embedding, now_ms).await?;
        Ok(best.filter(|(sim, _)| *sim >= self.config.threshold).map(|(similarity, response)| Hit { response, similarity }))
    }

    /// Store an answer for `ttl_ms` from `now_ms`.
    pub async fn insert(&self, tenant: &str, scope: &str, embedding: Vec<f32>, response: Value, now_ms: i64) -> Result<(), CacheError> {
        self.store
            .insert(CacheEntry {
                tenant: tenant.to_string(),
                scope: scope.to_string(),
                embedding,
                response,
                created_at_ms: now_ms,
                expires_at_ms: now_ms.saturating_add(self.config.ttl_ms),
            })
            .await
    }
}

/// Unit-length copy of a vector (a zero vector stays zero).
pub fn normalize(v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return v;
    }
    v.into_iter().map(|x| x / norm).collect()
}

/// Cosine similarity of two vectors (0 when the lengths differ or one is zero).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache(threshold: f32) -> SemanticCache {
        SemanticCache::new(
            Arc::new(LocalEmbedder::default()),
            Arc::new(MemoryVectorStore::new(1000)),
            CacheConfig { threshold, ttl_ms: 1000, ..CacheConfig::default() },
        )
    }

    #[tokio::test]
    async fn a_reworded_question_hits_and_an_unrelated_one_misses() {
        let c = cache(0.8);
        let e = c.embed("What is the capital city of France?").await.unwrap();
        c.insert("org/ws", "s", e, json!({"answer": "Paris"}), 0).await.unwrap();
        let near = c.embed("what is the capital city of france").await.unwrap();
        let hit = c.lookup("org/ws", "s", &near, 10).await.unwrap().expect("reworded question hits");
        assert_eq!(hit.response["answer"], "Paris");
        assert!(hit.similarity >= 0.8);
        let far = c.embed("Write a haiku about database indexes").await.unwrap();
        assert!(c.lookup("org/ws", "s", &far, 10).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn tenants_scopes_and_ttl_isolate_entries() {
        let c = cache(0.9);
        let e = c.embed("hello there").await.unwrap();
        c.insert("a", "s", e.clone(), json!(1), 0).await.unwrap();
        assert!(c.lookup("b", "s", &e, 1).await.unwrap().is_none(), "other tenant");
        assert!(c.lookup("a", "t", &e, 1).await.unwrap().is_none(), "other scope");
        assert!(c.lookup("a", "s", &e, 999).await.unwrap().is_some());
        assert!(c.lookup("a", "s", &e, 1000).await.unwrap().is_none(), "expired at ttl");
    }

    #[test]
    fn config_reads_and_validates_the_environment() {
        let cfg = CacheConfig::from_lookup(|k| match k {
            "RELAY_SEMANTIC_CACHE_THRESHOLD" => Some("0.9".into()),
            "RELAY_SEMANTIC_CACHE_TTL_S" => Some("60".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!((cfg.threshold, cfg.ttl_ms), (0.9, 60_000));
        assert!(CacheConfig::from_lookup(|k| (k == "RELAY_SEMANTIC_CACHE_THRESHOLD").then(|| "0.1".into())).is_err());
        assert!(CacheConfig::from_lookup(|k| (k == "RELAY_SEMANTIC_CACHE_TTL_S").then(|| "-1".into())).is_err());
    }

    #[test]
    fn cosine_handles_edges() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), 1.0);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }
}
