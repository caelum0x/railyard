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
    let client = reqwest::Client::new();

    let mut attempts = 0;
    let mut last_status: Option<i32> = None;
    let mut last_error: Option<String> = None;
    let mut success = false;

    while attempts < MAX_ATTEMPTS {
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
