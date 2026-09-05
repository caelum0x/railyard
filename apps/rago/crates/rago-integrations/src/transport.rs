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
}

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
#[async_trait]
impl HttpTransport for ReqwestTransport {
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
}
