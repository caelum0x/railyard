//! Egress end-to-end against in-process upstreams on 127.0.0.1 (exempted only
//! through the test-only `allow_ips`), with a fake resolver mapping test hosts.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use switchyard_core::{EgressError, HttpRequest, HttpTransport};
use switchyard_egress::{EgressConfig, Resolver, SafeClient};

struct FakeDns(HashMap<String, Vec<IpAddr>>);

#[async_trait]
impl Resolver for FakeDns {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.0.get(host).cloned().ok_or_else(|| "nxdomain".to_string())
    }
}

/// Answers public first, then a private address (a rebinding attacker).
struct Rebinding(AtomicUsize);

#[async_trait]
impl Resolver for Rebinding {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![if n == 0 { "93.184.216.34".parse().unwrap() } else { "10.0.0.5".parse().unwrap() }])
    }
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn client(hosts: &[&str], dns: HashMap<String, Vec<IpAddr>>) -> SafeClient {
    let cfg = EgressConfig {
        allow_hosts: hosts.iter().map(|h| h.to_string()).collect::<BTreeSet<_>>(),
        allow_http: true,
        allow_ips: vec!["127.0.0.1".parse().unwrap()],
        ..Default::default()
    };
    SafeClient::with_resolver(cfg, Arc::new(FakeDns(dns))).unwrap()
}

fn dns(pairs: &[(&str, &str)]) -> HashMap<String, Vec<IpAddr>> {
    pairs.iter().map(|(h, ip)| (h.to_string(), vec![ip.parse().unwrap()])).collect()
}

#[tokio::test]
async fn allowed_host_round_trip_and_caller_auth_stripped() {
    let app = Router::new().route(
        "/echo",
        get(|headers: HeaderMap| async move {
            let auth = headers.get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let key = headers.get("x-api-key").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let cookie = headers.contains_key("cookie");
            axum::Json(serde_json::json!({"auth": auth, "key": key, "cookie": cookie}))
        }),
    );
    let addr = serve(app).await;
    let c = client(&["api.test"], dns(&[("api.test", "127.0.0.1")]));
    let req = HttpRequest::get(format!("http://api.test:{}/echo", addr.port()))
        .header("authorization", "Bearer caller-token")
        .header("cookie", "session=1")
        .secret_header("x-api-key", "injected");
    let resp = c.send(req).await.unwrap();
    assert_eq!(resp.status, 200);
    let v = resp.json().unwrap();
    assert_eq!(v["auth"], "", "caller Authorization must never be forwarded");
    assert_eq!(v["cookie"], false);
    assert_eq!(v["key"], "injected");
}

#[tokio::test]
async fn unlisted_host_is_refused() {
    let c = client(&["api.test"], dns(&[("other.test", "127.0.0.1")]));
    let err = c.send(HttpRequest::get("http://other.test/")).await.unwrap_err();
    assert!(matches!(err, EgressError::HostNotAllowed(_)), "{err:?}");
}

#[tokio::test]
async fn private_resolution_is_blocked_even_when_listed() {
    let c = SafeClient::with_resolver(
        EgressConfig { allow_hosts: ["internal.test".to_string()].into(), allow_http: true, ..Default::default() },
        Arc::new(FakeDns(dns(&[("internal.test", "10.1.2.3")]))),
    )
    .unwrap();
    let err = c.send(HttpRequest::get("http://internal.test/")).await.unwrap_err();
    assert!(matches!(err, EgressError::BlockedAddress(_)), "{err:?}");
}

#[tokio::test]
async fn ip_literal_and_decimal_and_metadata_bypasses_blocked() {
    let c = SafeClient::with_resolver(
        EgressConfig { allow_hosts: ["api.test".to_string()].into(), allow_http: true, ..Default::default() },
        Arc::new(FakeDns(HashMap::new())),
    )
    .unwrap();
    for url in [
        "http://127.0.0.1/",
        "http://2130706433/",
        "http://0x7f000001/",
        "http://0177.0.0.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "http://[::ffff:169.254.169.254]/",
        "http://[fd00:ec2::254]/",
    ] {
        let err = c.send(HttpRequest::get(url)).await.unwrap_err();
        assert!(matches!(err, EgressError::BlockedAddress(_)), "{url}: {err:?}");
    }
}

#[tokio::test]
async fn https_only_in_production_mode() {
    let c = SafeClient::with_resolver(
        EgressConfig { allow_hosts: ["api.test".to_string()].into(), ..Default::default() },
        Arc::new(FakeDns(dns(&[("api.test", "93.184.216.34")]))),
    )
    .unwrap();
    for url in ["http://api.test/", "ftp://api.test/", "file:///etc/passwd", "gopher://api.test/"] {
        let err = c.send(HttpRequest::get(url)).await.unwrap_err();
        assert!(matches!(err, EgressError::Scheme(_)), "{url}: {err:?}");
    }
}

