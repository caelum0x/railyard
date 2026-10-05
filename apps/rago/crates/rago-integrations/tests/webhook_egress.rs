//! Outbound webhooks reach public addresses only (egress-guard): refused
//! endpoints are never sent, and the live transport resolves once, pins the
//! validated address and follows no redirect.

use rago_integrations::webhooks::{deliver, Webhook, WebhookStatus};
use rago_integrations::MockTransport;
use serde_json::json;
use uuid::Uuid;

fn hook(endpoint: &str) -> Webhook {
    Webhook::build(Uuid::new_v4(), endpoint, "invoice.created", "invoice", json!({"id": "inv_1"}))
}

#[tokio::test]
async fn internal_endpoints_in_any_spelling_fail_without_a_request_or_a_retry() {
    let transport = MockTransport::always(200, "{}");
    for endpoint in [
        "http://2130706433/hooks",
        "http://0177.0.0.1/hooks",
        "http://0x7f.0.0.1/hooks",
        "http://[::ffff:127.0.0.1]/hooks",
        "http://[::ffff:a9fe:a9fe]/latest/meta-data",
        "http://169.254.169.254/latest/meta-data",
        "http://10.0.0.8/hooks",
        "http://[fd00:ec2::254]/",
        "https://user:pw@hooks.example/rago",
        "file:///etc/passwd",
    ] {
        let mut w = hook(endpoint);
        assert!(!deliver(&transport, &mut w, "k").await, "no retry for {endpoint}");
        assert_eq!(w.status, WebhookStatus::Failed, "{endpoint}");
        assert!(w.response.as_deref().unwrap_or_default().starts_with("blocked target:"), "{endpoint}: {:?}", w.response);
    }
    assert!(transport.requests().is_empty(), "nothing was sent");
    let mut ok = hook("https://hooks.example/rago");
    assert!(!deliver(&transport, &mut ok, "k").await);
    assert_eq!(ok.status, WebhookStatus::Succeeded);
}

#[cfg(feature = "live")]
mod live {
    use std::future::Future;
    use std::io;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use egress_guard::{Policy, Resolver};
    use rago_integrations::{HttpTransport, ReqwestTransport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct Script {
        answers: Mutex<Vec<Vec<SocketAddr>>>,
        calls: AtomicUsize,
    }

    impl Script {
        fn new(answers: Vec<Vec<String>>) -> Arc<Self> {
            let answers = answers.into_iter().map(|a| a.into_iter().map(|s| s.parse().unwrap()).collect()).collect();
            Arc::new(Self { answers: Mutex::new(answers), calls: AtomicUsize::new(0) })
        }
    }

    impl Resolver for Script {
        fn resolve<'a>(&'a self, _host: &'a str, _port: u16) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut answers = self.answers.lock().unwrap();
            let next = if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() };
            Box::pin(async move { Ok(next) })
        }
    }

    async fn server(status: &'static str, extra: String) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let extra = extra.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut seen = Vec::new();
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n{extra}\r\nok");
                    let _ = sock.write_all(reply.as_bytes()).await;
                });
            }
        });
        (addr, hits)
    }

    const LOCAL: Policy = Policy { allow_http: true, allow_private: true };
    const STRICT: Policy = Policy { allow_http: true, allow_private: false };

    #[tokio::test]
    async fn a_rebinding_host_is_refused_or_pinned_never_resolved_twice() {
        // An internal answer is refused before any connection.
        let dns = Script::new(vec![vec!["127.0.0.1:443".into()]]);
        let t = ReqwestTransport::with_guard(dns.clone(), STRICT);
        let err = t.post_external("https://rebind.example/hook", &[], "{}".into()).await.unwrap_err();
        assert!(err.to_string().contains("blocked target"), "{err}");

        // Pinned: the host has no DNS record anywhere, so reaching the server
        // proves the second (rebound) answer was never asked for.
        let (addr, hits) = server("200 OK", String::new()).await;
        let dns = Script::new(vec![vec![addr.to_string()], vec!["10.9.9.9:1".into()]]);
        let t = ReqwestTransport::with_guard(dns.clone(), LOCAL);
        let resp = t.post_external(&format!("http://rebind.invalid:{}/hook", addr.port()), &[], "{}".into()).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!((hits.load(Ordering::SeqCst), dns.calls.load(Ordering::SeqCst)), (1, 1));
    }

    #[tokio::test]
    async fn a_redirect_to_an_internal_address_is_not_followed() {
        let (inner, inner_hits) = server("200 OK", String::new()).await;
        let (outer, _) = server("302 Found", format!("location: http://127.0.0.1:{}/admin\r\n", inner.port())).await;
        let dns = Script::new(vec![vec![outer.to_string()]]);
        let t = ReqwestTransport::with_guard(dns, LOCAL);
        let resp = t.post_external(&format!("http://hooks.invalid:{}/hook", outer.port()), &[], "{}".into()).await.unwrap();
        assert_eq!(resp.status, 302);
        assert_eq!(inner_hits.load(Ordering::SeqCst), 0);
    }
}
