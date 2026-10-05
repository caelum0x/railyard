//! The destination rules end to end: address spellings, one pinned
//! resolution (DNS rebinding), and redirects that are never followed.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egress_guard::{check_url, resolve_pinned, Policy, Resolver};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const STRICT: Policy = Policy { allow_http: true, allow_private: false };
/// Loopback test servers need the address rule lifted; pinning still applies.
const LOCAL: Policy = Policy { allow_http: true, allow_private: true };

/// Answers each lookup with the next list (the last one repeats), counting calls.
struct Script {
    answers: Mutex<Vec<Vec<SocketAddr>>>,
    calls: AtomicUsize,
}

impl Script {
    fn new(answers: Vec<Vec<&str>>) -> Self {
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

/// A one-route HTTP server on loopback answering `status` with `extra`
/// headers; returns its address and a hit counter.
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

#[test]
fn address_spellings_are_judged_as_the_address_they_name() {
    for raw in [
        "http://2130706433/",                  // decimal 127.0.0.1
        "http://0177.0.0.1/",                  // octal
        "http://0x7f.1/",                      // hex, short form
        "http://017700000001/",                // octal, one number
        "http://127.1/",                       // short form
        "http://[::ffff:127.0.0.1]/",          // IPv4-mapped
        "http://[::ffff:7f00:1]/",             // IPv4-mapped, hex
        "http://[::ffff:a9fe:a9fe]/latest",    // mapped metadata
        "https://169.254.169.254/latest/meta-data",
        "http://0251.0376.0251.0376/",         // octal metadata
        "http://[fd00:ec2::254]/",             // AWS IPv6 metadata (ULA)
        "http://[fe80::1%25eth0]/",            // link-local with a zone
        "http://100.100.100.200/",             // CGNAT metadata
        "http://[::]/",
        "http://0/",
    ] {
        assert!(check_url(raw, STRICT).is_err(), "{raw}");
    }
    for raw in ["https://hooks.example.com/x", "http://8.8.8.8/", "https://[2606:4700:4700::1111]/"] {
        assert!(check_url(raw, STRICT).is_ok(), "{raw}");
    }
    for raw in ["ftp://example.com/", "file:///etc/passwd", "https://user:pw@example.com/", "not a url"] {
        assert!(check_url(raw, STRICT).is_err(), "{raw}");
    }
    let https_only = Policy { allow_http: false, allow_private: false };
    assert!(check_url("http://example.com/", https_only).is_err());
}

#[tokio::test]
async fn any_internal_answer_refuses_the_host() {
    for answer in [vec!["10.0.0.5:443"], vec!["93.184.216.34:443", "127.0.0.1:443"], vec!["[::ffff:10.0.0.1]:443"], vec!["[fc00::1]:443"]] {
        let dns = Script::new(vec![answer.clone()]);
        assert!(resolve_pinned("https://hooks.example.com/x", STRICT, &dns).await.is_err(), "{answer:?}");
    }
    let dns = Script::new(vec![vec![]]);
    assert!(resolve_pinned("https://hooks.example.com/x", STRICT, &dns).await.is_err(), "no answer");
}

/// DNS rebinding: the first answer is public, later ones internal. The host
/// is resolved once and the validated address is what is pinned.
#[tokio::test]
async fn the_first_validated_answer_is_pinned_and_never_looked_up_again() {
    let dns = Script::new(vec![vec!["93.184.216.34:443"], vec!["127.0.0.1:443"]]);
    let pinned = resolve_pinned("https://rebind.example/hook", STRICT, &dns).await.unwrap();
    assert_eq!(pinned.addrs, vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]);
    assert_eq!(pinned.domain.as_deref(), Some("rebind.example"));
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
}

/// The client connects to the pinned address, not to a fresh lookup:
/// `rebind.invalid` has no DNS record anywhere, so reaching the server proves
/// no second resolution happened.
#[tokio::test]
async fn the_pinned_client_connects_only_to_the_validated_address() {
    let (addr, hits) = server("200 OK", String::new()).await;
    let dns = Script::new(vec![vec![&addr.to_string()], vec!["10.9.9.9:1"]]);
    let pinned = resolve_pinned(&format!("http://rebind.invalid:{}/hook", addr.port()), LOCAL, &dns).await.unwrap();
    let client = egress_guard::pinned_client(&pinned, Duration::from_secs(5)).unwrap();
    for _ in 0..2 {
        let resp = client.post(pinned.url.clone()).body("{}").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1, "retries reuse the pinned address");
}

/// A redirect is answered as is: never followed, to an internal address or anywhere.
#[tokio::test]
async fn redirects_to_internal_addresses_are_not_followed() {
    let (inner, inner_hits) = server("200 OK", String::new()).await;
    for location in [format!("http://127.0.0.1:{}/admin", inner.port()), "http://169.254.169.254/latest/meta-data".to_owned()] {
        let (outer, _) = server("302 Found", format!("location: {location}\r\n")).await;
        let dns = Script::new(vec![vec![&outer.to_string()]]);
        let pinned = resolve_pinned(&format!("http://hooks.invalid:{}/hook", outer.port()), LOCAL, &dns).await.unwrap();
        let client = egress_guard::pinned_client(&pinned, Duration::from_secs(5)).unwrap();
        let resp = client.post(pinned.url.clone()).body("{}").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 302, "{location}");
    }
    assert_eq!(inner_hits.load(Ordering::SeqCst), 0, "the redirect target was never contacted");
}
