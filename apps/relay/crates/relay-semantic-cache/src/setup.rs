//! Building the layer from the environment.
//!
//! * `RELAY_SEMANTIC_CACHE`: `off` (default), `memory`, or `pgvector` (uses the
//!   gateway's Postgres; the `vector` extension must be available).
//! * `RELAY_SEMANTIC_CACHE_EMBEDDINGS_URL` (+ `_MODEL`, `_API_KEY`): an
//!   OpenAI-compatible embeddings route, e.g. Relay's own
//!   `http://localhost:3000/openai/v1/embeddings`; without it the local
//!   hashed embedder is used.
//! * `RELAY_SEMANTIC_CACHE_THRESHOLD`, `RELAY_SEMANTIC_CACHE_TTL_S`,
//!   `RELAY_SEMANTIC_CACHE_MEMORY_ENTRIES`.

use std::sync::Arc;

use sqlx::PgPool;

use crate::middleware::{SemanticCacheState, TenantFn};
use crate::{CacheConfig, CacheError, Embedder, HttpEmbedder, LocalEmbedder, MemoryVectorStore, PgVectorStore, SemanticCache, VectorStore};

/// The default model asked of an embeddings route.
pub const DEFAULT_EMBEDDINGS_MODEL: &str = "text-embedding-3-small";
/// The default in-memory capacity.
pub const DEFAULT_MEMORY_ENTRIES: usize = 10_000;

/// Which store the cache uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Memory,
    PgVector,
}

/// The parsed settings (no I/O).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub store: StoreKind,
    pub config: CacheConfig,
    pub embeddings_url: Option<String>,
    pub embeddings_model: String,
    pub embeddings_api_key: Option<String>,
    pub memory_entries: usize,
}

impl Settings {
    /// `None` when the cache is off.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, CacheError> {
        let get = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let store = match get("RELAY_SEMANTIC_CACHE").as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("off" | "false" | "0") => return Ok(None),
            Some("memory" | "on" | "true" | "1") => StoreKind::Memory,
            Some("pgvector" | "postgres") => StoreKind::PgVector,
            Some(other) => return Err(CacheError::Config(format!("RELAY_SEMANTIC_CACHE={other:?}: use off, memory or pgvector"))),
        };
        let memory_entries = match get("RELAY_SEMANTIC_CACHE_MEMORY_ENTRIES") {
            Some(raw) => raw.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| CacheError::Config("RELAY_SEMANTIC_CACHE_MEMORY_ENTRIES must be a positive number".into()))?,
            None => DEFAULT_MEMORY_ENTRIES,
        };
        let embeddings_url = get("RELAY_SEMANTIC_CACHE_EMBEDDINGS_URL");
        if let Some(url) = &embeddings_url {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(CacheError::Config("RELAY_SEMANTIC_CACHE_EMBEDDINGS_URL must be an http(s) URL".into()));
            }
        }
        Ok(Some(Self {
            store,
            config: CacheConfig::from_lookup(&get)?,
            embeddings_url,
            embeddings_model: get("RELAY_SEMANTIC_CACHE_EMBEDDINGS_MODEL").unwrap_or_else(|| DEFAULT_EMBEDDINGS_MODEL.into()),
            embeddings_api_key: get("RELAY_SEMANTIC_CACHE_EMBEDDINGS_API_KEY"),
            memory_entries,
        }))
    }
}

/// Build the layer state, migrating pgvector when chosen.
pub async fn build(settings: Settings, pool: Option<PgPool>, tenant: TenantFn) -> Result<SemanticCacheState, CacheError> {
    let embedder: Arc<dyn Embedder> = match &settings.embeddings_url {
        Some(url) => Arc::new(HttpEmbedder::new(url.clone(), settings.embeddings_model.clone(), settings.embeddings_api_key.clone())),
        None => Arc::new(LocalEmbedder::default()),
    };
    let store: Arc<dyn VectorStore> = match settings.store {
        StoreKind::Memory => Arc::new(MemoryVectorStore::new(settings.memory_entries)),
        StoreKind::PgVector => {
            let pool = pool.ok_or_else(|| CacheError::Config("RELAY_SEMANTIC_CACHE=pgvector needs the gateway's Postgres".into()))?;
            let store = PgVectorStore::new(pool);
            store.migrate().await?;
            Arc::new(store)
        }
    };
    Ok(SemanticCacheState { cache: Arc::new(SemanticCache::new(embedder, store, settings.config)), tenant })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_string())
    }

    #[test]
    fn off_by_default_and_parsed_when_on() {
        assert_eq!(Settings::from_lookup(env(&[])).unwrap(), None);
        assert_eq!(Settings::from_lookup(env(&[("RELAY_SEMANTIC_CACHE", "off")])).unwrap(), None);
        let s = Settings::from_lookup(env(&[
            ("RELAY_SEMANTIC_CACHE", "pgvector"),
            ("RELAY_SEMANTIC_CACHE_EMBEDDINGS_URL", "http://localhost:3000/openai/v1/embeddings"),
            ("RELAY_SEMANTIC_CACHE_TTL_S", "120"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(s.store, StoreKind::PgVector);
        assert_eq!(s.config.ttl_ms, 120_000);
        assert_eq!(s.embeddings_model, DEFAULT_EMBEDDINGS_MODEL);
        assert!(Settings::from_lookup(env(&[("RELAY_SEMANTIC_CACHE", "redis")])).is_err());
        assert!(Settings::from_lookup(env(&[("RELAY_SEMANTIC_CACHE", "memory"), ("RELAY_SEMANTIC_CACHE_EMBEDDINGS_URL", "file:///x")])).is_err());
    }

    #[tokio::test]
    async fn pgvector_without_a_pool_is_a_config_error() {
        let s = Settings::from_lookup(env(&[("RELAY_SEMANTIC_CACHE", "pgvector")])).unwrap().unwrap();
        let tenant: TenantFn = Arc::new(|_| None);
        assert!(matches!(build(s, None, tenant).await, Err(CacheError::Config(_))));
        let m = Settings::from_lookup(env(&[("RELAY_SEMANTIC_CACHE", "memory")])).unwrap().unwrap();
        assert!(build(m, None, Arc::new(|_| None)).await.is_ok());
    }
}
