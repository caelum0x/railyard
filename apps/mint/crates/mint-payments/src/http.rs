//! The injectable HTTP seam so every `confirm_deposit` / `create_checkout` /
//! `initiate_payout` provider call is fake-injectable in tests.
//!
//! [`HttpTransport`] is the trait every adapter holds behind an `Arc`;
//! [`ReqwestTransport`] is the production impl. Transport/5xx failures map to
//! [`mint_reserve::RampError::ProviderUnavailable`] and 404 to
//! [`mint_reserve::RampError::DepositNotFound`] via [`map_status`].
//!
//! [`Headers`] is the axum-free header-lookup seam used by every
//! `verify_webhook` so signature verification runs on RAW bytes with borrowed
//! header values (no axum dependency in this crate).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use mint_reserve::RampError;

/// A raw HTTP response: numeric status + body bytes (never pre-parsed).
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The raw response body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// `true` iff the status is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// A transport-level failure (DNS, TLS, timeout, connection reset). Always
/// retryable → maps to [`RampError::ProviderUnavailable`].
#[derive(Clone, Debug)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transport error: {}", self.0)
    }
}

impl std::error::Error for TransportError {}

/// The injectable HTTP client seam. Header pairs are `(name, value)`; a caller
/// that supplies a `content-type` header overrides the transport default.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Issue a GET, returning status + raw body.
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError>;

    /// Issue a POST with a raw body (JSON by default; form when the caller sets
    /// `content-type: application/x-www-form-urlencoded`).
    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, TransportError>;
}

/// Map a transport result onto the [`RampError`] taxonomy: transport error or
/// 5xx → `ProviderUnavailable`, 404 → `DepositNotFound`, any other non-2xx →
/// `ProviderUnavailable` (conservatively retryable), 2xx → the response.
pub fn map_status(result: Result<HttpResponse, TransportError>) -> Result<HttpResponse, RampError> {
    match result {
        Err(_) => Err(RampError::ProviderUnavailable),
        Ok(resp) if resp.is_success() => Ok(resp),
        Ok(resp) if resp.status == 404 => Err(RampError::DepositNotFound),
        Ok(_) => Err(RampError::ProviderUnavailable),
    }
}

/// The production [`HttpTransport`], backed by a rustls `reqwest::Client`.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestTransport {
    /// Build a transport with a 20s request timeout.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with rustls");
        ReqwestTransport { client }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError(e.to_string()))?;
        into_http_response(resp).await
    }

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        let has_content_type = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
        let mut req = self.client.post(url);
        if !has_content_type {
            req = req.header("content-type", "application/json");
        }
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| TransportError(e.to_string()))?;
        into_http_response(resp).await
    }
}

async fn into_http_response(resp: reqwest::Response) -> Result<HttpResponse, TransportError> {
    let status = resp.status().as_u16();
    let body = resp
        .bytes()
        .await
        .map_err(|e| TransportError(e.to_string()))?
        .to_vec();
    Ok(HttpResponse { status, body })
}

/// A case-insensitive header-lookup seam so `verify_webhook` stays axum-free.
/// `mint-server`'s thin glue implements this for `axum::http::HeaderMap`; tests
/// use a `HashMap<String, String>`.
pub trait Headers {
    /// Look up a header value by (case-insensitive) name.
    fn get_header(&self, name: &str) -> Option<&str>;
}

impl Headers for HashMap<String, String> {
    fn get_header(&self, name: &str) -> Option<&str> {
        self.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl Headers for Vec<(String, String)> {
    fn get_header(&self, name: &str) -> Option<&str> {
        self.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}
