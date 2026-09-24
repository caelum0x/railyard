//! Injectable HTTP transport.
//!
//! Both the payment providers and the webhook deliverer talk to the outside
//! world through this one trait, so their real request shape (URL, headers,
//! body) can be verified against an in-memory transport in tests without any
//! network access. The live [`reqwest`](https://docs.rs/reqwest)-backed
//! transport is behind the `live` feature (owner-gated — it needs real keys).

use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;

/// A raw HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport error: {0}")]
    Io(String),
}

/// A minimal async HTTP transport: POST a body with headers, get a response.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError>;

    /// GET `url` (used for provider lookups such as listing a Stripe customer's
    /// payment methods). Transports that only script POSTs inherit this default,
    /// which reports the capability as unavailable.
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        let _ = (url, headers);
        Err(TransportError::Io("GET is not supported by this transport".into()))
    }

    /// PATCH `url` with a body (used to update an existing provider record,
    /// e.g. a CRM contact). Transports without the capability report it as
    /// unavailable.
    async fn patch(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        let _ = (url, headers, body);
        Err(TransportError::Io("PATCH is not supported by this transport".into()))
    }

    /// PUT `url` with a body (used for idempotent provider writes such as CRM
    /// associations).
    async fn put(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        let _ = (url, headers, body);
        Err(TransportError::Io("PUT is not supported by this transport".into()))
    }
}

/// The transport used when outbound HTTP is not compiled in (builds without the
/// `live` feature). Every call fails loudly instead of pretending to succeed,
/// so a non-live deployment never marks a charge or a webhook as delivered.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledTransport;

const DISABLED_MSG: &str =
    "outbound HTTP is disabled in this build (rebuild rago with `--features live`)";

#[async_trait]
impl HttpTransport for DisabledTransport {
    async fn post(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: String,
    ) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Io(DISABLED_MSG.into()))
    }

    async fn get(
        &self,
        _url: &str,
        _headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Io(DISABLED_MSG.into()))
    }

    async fn patch(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: String,
    ) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Io(DISABLED_MSG.into()))
    }

    async fn put(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: String,
    ) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Io(DISABLED_MSG.into()))
    }
}

/// The production transport for this build: real reqwest under `live`.
#[cfg(feature = "live")]
pub type DefaultTransport = ReqwestTransport;
/// The production transport for this build: disabled without `live`.
#[cfg(not(feature = "live"))]
pub type DefaultTransport = DisabledTransport;

/// Construct the build's production transport.
pub fn default_transport() -> DefaultTransport {
    DefaultTransport::default()
}

/// Whether this build can reach external services.
pub const LIVE: bool = cfg!(feature = "live");

/// A shared reference to a transport is itself a transport, so one transport
/// instance can back both the payment provider and the webhook deliverer.
#[async_trait]
impl<T: HttpTransport + ?Sized> HttpTransport for &T {
    async fn post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        (**self).post(url, headers, body).await
    }

    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        (**self).get(url, headers).await
    }

    async fn patch(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        (**self).patch(url, headers, body).await
    }

    async fn put(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        (**self).put(url, headers, body).await
    }
}

/// A recorded request (for assertions).
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// In-memory transport: records requests and replays a scripted queue of
/// responses (or a default). Used to verify the real protocol shape in tests
/// and to drive the payment/webhook pipeline without a network.
pub struct MockTransport {
    requests: Mutex<Vec<RecordedRequest>>,
    responses: Mutex<Vec<Result<HttpResponse, TransportError>>>,
    default: Result<HttpResponse, TransportError>,
}

impl MockTransport {
    /// Always return `status`/`body`.
    pub fn always(status: u16, body: impl Into<String>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            default: Ok(HttpResponse {
                status,
                body: body.into(),
            }),
        }
    }

    /// Replay `responses` in order, then fall back to `default`.
    pub fn scripted(
        responses: Vec<Result<HttpResponse, TransportError>>,
        default: HttpResponse,
    ) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
            default: Ok(default),
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.requests.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(RecordedRequest {
            url: url.to_string(),
            headers: headers.to_vec(),
            body,
        });
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            self.default.clone()
        } else {
            queue.remove(0)
        }
    }

    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        self.post(url, headers, String::new()).await
    }

    async fn patch(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        self.post(url, headers, body).await
    }

    async fn put(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        self.post(url, headers, body).await
    }
}

impl Clone for TransportError {
    fn clone(&self) -> Self {
        match self {
            TransportError::Io(s) => TransportError::Io(s.clone()),
        }
    }
}

/// Live reqwest transport (owner-gated behind the `live` feature).
#[cfg(feature = "live")]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(feature = "live")]
impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "live")]
impl ReqwestTransport {
    async fn send_with_body(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        let mut req = self.client.request(method, url).body(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(HttpResponse { status, body })
    }
}

#[cfg(feature = "live")]
#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn patch(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        self.send_with_body(reqwest::Method::PATCH, url, headers, body).await
    }

    async fn put(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        self.send_with_body(reqwest::Method::PUT, url, headers, body).await
    }

    async fn post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: String,
    ) -> Result<HttpResponse, TransportError> {
        let mut req = self.client.post(url).body(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(HttpResponse { status, body })
    }

    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, TransportError> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(HttpResponse { status, body })
    }
}
