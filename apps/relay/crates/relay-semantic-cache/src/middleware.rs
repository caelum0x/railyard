//! The axum layer. On a cacheable chat request it embeds the prompt and, on
//! a hit, answers from the cache without running any inner layer (no hold,
//! no provider call, no charge). On a miss it forwards the request and stores
//! a successful JSON answer. Every failure of the cache itself fails open.
//!
//! Callers opt out per request with `x-relay-cache: bypass` or
//! `cache-control: no-cache` / `no-store`.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::Value;

use crate::{cacheable, now_ms, SemanticCache};

/// Reported on every request the layer considered: `hit`, `miss` or `bypass`.
pub const CACHE_HEADER: &str = "x-relay-cache";
/// The similarity of a hit.
pub const SIMILARITY_HEADER: &str = "x-relay-cache-similarity";

/// Resolves the tenant (e.g. `organization/workspace`) of an authenticated request.
pub type TenantFn = Arc<dyn Fn(&Request) -> Option<String> + Send + Sync>;

/// Shared state of the layer.
#[derive(Clone)]
pub struct SemanticCacheState {
    pub cache: Arc<SemanticCache>,
    pub tenant: TenantFn,
}

fn is_chat_path(path: &str) -> bool {
    path.ends_with("/chat/completions")
}

fn opted_out(req: &Request) -> bool {
    let h = |name: header::HeaderName| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_ascii_lowercase);
    let bypass = req
        .headers()
        .get(CACHE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bypass") || v.eq_ignore_ascii_case("off"));
    let no_cache = h(header::CACHE_CONTROL).is_some_and(|v| v.contains("no-cache") || v.contains("no-store"));
    bypass || no_cache
}

fn tagged(mut resp: Response, value: &'static str) -> Response {
    resp.headers_mut().insert(CACHE_HEADER, HeaderValue::from_static(value));
    resp
}

/// `axum::middleware::from_fn_with_state(state, semantic_cache_middleware)`.
pub async fn semantic_cache_middleware(State(state): State<SemanticCacheState>, req: Request, next: Next) -> Response {
    if req.method() != Method::POST || !is_chat_path(req.uri().path()) {
        return next.run(req).await;
    }
    if opted_out(&req) {
        return tagged(next.run(req).await, "bypass");
    }
    let Some(tenant) = (state.tenant)(&req) else {
        // Unauthenticated deployments have no tenant to isolate by: never share.
        return next.run(req).await;
    };
    let (parts, body) = req.into_parts();
    let bytes: Bytes = match http_body_util::Limited::new(body, state.cache.config.max_body_bytes).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "The request body is larger than this gateway accepts.").into_response();
        }
    };
    let rebuilt = || Request::from_parts(parts.clone(), Body::from(bytes.clone()));
    let Some(key) = serde_json::from_slice::<Value>(&bytes).ok().as_ref().and_then(cacheable) else {
        return next.run(rebuilt()).await;
    };
    let embedding = match state.cache.embed(&key.prompt).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "semantic cache: embedding failed; serving uncached");
            return next.run(rebuilt()).await;
        }
    };
    match state.cache.lookup(&tenant, &key.scope, &embedding, now_ms()).await {
        Ok(Some(hit)) => {
            let mut resp = axum::Json(hit.response).into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("{:.4}", hit.similarity)) {
                resp.headers_mut().insert(SIMILARITY_HEADER, v);
            }
            return tagged(resp, "hit");
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "semantic cache: lookup failed; serving uncached"),
    }
    let resp = next.run(rebuilt()).await;
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    if resp.status() != StatusCode::OK || !is_json {
        return tagged(resp, "miss");
    }
    let (rparts, rbody) = resp.into_parts();
    let rbytes = match http_body_util::Limited::new(rbody, state.cache.config.max_body_bytes).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            tracing::warn!("semantic cache: response too large to buffer");
            return (StatusCode::BAD_GATEWAY, "The upstream response could not be read.").into_response();
        }
    };
    if let Ok(answer) = serde_json::from_slice::<Value>(&rbytes) {
        if let Err(e) = state.cache.insert(&tenant, &key.scope, embedding, answer, now_ms()).await {
            tracing::warn!(error = %e, "semantic cache: store failed");
        }
    }
    tagged(Response::from_parts(rparts, Body::from(rbytes)), "miss")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheConfig, LocalEmbedder, MemoryVectorStore};
    use axum::routing::post;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn app(calls: Arc<AtomicUsize>) -> axum::Router {
        let cache = SemanticCache::new(
            Arc::new(LocalEmbedder::default()),
            Arc::new(MemoryVectorStore::new(100)),
            CacheConfig { threshold: 0.9, ..CacheConfig::default() },
        );
        let state = SemanticCacheState {
            cache: Arc::new(cache),
            tenant: Arc::new(|r: &Request| r.headers().get("x-tenant").and_then(|v| v.to_str().ok()).map(str::to_string)),
        };
        axum::Router::new()
            .route(
                "/openai/v1/chat/completions",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let calls = calls.clone();
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                        axum::Json(json!({ "id": format!("c{n}"), "echo": body["messages"][0]["content"] }))
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(state, semantic_cache_middleware))
    }

    async fn ask(app: &axum::Router, tenant: Option<&str>, text: &str, extra: Option<(&str, &str)>) -> (String, Value) {
        let mut b = Request::builder().method("POST").uri("/openai/v1/chat/completions").header("content-type", "application/json");
        if let Some(t) = tenant {
            b = b.header("x-tenant", t);
        }
        if let Some((k, v)) = extra {
            b = b.header(k, v);
        }
        let body = json!({ "model": "m", "temperature": 0, "messages": [{ "role": "user", "content": text }] });
        let resp = app.clone().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let tag = resp.headers().get(CACHE_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("none").to_string();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (tag, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn second_similar_question_is_served_from_cache_per_tenant() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(calls.clone());
        let (t1, a1) = ask(&app, Some("acme"), "What is the capital of France?", None).await;
        assert_eq!((t1.as_str(), a1["id"].as_str()), ("miss", Some("c1")));
        let (t2, a2) = ask(&app, Some("acme"), "what is the capital of france", None).await;
        assert_eq!((t2.as_str(), a2["id"].as_str()), ("hit", Some("c1")));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a hit never reaches the inner layers");
        // Another tenant never sees acme's answer.
        let (t3, a3) = ask(&app, Some("globex"), "What is the capital of France?", None).await;
        assert_eq!((t3.as_str(), a3["id"].as_str()), ("miss", Some("c2")));
        // Opt-out and unauthenticated requests go straight through.
        let (t4, _) = ask(&app, Some("acme"), "What is the capital of France?", Some(("x-relay-cache", "bypass"))).await;
        assert_eq!(t4, "bypass");
        let (t5, _) = ask(&app, None, "What is the capital of France?", None).await;
        assert_eq!(t5, "none");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
