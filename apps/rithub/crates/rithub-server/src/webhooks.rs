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

    // Re-validate the target at delivery time (defeats DNS rebinding between
    // registration and delivery) and never follow redirects to another host.
    let client = match validate_target(&hook.url).await {
        Ok(()) => match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(client) => Some(client),
            Err(e) => {
                last_error = Some(format!("http client unavailable: {e}"));
                None
            }
        },
        Err(reason) => {
            last_error = Some(format!("blocked target: {reason}"));
            attempts = MAX_ATTEMPTS;
            None
        }
    };

    while let Some(client) = client.as_ref().filter(|_| attempts < MAX_ATTEMPTS) {
        attempts += 1;
        let result = client
            .post(&hook.url)
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
fn allow_private_targets() -> bool {
    matches!(
        std::env::var("RITHUB_WEBHOOK_ALLOW_PRIVATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Whether an address is loopback, private, link-local, or otherwise internal.
fn is_internal(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                // 100.64.0.0/10 carrier-grade NAT
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(mapped));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Validate a webhook target URL: `http`/`https` only, and (unless explicitly
/// allowed) it must not resolve to an internal address — this is the SSRF guard
/// applied both when a hook is saved and again before each delivery.
pub(crate) async fn validate_target(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| "not a valid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }
    let host = url.host_str().ok_or_else(|| "URL has no host".to_string())?;
    if allow_private_targets() {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let host_for_lookup = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host_for_lookup, port))
        .await
        .map_err(|_| format!("cannot resolve host '{host}'"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("cannot resolve host '{host}'"));
    }
    if addrs.iter().any(|a| is_internal(a.ip())) {
        return Err(format!(
            "host '{host}' resolves to a private or loopback address"
        ));
    }
    Ok(())
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
