use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use switchyard_core::{EgressError, HttpRequest, HttpResponse, HttpTransport};

#[derive(Clone, Debug)]
pub enum FakeReply {
    Json(u16, Value),
    Text(u16, String, String),
    Fail(EgressError),
}

type Responder = Arc<dyn Fn(&HttpRequest) -> FakeReply + Send + Sync>;

struct Route {
    host: String,
    path_prefix: String,
    responder: Responder,
    delay: Duration,
    hits: Arc<AtomicUsize>,
}

/// A scripted upstream. Unmatched requests fail with a network error.
#[derive(Default)]
pub struct FakeHttp {
    routes: Mutex<Vec<Route>>,
    log: Mutex<Vec<HttpRequest>>,
}

impl FakeHttp {
    pub fn new() -> Arc<Self> {
        Arc::new(FakeHttp::default())
    }

    pub fn route(&self, host: &str, path_prefix: &str, reply: FakeReply) -> Arc<AtomicUsize> {
        self.route_fn(host, path_prefix, Duration::ZERO, move |_| reply.clone())
    }

    pub fn route_json(&self, host: &str, path_prefix: &str, status: u16, body: Value) -> Arc<AtomicUsize> {
        self.route(host, path_prefix, FakeReply::Json(status, body))
    }

    pub fn route_fn(&self, host: &str, path_prefix: &str, delay: Duration, f: impl Fn(&HttpRequest) -> FakeReply + Send + Sync + 'static) -> Arc<AtomicUsize> {
        let hits = Arc::new(AtomicUsize::new(0));
        // Newer routes win (lets a test override a fixture).
        self.routes.lock().unwrap().insert(0, Route { host: host.into(), path_prefix: path_prefix.into(), responder: Arc::new(f), delay, hits: hits.clone() });
        hits
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.log.lock().unwrap().clone()
    }

    pub fn total_requests(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

#[async_trait]
impl HttpTransport for FakeHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, EgressError> {
        self.log.lock().unwrap().push(req.clone());
        let u = url::Url::parse(&req.url).map_err(|e| EgressError::InvalidUrl(e.to_string()))?;
        let host = u.host_str().unwrap_or_default().to_string();
        let path = u.path().to_string();
        let found = {
            let routes = self.routes.lock().unwrap();
            routes.iter().find(|r| r.host == host && path.starts_with(&r.path_prefix)).map(|r| (r.responder.clone(), r.delay, r.hits.clone()))
        };
        let Some((responder, delay, hits)) = found else {
            return Err(EgressError::Network(format!("no fake route for {host}{path}")));
        };
        hits.fetch_add(1, Ordering::SeqCst);
        if !delay.is_zero() {
            if delay >= req.timeout {
                tokio::time::sleep(req.timeout).await;
                return Err(EgressError::Timeout);
            }
            tokio::time::sleep(delay).await;
        }
        match responder(&req) {
            FakeReply::Json(status, body) => Ok(HttpResponse {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.to_string().into_bytes(),
                final_url: req.url.clone(),
            }),
            FakeReply::Text(status, body, ctype) => Ok(HttpResponse { status, headers: vec![("content-type".into(), ctype)], body: body.into_bytes(), final_url: req.url.clone() }),
            FakeReply::Fail(e) => Err(e),
        }
    }
}
