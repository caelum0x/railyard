use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use http::{HeaderMap, Method};
use std::pin::Pin;
use url::Url;

use crate::error::{RelaySdkError, Result};

pub type HttpByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + Sync + 'static>>;

#[derive(Debug)]
pub struct HttpRequest {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: HttpByteStream,
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse>;
}

#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new(
        connect_timeout: Duration,
        read_timeout: Duration,
        user_agent: &str,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .read_timeout(read_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(64)
            .user_agent(user_agent)
            .build()
            .map_err(|error| RelaySdkError::InvalidConfig(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
        let response = self
            .client
            .request(request.method, request.url)
            .headers(request.headers)
            .body(request.body)
            .timeout(request.timeout)
            .send()
            .await
            .map_err(|error| RelaySdkError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response
            .bytes_stream()
            .map(|result| result.map_err(|error| RelaySdkError::Transport(error.to_string())));
        Ok(HttpResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}