#[tokio::test]
async fn dns_rebinding_uses_pinned_first_answer() {
    // First answer is public and passes; the pinned address is used for the
    // connection. The resolver is consulted exactly once per hop.
    let resolver = Arc::new(Rebinding(AtomicUsize::new(0)));
    let c = SafeClient::with_resolver(
        EgressConfig { allow_hosts: ["rebind.test".to_string()].into(), allow_http: true, connect_timeout: Duration::from_millis(300), ..Default::default() },
        resolver.clone(),
    )
    .unwrap();
    let _ = c.send(HttpRequest::get("http://rebind.test/").timeout(Duration::from_millis(500))).await;
    assert_eq!(resolver.0.load(Ordering::SeqCst), 1, "reqwest must never re-resolve on its own");
    // A second request re-resolves (now private) and is refused before connecting.
    let err = c.send(HttpRequest::get("http://rebind.test/")).await.unwrap_err();
    assert!(matches!(err, EgressError::BlockedAddress(_)), "{err:?}");
}

#[tokio::test]
async fn redirect_to_private_ip_is_blocked() {
    let app = Router::new().route("/go", get(|| async { Redirect::temporary("http://169.254.169.254/latest/meta-data/") }));
    let addr = serve(app).await;
    let c = client(&["api.test"], dns(&[("api.test", "127.0.0.1")]));
    let err = c.send(HttpRequest::get(format!("http://api.test:{}/go", addr.port()))).await.unwrap_err();
    assert!(matches!(err, EgressError::BlockedAddress(_)), "{err:?}");
}

#[tokio::test]
async fn credentials_never_cross_hosts_on_redirect() {
    let port_holder = Arc::new(std::sync::Mutex::new(0u16));
    let ph = port_holder.clone();
    let app = Router::new()
        .route(
            "/start",
            get(move || {
                let port = *ph.lock().unwrap();
                async move { Redirect::temporary(&format!("http://b.test:{port}/land")) }
            }),
        )
        .route(
            "/land",
            get(|req: Request| async move {
                let key = req.headers().get("x-api-key").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
                axum::Json(serde_json::json!({"key": key})).into_response()
            }),
        );
    let addr = serve(app).await;
    *port_holder.lock().unwrap() = addr.port();
    let c = client(&["a.test", "b.test"], dns(&[("a.test", "127.0.0.1"), ("b.test", "127.0.0.1")]));
    let resp = c.send(HttpRequest::get(format!("http://a.test:{}/start", addr.port())).secret_header("x-api-key", "secret")).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json().unwrap()["key"], "", "credential dropped once the redirect left a.test");
}

#[tokio::test]
async fn redirect_loop_capped_and_body_size_capped() {
    let app = Router::new()
        .route("/loop", get(|| async { Redirect::temporary("/loop") }))
        .route("/big", get(|| async { "x".repeat(10_000) }));
    let addr = serve(app).await;
    let c = client(&["api.test"], dns(&[("api.test", "127.0.0.1")]));
    let err = c.send(HttpRequest::get(format!("http://api.test:{}/loop", addr.port()))).await.unwrap_err();
    assert_eq!(err, EgressError::TooManyRedirects);
    let mut req = HttpRequest::get(format!("http://api.test:{}/big", addr.port()));
    req.max_body_bytes = 1_000;
    assert_eq!(c.send(req).await.unwrap_err(), EgressError::TooLarge(1_000));
}

#[tokio::test]
async fn slow_upstream_times_out() {
    let app = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "late"
        }),
    );
    let addr = serve(app).await;
    let c = client(&["api.test"], dns(&[("api.test", "127.0.0.1")]));
    let err = c.send(HttpRequest::get(format!("http://api.test:{}/slow", addr.port())).timeout(Duration::from_millis(200))).await.unwrap_err();
    assert_eq!(err, EgressError::Timeout);
}

#[tokio::test]
async fn per_request_extra_hosts_are_scoped() {
    let app = Router::new().route("/", get(|| async { "ok" }));
    let addr = serve(app).await;
    let c = client(&[], dns(&[("page.test", "127.0.0.1")]));
    let mut req = HttpRequest::get(format!("http://page.test:{}/", addr.port()));
    req.extra_hosts = vec!["page.test".into()];
    assert_eq!(c.send(req).await.unwrap().text(), "ok");
    assert!(matches!(c.send(HttpRequest::get(format!("http://page.test:{}/", addr.port()))).await, Err(EgressError::HostNotAllowed(_))));
}
