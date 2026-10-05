//! Webhook delivery: HMAC-SHA256 signing + retrying HTTP POST, recorded in
//! the delivery audit table.
//!
//! This module no longer fires deliveries inline from API handlers. Handlers
//! enqueue a `webhook_delivery` job via [`rithub_core::OutboxStore::enqueue_job`]
//! and the worker in `crate::jobs` calls [`deliver`] for each active
//! subscription. This keeps slow/failing receivers off the hot request path
//! and gives durable retries with backoff.
//!
//! The signature header (`X-Rithub-Signature-256: sha256=<hex>`) matches the
//! GitHub webhook HMAC scheme so existing receiver tooling works unchanged.

use hmac::{Hmac, Mac};
use rithub_core::{Webhook, WebhookStore};
use sha2::Sha256;

use crate::state::AppState;

/// Signature header receivers check.
const SIGNATURE_HEADER: &str = "X-Rithub-Signature-256";
/// Event-name header.
const EVENT_HEADER: &str = "X-Rithub-Event";
/// Max delivery attempts per subscription (in the inline retry loop within one
/// job execution — outer retries are handled by the outbox backoff).
const MAX_ATTEMPTS: i32 = 3;

/// Attempt delivery to one subscription with a small bounded retry, then
/// record the outcome in `webhook_deliveries`.
///
/// `pub(crate)` — called by the jobs worker, not by HTTP handlers.
pub(crate) async fn deliver(state: &AppState, hook: &Webhook, event: &str, body: &[u8]) {
    let signature = sign(&hook.secret, body);

    let mut attempts = 0;
    let mut last_status: Option<i32> = None;
    let mut last_error: Option<String> = None;
    let mut success = false;

    // Resolve the target once at delivery time, refuse any internal answer,
    // and connect only to the validated addresses (no second lookup, so DNS
    // rebinding between the check and the connect cannot reach an internal
    // address). Redirects are never followed; retries reuse the same pin.
    let target = match pinned_target(&hook.url, webhook_policy(), &egress_guard::SystemResolver).await {
        Ok(target) => Some(target),
        Err(reason) => {
            last_error = Some(reason);
            attempts = MAX_ATTEMPTS;
            None
        }
    };

    while let Some((url, client)) = target.as_ref().filter(|_| attempts < MAX_ATTEMPTS) {
        attempts += 1;
        let result = client
            .post(url.clone())
            .header(EVENT_HEADER, event)
            .header(SIGNATURE_HEADER, format!("sha256={signature}"))
            .header("content-type", "application/json")
            .body(body.to_vec())
            .send()
            .await;
        match result {
            Ok(response) => {
                let code = response.status().as_u16() as i32;
                last_status = Some(code);
                if (200..300).contains(&code) {
                    success = true;
                    break;
                }
                last_error = Some(format!("non-2xx status {code}"));
            }
            Err(error) => last_error = Some(error.to_string()),
        }
        if attempts < MAX_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(200 * attempts as u64)).await;
        }
    }

    if let Err(error) = state
        .store
        .record_delivery(
            hook.id,
            event,
            success,
            last_status,
            attempts,
            last_error.as_deref(),
        )
        .await
    {
        tracing::warn!(%error, "failed to record webhook delivery");
    }
}

