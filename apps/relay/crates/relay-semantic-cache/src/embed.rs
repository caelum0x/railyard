//! Prompt embeddings.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::CacheError;

/// Text to vector.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, CacheError>;
}

/// A local, deterministic embedder: signed feature hashing of word unigrams,
/// word bigrams and character trigrams into `dims` buckets. No network and no
/// model download; it captures lexical closeness (rewording, casing,
/// punctuation, word order), not deep paraphrase. Use [`HttpEmbedder`] for a
/// learned model.
#[derive(Debug, Clone)]
pub struct LocalEmbedder {
    pub dims: usize,
}

impl Default for LocalEmbedder {
    fn default() -> Self {
        Self { dims: 512 }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

impl LocalEmbedder {
    fn add(&self, v: &mut [f32], feature: &str, weight: f32) {
        let h = fnv1a(feature.as_bytes());
        let idx = usize::try_from(h % self.dims as u64).unwrap_or(0);
        let sign = if (h >> 63) == 1 { -1.0 } else { 1.0 };
        v[idx] += sign * weight;
    }

    /// The embedding, synchronously.
    pub fn vector(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; self.dims.max(1)];
        let lower = text.to_lowercase();
        let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        for w in &words {
            self.add(&mut v, &format!("w:{w}"), 1.0);
            let chars: Vec<char> = format!("^{w}$").chars().collect();
            for tri in chars.windows(3) {
                self.add(&mut v, &format!("c:{}", tri.iter().collect::<String>()), 0.3);
            }
        }
        for pair in words.windows(2) {
            self.add(&mut v, &format!("b:{} {}", pair[0], pair[1]), 0.5);
        }
        v
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, CacheError> {
        Ok(self.vector(text))
    }
}

/// An OpenAI-compatible `/embeddings` endpoint: Relay's own
/// (`<relay>/openai/v1/embeddings`, routed through Relay's providers and
/// billing like any request) or a self-hosted model server.
#[derive(Clone)]
pub struct HttpEmbedder {
    pub client: reqwest::Client,
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder").field("url", &self.url).field("model", &self.model).finish_non_exhaustive()
    }
}

impl HttpEmbedder {
    pub fn new(url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self { client: reqwest::Client::new(), url: url.into(), model: model.into(), api_key }
    }
}

#[async_trait]
impl Embedder for HttpEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, CacheError> {
        let mut req = self.client.post(&self.url).json(&json!({ "model": self.model, "input": text }));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| CacheError::Embedding(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CacheError::Embedding(format!("embeddings endpoint answered {status}")));
        }
        let body: Value = resp.json().await.map_err(|e| CacheError::Embedding(e.to_string()))?;
        let vector: Vec<f32> = body
            .pointer("/data/0/embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| CacheError::Embedding("no data[0].embedding in the response".into()))?
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<_>>()
            .ok_or_else(|| CacheError::Embedding("embedding holds a non-number".into()))?;
        if vector.is_empty() {
            return Err(CacheError::Embedding("empty embedding".into()));
        }
        Ok(vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosine;

    #[test]
    fn local_embeddings_are_deterministic_and_lexically_close() {
        let e = LocalEmbedder::default();
        assert_eq!(e.vector("Hello world"), e.vector("hello, WORLD!"));
        let a = e.vector("how do I reset my password");
        let b = e.vector("how can I reset my password please");
        let c = e.vector("quarterly revenue by region chart");
        assert!(cosine(&a, &b) > 0.7, "{}", cosine(&a, &b));
        assert!(cosine(&a, &c) < 0.3, "{}", cosine(&a, &c));
    }

    #[tokio::test]
    async fn http_embedder_reads_openai_shape_and_sends_the_key() {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/v1/embeddings",
            post(|headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                axum::Json(json!({ "data": [{ "embedding": [0.5, 0.25, body["input"].as_str().map_or(0.0, |s| s.len() as f64)] }], "auth": auth }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        let e = HttpEmbedder::new(format!("http://{addr}/v1/embeddings"), "text-embedding-3-small", Some("k".into()));
        assert_eq!(e.embed("abcd").await.unwrap(), vec![0.5, 0.25, 4.0]);
        let bad = HttpEmbedder::new(format!("http://{addr}/nope"), "m", None);
        assert!(bad.embed("x").await.is_err());
    }
}
