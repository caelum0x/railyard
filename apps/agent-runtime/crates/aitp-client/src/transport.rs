//! The HTTP transport seam for the Mint-backed gateway.
//!
//! [`MintPaymentGateway`](crate::mint::MintPaymentGateway) never touches
//! `reqwest` directly — it speaks to this small [`HttpTransport`] trait. The
//! production wiring injects [`ReqwestTransport`]; tests inject a fake that
//! records the exact requests and replays canned mint-server responses. This is
//! the boundary that keeps the gateway's protocol logic unit-testable without a
//! live server or a network.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{stream, Stream, StreamExt};
use std::pin::Pin;
use std::time::Duration;

use crate::error::PayError;

/// Timeouts applied to every money-path request the transport makes.
///
/// A hung Mint/Relay must never block `authorize`/`capture`/`release` forever —
/// an unbounded wait on a settlement call is itself a correctness bug (holds
/// never resolve, captures never confirm). These bound both connection setup and
/// the full request/response.
#[derive(Debug, Clone, Copy)]
pub struct TransportTimeouts {
    /// Max time to establish the TCP+TLS connection.
    pub connect: Duration,
    /// Max time for the entire request, including reading the response body.
    pub request: Duration,
}

impl TransportTimeouts {
    /// Sane production defaults: 10s to connect, 30s for the whole request.
    pub const DEFAULT_CONNECT: Duration = Duration::from_secs(10);
    /// See [`TransportTimeouts::DEFAULT_CONNECT`].
    pub const DEFAULT_REQUEST: Duration = Duration::from_secs(30);
}

impl Default for TransportTimeouts {
    fn default() -> Self {
        Self {
            connect: Self::DEFAULT_CONNECT,
            request: Self::DEFAULT_REQUEST,
        }
    }
}

/// An outbound HTTP request the gateway wants to make against mint-server.
///
/// Header names are lowercased (HTTP headers are case-insensitive); the body is
/// already-serialized JSON bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// `"POST"` or `"GET"`.
    pub method: &'static str,
    /// Path only (e.g. `"/v1/aitp/verify"`); the transport prepends the base URL.
    pub path: String,
    /// Lowercased header name/value pairs (e.g. `("x-api-key", "…")`).
    pub headers: Vec<(String, String)>,
    /// Serialized JSON request body (empty for GET).
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A `GET` with no body.
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A `POST` carrying `body` as `application/json`.
    pub fn post_json(path: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: "POST",
            path: path.into(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body,
        }
    }

    /// Append a header (name is lowercased for case-insensitive matching).
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .push((name.into().to_ascii_lowercase(), value.into()));
        self
    }

    /// Replace every existing value for a header with one controlled value.
    ///
    /// Protocol wrappers use this for correlation and authorization headers so
    /// a caller cannot leave a stale value earlier in the header list.
    #[must_use]
    pub fn with_replaced_header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let name = name.into().to_ascii_lowercase();
        self.headers.retain(|(key, _)| key != &name);
        self.headers.push((name, value.into()));
        self
    }

    /// Case-insensitive request-header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let needle = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == needle)
            .map(|(_, value)| value.as_str())
    }
}

/// An HTTP response from mint-server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code (e.g. `200`, `402`).
    pub status: u16,
    /// Lowercased header name/value pairs.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

/// Incremental response body returned by a streaming transport.
pub type HttpByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, PayError>> + Send + 'static>>;

/// HTTP status and headers are available before the incremental body. This is
/// required for AITP because a 402 challenge lives entirely in response
/// headers and must be handled before any model bytes are exposed.
pub struct StreamingHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: HttpByteStream,
}

impl StreamingHttpResponse {
    /// Case-insensitive response-header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let needle = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == needle)
            .map(|(_, value)| value.as_str())
    }

    /// Append a header (name lowercased).
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .push((name.into().to_ascii_lowercase(), value.into()));
        self
    }
}

impl HttpResponse {
    /// Construct a response with a body and no headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Append a header (name lowercased).
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .push((name.into().to_ascii_lowercase(), value.into()));
        self
    }

    /// Case-insensitive header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let needle = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == needle)
            .map(|(_, v)| v.as_str())
    }

    /// `true` for `2xx`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The injectable transport. Implementors carry the base URL and API key.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Send `req` and return the response, or a [`PayError::Gateway`] on a
    /// transport-level failure (DNS, connection, TLS, timeout).
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError>;

    /// Send without buffering the response body. Existing transports get a
    /// safe one-chunk compatibility implementation; production transports
    /// should override this to retain genuine network backpressure.
    async fn send_streaming(&self, req: HttpRequest) -> Result<StreamingHttpResponse, PayError> {
        let response = self.send(req).await?;
        Ok(StreamingHttpResponse {
            status: response.status,
            headers: response.headers,
            body: Box::pin(stream::once(async move { Ok(Bytes::from(response.body)) })),
        })
    }
}

