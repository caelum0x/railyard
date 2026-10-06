//! The axum layer: buffers an inference request's JSON body, applies the
//! [`Policy`](crate::Policy), and either forwards it (unchanged or redacted)
//! or answers `400 guardrail_blocked` before any inner layer runs.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::json;

use crate::{Policy, Verdict};

/// Reported on every request the policy changed (`email=1,card=2`).
pub const REDACTIONS_HEADER: &str = "x-relay-guardrails";

/// Shared state of the layer.
#[derive(Clone)]
pub struct GuardrailsState {
    pub policy: Arc<Policy>,
    /// Bodies larger than this are refused (the gateway's own body limit).
    pub max_body_bytes: usize,
}

/// The inference routes the policy applies to (any base path).
pub fn is_inference_path(path: &str) -> bool {
    path.ends_with("/inference")
        || path.ends_with("/batch_inference")
        || path.ends_with("/chat/completions")
        || path.ends_with("/completions")
        || path.ends_with("/responses")
        || path.ends_with("/embeddings")
}

fn blocked(code: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(
            json!({ "error": { "message": message, "type": "guardrail_blocked", "code": code } }),
        ),
    )
        .into_response()
}

/// `axum::middleware::from_fn_with_state(state, guardrails_middleware)`.
pub async fn guardrails_middleware(
    State(state): State<GuardrailsState>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::POST || !is_inference_path(req.uri().path()) {
        return next.run(req).await;
    }
    let is_json = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.to_ascii_lowercase().contains("json"));
    if !is_json {
        // Multipart and other bodies carry no chat prompt to read.
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes: Bytes = match http_body_util::Limited::new(body, state.max_body_bytes)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return blocked(
                "body_too_large",
                "The request body is larger than this gateway accepts.",
            )
        }
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        // Not JSON: the handler reports the parse error as usual.
        Err(_) => {
            return next
                .run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
    };
    match state.policy.check(&value) {
        Verdict::Pass => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Verdict::Blocked { code, message } => {
            tracing::info!(code, path = %parts.uri.path(), "guardrails refused a request");
            blocked(code, &message)
        }
        Verdict::Redacted { body, counts } => {
            let encoded = match serde_json::to_vec(&body) {
                Ok(b) => b,
                Err(_) => return blocked("redaction_failed", "The request could not be redacted."),
            };
            let summary = counts
                .iter()
                .map(|(k, n)| format!("{}={n}", k.to_ascii_lowercase()))
                .collect::<Vec<_>>()
                .join(",");
            let mut parts = parts;
            parts.headers.remove(header::CONTENT_LENGTH);
            let mut res = next
                .run(Request::from_parts(parts, Body::from(encoded)))
                .await;
            if let Ok(v) = HeaderValue::from_str(&summary) {
                res.headers_mut().insert(REDACTIONS_HEADER, v);
            }
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    /// An upstream that echoes the body it received (what billing and the
    /// provider would see).
    fn app(policy: &str) -> Router {
        let state = GuardrailsState {
            policy: Arc::new(Policy::from_json(policy).unwrap()),
            max_body_bytes: 1024,
        };
        Router::new()
            .route(
                "/openai/v1/chat/completions",
                post(|body: Bytes| async move { body }),
            )
            .route("/v1/models/list", post(|body: Bytes| async move { body }))
            .layer(axum::middleware::from_fn_with_state(
                state,
                guardrails_middleware,
            ))
    }

    async fn send(app: &Router, path: &str, body: &str) -> (StatusCode, Option<String>, String) {
        let res = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let header = res
            .headers()
            .get(REDACTIONS_HEADER)
            .map(|v| v.to_str().unwrap().to_owned());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, header, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_provider_only_sees_the_redacted_prompt() {
        let app = app(r#"{"pii":"redact"}"#);
        let (status, header, upstream) = send(
            &app,
            "/openai/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"reach me at ada@example.com"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header.as_deref(), Some("email=1"));
        assert!(
            upstream.contains("[REDACTED_EMAIL]") && !upstream.contains("ada@example.com"),
            "{upstream}"
        );
    }

    #[tokio::test]
    async fn a_blocked_request_never_reaches_the_inner_layers() {
        let app = app(r#"{"pii":"block"}"#);
        let (status, _, body) = send(
            &app,
            "/openai/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"card 4111 1111 1111 1111"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["type"], "guardrail_blocked");
        assert_eq!(v["error"]["code"], "personal_data");
        assert!(!body.contains("4111"), "the data is never echoed back");
    }

    #[tokio::test]
    async fn other_routes_bad_json_and_clean_prompts_pass_untouched() {
        let app = app(r#"{"pii":"block"}"#);
        let raw = r#"{"content":"ada@example.com"}"#;
        assert_eq!(
            send(&app, "/v1/models/list", raw).await.2,
            raw,
            "not an inference route"
        );
        assert_eq!(
            send(&app, "/openai/v1/chat/completions", "{not json")
                .await
                .2,
            "{not json"
        );
        let clean = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let (status, header, upstream) = send(&app, "/openai/v1/chat/completions", clean).await;
        assert_eq!(
            (status, header, upstream.as_str()),
            (StatusCode::OK, None, clean)
        );
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let app = app(r#"{"pii":"redact"}"#);
        let big = format!(
            r#"{{"messages":[{{"role":"user","content":"{}"}}]}}"#,
            "a".repeat(2048)
        );
        assert_eq!(
            send(&app, "/openai/v1/chat/completions", &big).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