/// Whether webhooks may target private/loopback networks
/// (`RITHUB_WEBHOOK_ALLOW_PRIVATE=1`, for self-hosted/internal receivers).
/// The connection is still pinned to the address resolved once.
fn allow_private_targets() -> bool {
    matches!(
        std::env::var("RITHUB_WEBHOOK_ALLOW_PRIVATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// `http`/`https`, no credentials, and (unless allowed) a public address
/// (private, loopback, link-local, metadata, CGNAT, IPv4-mapped, ULA and
/// other reserved ranges are refused; see `egress_guard::ip`).
fn webhook_policy() -> egress_guard::Policy {
    egress_guard::Policy { allow_http: true, allow_private: allow_private_targets() }
}

/// How long one delivery attempt may take.
const DELIVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Resolves the target once, refuses internal answers, and returns the URL
/// with a client pinned to the validated addresses (no redirects, no proxy).
async fn pinned_target(
    raw: &str,
    policy: egress_guard::Policy,
    resolver: &dyn egress_guard::Resolver,
) -> Result<(egress_guard::Url, reqwest::Client), String> {
    let pinned = egress_guard::resolve_pinned(raw, policy, resolver)
        .await
        .map_err(|reason| format!("blocked target: {reason}"))?;
    let client = egress_guard::pinned_client(&pinned, DELIVERY_TIMEOUT)
        .map_err(|e| format!("http client unavailable: {e}"))?;
    Ok((pinned.url, client))
}

/// Validate a webhook target URL when a hook is saved: `http`/`https` only,
/// no credentials, and (unless explicitly allowed) every address it resolves
/// to is public. Delivery checks again and pins what it checked.
pub(crate) async fn validate_target(raw: &str) -> Result<(), String> {
    let policy = webhook_policy();
    if policy.allow_private {
        return egress_guard::check_url(raw, policy).map(|_| ()).map_err(|e| e.to_string());
    }
    egress_guard::resolve_pinned(raw, policy, &egress_guard::SystemResolver)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Lowercase-hex HMAC-SHA256 of `body` under `secret`.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::io;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use egress_guard::{Policy, Resolver};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{pinned_target, validate_target};

    const STRICT: Policy = Policy { allow_http: true, allow_private: false };
    const LOCAL: Policy = Policy { allow_http: true, allow_private: true };

    /// Answers each lookup with the next list (the last repeats), counting calls.
    struct Script {
        answers: Mutex<Vec<Vec<SocketAddr>>>,
        calls: AtomicUsize,
    }

    impl Script {
        fn new(answers: Vec<Vec<String>>) -> Self {
            let answers = answers.into_iter().map(|a| a.into_iter().map(|s| s.parse().unwrap()).collect()).collect();
            Self { answers: Mutex::new(answers), calls: AtomicUsize::new(0) }
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

    #[tokio::test]
    async fn internal_targets_in_any_spelling_are_refused_without_dns() {
        for raw in [
            "http://2130706433/hook",
            "http://0177.0.0.1/hook",
            "http://0x7f.1/hook",
            "http://[::ffff:127.0.0.1]/hook",
            "http://[::ffff:a9fe:a9fe]/latest/meta-data",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/hook",
            "http://[fc00::1]/hook",
            "http://[fe80::1]/hook",
            "ftp://example.com/hook",
            "https://user:pw@example.com/hook",
        ] {
            let dns = Script::new(vec![vec!["93.184.216.34:80".into()]]);
            assert!(pinned_target(raw, STRICT, &dns).await.is_err(), "{raw}");
            assert_eq!(dns.calls.load(Ordering::SeqCst), 0, "{raw}: judged without a lookup");
        }
        assert!(validate_target("http://0177.0.0.1/hook").await.is_err());
    }

    #[tokio::test]
    async fn a_host_answering_with_an_internal_address_is_refused() {
        for answer in [vec!["10.0.0.7:443".to_owned()], vec!["93.184.216.34:443".to_owned(), "[::ffff:127.0.0.1]:443".to_owned()]] {
            let dns = Script::new(vec![answer.clone()]);
            assert!(pinned_target("https://hooks.example/x", STRICT, &dns).await.is_err(), "{answer:?}");
        }
    }

    /// DNS rebinding: the host resolves once and the delivery connects to that
    /// answer; `rebind.invalid` has no record anywhere, so reaching the server
    /// proves no second lookup happened (and retries reuse the pin).
    #[tokio::test]
    async fn delivery_connects_only_to_the_address_checked() {
        let (addr, hits) = server("200 OK", String::new()).await;
        let dns = Script::new(vec![vec![addr.to_string()], vec!["10.9.9.9:1".into()]]);
        let (url, client) = pinned_target(&format!("http://rebind.invalid:{}/hook", addr.port()), LOCAL, &dns).await.unwrap();
        for _ in 0..2 {
            assert_eq!(client.post(url.clone()).body("{}").send().await.unwrap().status().as_u16(), 200);
        }
        assert_eq!((hits.load(Ordering::SeqCst), dns.calls.load(Ordering::SeqCst)), (2, 1));
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_not_followed() {
        let (inner, inner_hits) = server("200 OK", String::new()).await;
        let (outer, _) = server("302 Found", format!("location: http://127.0.0.1:{}/admin\r\n", inner.port())).await;
        let dns = Script::new(vec![vec![outer.to_string()]]);
        let (url, client) = pinned_target(&format!("http://hooks.invalid:{}/hook", outer.port()), LOCAL, &dns).await.unwrap();
        assert_eq!(client.post(url).body("{}").send().await.unwrap().status().as_u16(), 302);
        assert_eq!(inner_hits.load(Ordering::SeqCst), 0);
    }
}