/// The production [`HttpTransport`]: a `reqwest` client bound to a base URL.
///
/// Construct via [`ReqwestTransport::new`], passing the mint-server base URL
/// (e.g. `https://mint.internal`). The per-request `X-API-Key` header is set by
/// the gateway, not here, so the transport stays a dumb pipe.
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
}

impl ReqwestTransport {
    /// A transport for `base_url` (trailing slash trimmed) with the default
    /// money-path timeouts ([`TransportTimeouts::default`]). Fails only if the
    /// underlying TLS/client build fails.
    pub fn new(base_url: impl Into<String>) -> Result<Self, PayError> {
        Self::with_timeouts(base_url, TransportTimeouts::default())
    }

    /// A transport for `base_url` with explicit connect/request timeouts.
    ///
    /// Use this to tune the bounds a hung Mint/Relay may impose on
    /// `authorize`/`capture`/`release`.
    pub fn with_timeouts(
        base_url: impl Into<String>,
        timeouts: TransportTimeouts,
    ) -> Result<Self, PayError> {
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .map_err(|e| PayError::Gateway(format!("http client build failed: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Reuse an already-configured client (e.g. one with timeouts/proxy set).
    pub fn with_client(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        let url = format!("{}{}", self.base_url, req.path);
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| PayError::Gateway(format!("bad method {}: {e}", req.method)))?;

        let mut builder = self.client.request(method, &url);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| PayError::Gateway(format!("request to {url} failed: {e}")))?;

        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = resp
            .bytes()
            .await
            .map_err(|e| PayError::Gateway(format!("reading body from {url} failed: {e}")))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn send_streaming(&self, req: HttpRequest) -> Result<StreamingHttpResponse, PayError> {
        let url = format!("{}{}", self.base_url, req.path);
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|error| PayError::Gateway(format!("bad method {}: {error}", req.method)))?;

        let mut builder = self.client.request(method, &url);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body);
        }

        let response = builder
            .send()
            .await
            .map_err(|error| PayError::Gateway(format!("request to {url} failed: {error}")))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response.bytes_stream().map(move |chunk| {
            chunk.map_err(|error| PayError::Gateway(format!("reading stream from {url}: {error}")))
        });

        Ok(StreamingHttpResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Instant;

    #[test]
    fn timeouts_default_to_bounded_values() {
        let t = TransportTimeouts::default();
        assert_eq!(t.connect, TransportTimeouts::DEFAULT_CONNECT);
        assert_eq!(t.request, TransportTimeouts::DEFAULT_REQUEST);
    }

    #[test]
    fn new_and_with_timeouts_build_a_client() {
        assert!(ReqwestTransport::new("https://mint.internal/").is_ok());
        assert!(ReqwestTransport::with_timeouts(
            "https://mint.internal",
            TransportTimeouts {
                connect: Duration::from_millis(250),
                request: Duration::from_millis(250),
            },
        )
        .is_ok());
    }

    /// A hung server (accepts the connection, reads the request, then never
    /// responds) must not block a money-path call forever: the request timeout
    /// fires and surfaces a `Gateway` error well within the bound.
    #[tokio::test(flavor = "current_thread")]
    async fn request_timeout_is_applied_to_a_hung_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");

        // A background thread accepts one connection, drains the request bytes,
        // and then holds the socket open without ever writing a response.
        let handle = std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf);
                // Hold the connection open long enough for the client timeout to
                // fire; the socket closes when this thread returns.
                std::thread::sleep(Duration::from_millis(800));
            }
        });

        let transport = ReqwestTransport::with_timeouts(
            format!("http://{addr}"),
            TransportTimeouts {
                connect: Duration::from_millis(200),
                request: Duration::from_millis(200),
            },
        )
        .expect("transport builds");

        let started = Instant::now();
        let result = transport.send(HttpRequest::get("/v1/aitp/verify")).await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "a hung server must not hang the caller");
        assert!(
            matches!(result, Err(PayError::Gateway(_))),
            "transport failure maps to PayError::Gateway"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "request timeout must fire promptly, took {elapsed:?}"
        );

        let _ = handle.join();
    }
}
