//! Webhook delivery: HMAC-SHA256 signing + retrying HTTP POST, recorded in the
//! delivery audit table.
//!
//! Events are fired from the API handlers (issue.opened, pr.opened, pr.merged).
//! Each active subscription for the event gets an independent delivery attempt
//! in a background task, so a slow or failing receiver never blocks the request.
//! The signature header (`X-Rithub-Signature-256: sha256=<hex>`) lets receivers
//! verify authenticity — the same scheme GitHub uses.
//!
//! `push` events and a durable retry/DLQ queue are deliberately out of this
//! first cut (flagged for Phase 4); retries here are a small in-process loop.

use hmac::{Hmac, Mac};
use rithub_core::{Webhook, WebhookStore};
use sha2::Sha256;
use uuid::Uuid;

use crate::state::AppState;

/// Signature header receivers check.
const SIGNATURE_HEADER: &str = "X-Rithub-Signature-256";
/// Event name header.
const EVENT_HEADER: &str = "X-Rithub-Event";
/// Max delivery attempts per subscription.
const MAX_ATTEMPTS: i32 = 3;

/// Fire an event to every active subscription for `repo_id`. Non-blocking: each
/// subscription is delivered in its own background task. `payload` is a JSON
/// value describing the event.
pub fn dispatch(state: &AppState, repo_id: Uuid, event: &'static str, payload: serde_json::Value) {
    let state = state.clone();
    let body = serde_json::to_vec(&payload).unwrap_or_default();
    tokio::spawn(async move {
        let hooks = match state.store.webhooks_for_event(repo_id, event).await {
            Ok(hooks) => hooks,
            Err(error) => {
                tracing::warn!(%error, "failed to load webhooks for event");
                return;
            }
        };
        for hook in hooks {
            deliver(&state, &hook, event, &body).await;
        }
    });
}

/// Attempt delivery to one subscription with bounded retries, then record it.
async fn deliver(state: &AppState, hook: &Webhook, event: &str, body: &[u8]) {
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
        // Small linear backoff between attempts.
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
