//! Shared test doubles: a routing [`FakeTransport`] (canned provider JSON, no
//! network) + deterministic signing helpers that recompute each provider's
//! signature scheme with a TEST secret over a fixed raw body.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use mint_payments::http::{HttpResponse, HttpTransport, TransportError};
use mint_payments::verify::hmac_sha256_raw;

/// A fake HTTP transport that returns canned responses matched by URL substring.
/// A `failing` transport simulates a transient provider outage (transport error).
pub struct FakeTransport {
    routes: Vec<(String, u16, Vec<u8>)>,
    fail: bool,
}

impl FakeTransport {
    pub fn new() -> Self {
        FakeTransport {
            routes: Vec::new(),
            fail: false,
        }
    }

    /// A transport whose every call fails at the transport level (→ outage).
    pub fn failing() -> Arc<dyn HttpTransport> {
        Arc::new(FakeTransport {
            routes: Vec::new(),
            fail: true,
        })
    }

    /// Add a canned JSON response for any URL containing `pat`.
    pub fn route(mut self, pat: &str, status: u16, body: serde_json::Value) -> Self {
        self.routes
            .push((pat.to_string(), status, serde_json::to_vec(&body).unwrap()));
        self
    }

    /// Add a canned response with a non-JSON / empty body.
    pub fn route_status(mut self, pat: &str, status: u16) -> Self {
        self.routes.push((pat.to_string(), status, b"{}".to_vec()));
        self
    }

    pub fn arc(self) -> Arc<dyn HttpTransport> {
        Arc::new(self)
    }

    fn resolve(&self, url: &str) -> Result<HttpResponse, TransportError> {
        if self.fail {
            return Err(TransportError("simulated outage".into()));
        }
        for (pat, status, body) in &self.routes {
            if url.contains(pat.as_str()) {
                return Ok(HttpResponse {
                    status: *status,
                    body: body.clone(),
                });
            }
        }
        Ok(HttpResponse {
            status: 200,
            body: b"{}".to_vec(),
        })
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn get(
        &self,
        url: &str,
        _headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        self.resolve(url)
    }

    async fn post_json(
        &self,
        url: &str,
        _headers: &[(String, String)],
        _body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        self.resolve(url)
    }
}

/// A `HashMap<String, String>` header bag (implements `mint_payments::Headers`).
pub fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- signing helpers (recompute each provider's scheme with a test secret) --

/// `Stripe-Signature: t=<ts>,v1=<hex hmac over "{t}.{body}">`.
pub fn stripe_sig(secret: &[u8], ts: &str, body: &[u8]) -> String {
    let mut msg = Vec::new();
    msg.extend_from_slice(ts.as_bytes());
    msg.push(b'.');
    msg.extend_from_slice(body);
    let tag = hex::encode(hmac_sha256_raw(secret, &msg));
    format!("t={ts},v1={tag}")
}

/// Creem / Coinbase `hex(HMAC-SHA256(secret, raw_body))`.
pub fn hmac_hex(secret: &[u8], body: &[u8]) -> String {
    hex::encode(hmac_sha256_raw(secret, body))
}

/// Standard Webhooks `v1,<base64 HMAC over "{id}.{ts}.{body}">`, with the secret
/// returned as `whsec_<base64(raw_key)>`.
pub fn standard_webhooks(raw_key: &[u8], id: &str, ts: &str, body: &[u8]) -> (String, String) {
    let mut signed = Vec::new();
    signed.extend_from_slice(id.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(ts.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);
    let tag = B64.encode(hmac_sha256_raw(raw_key, &signed));
    let secret = format!("whsec_{}", B64.encode(raw_key));
    (secret, format!("v1,{tag}"))
}
