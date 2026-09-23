//! Outbound webhooks — faithful port of `Webhooks::*` + `Webhook` (payload,
//! HMAC signature, headers) and `Webhooks::SendHttpService` (POST, status,
//! retry with backoff).
//!
//! Payload shape (`Webhooks::BaseService#call`):
//! `{ "webhook_type": "invoice.created", "object_type": "invoice",
//!    "organization_id": "<uuid>", "invoice": <serialized> }`. The signature is
//! `base64(HMAC-SHA256(org.hmac_key, payload_json))`, sent in
//! `X-Lago-Signature` with `X-Lago-Signature-Algorithm: hmac` and a unique key.
//!
//! The full 75-entry Lago webhook catalog lives in [`catalog`] ([`WebhookKind`]);
//! the bespoke non-model payload shapes (error / checkout / vies_check envelopes)
//! live in [`payloads`]. Prefer [`WebhookKind::build`] as the single per-type
//! emit entry point so every outbound webhook uses the correct
//! `webhook_type`/`object_type` pair.

use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::transport::HttpTransport;

pub mod catalog;
pub mod payloads;

pub use catalog::WebhookKind;

/// Webhook delivery status (`Webhook::STATUS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookStatus {
    Pending,
    Succeeded,
    Failed,
    Retrying,
}

impl WebhookStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WebhookStatus::Pending => "pending",
            WebhookStatus::Succeeded => "succeeded",
            WebhookStatus::Failed => "failed",
            WebhookStatus::Retrying => "retrying",
        }
    }
}

/// An outbound webhook — the persisted delivery record.
#[derive(Debug, Clone)]
pub struct Webhook {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub webhook_type: String,
    pub object_type: String,
    pub endpoint: String,
    pub payload: Value,
    pub status: WebhookStatus,
    pub http_status: Option<i32>,
    pub response: Option<String>,
    pub retries: i32,
    pub last_retried_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Webhook {
    /// Build a webhook from an event (`Webhooks::BaseService#call`).
    pub fn build(
        organization_id: Uuid,
        endpoint: impl Into<String>,
        webhook_type: impl Into<String>,
        object_type: impl Into<String>,
        object: Value,
    ) -> Self {
        let webhook_type = webhook_type.into();
        let object_type = object_type.into();
        // Envelope (`Webhooks::BaseService#call`): webhook_type, object_type,
        // organization_id, then the serialized object keyed by object_type.
        let payload = json!({
            "webhook_type": webhook_type,
            "object_type": object_type,
            "organization_id": organization_id,
            object_type.clone(): object,
        });
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            organization_id,
            webhook_type,
            object_type,
            endpoint: endpoint.into(),
            payload,
            status: WebhookStatus::Pending,
            http_status: None,
            response: None,
            retries: 0,
            last_retried_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// `Webhook#hmac_signature` — `base64(HMAC-SHA256(hmac_key, payload_json))`.
    pub fn hmac_signature(&self, hmac_key: &str) -> String {
        let body = serde_json::to_string(&self.payload).unwrap_or_default();
        let mut mac =
            Hmac::<Sha256>::new_from_slice(hmac_key.as_bytes()).expect("HMAC accepts any key size");
        mac.update(body.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    /// `Webhook#generate_headers`.
    pub fn headers(&self, hmac_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Lago-Signature".into(), self.hmac_signature(hmac_key)),
            ("X-Lago-Signature-Algorithm".into(), "hmac".into()),
            ("X-Lago-Unique-Key".into(), self.id.to_string()),
        ]
    }

    /// Default max attempts (`LAGO_WEBHOOK_ATTEMPTS`).
    pub const RETRY_LIMIT: i32 = 3;

    /// `SendHttpService#wait_value` — ActiveJob-style backoff (no jitter here,
    /// so the schedule is deterministic): `retries^4 + 2` seconds.
    pub fn backoff_seconds(&self) -> f64 {
        let e = self.retries as f64;
        (e.powi(4)) + 2.0
    }

    fn mark_succeeded(&mut self, http_status: u16, body: String) {
        self.http_status = Some(http_status as i32);
        self.response = Some(body);
        self.status = WebhookStatus::Succeeded;
        self.updated_at = Utc::now();
    }

    fn mark_unsuccessful(&mut self, http_status: Option<u16>, body: String, retrying: bool) {
        self.http_status = http_status.map(|s| s as i32);
        self.response = Some(body);
        self.retries += 1;
        self.last_retried_at = Some(Utc::now());
        self.status = if retrying {
            WebhookStatus::Retrying
        } else {
            WebhookStatus::Failed
        };
        self.updated_at = Utc::now();
    }
}

/// `Webhooks::SendHttpService#call` — deliver one webhook once, updating its
/// status. Returns whether the caller should re-enqueue a retry.
pub async fn deliver<T: HttpTransport>(
    transport: &T,
    webhook: &mut Webhook,
    hmac_key: &str,
) -> bool {
    let headers = webhook.headers(hmac_key);
    let body = serde_json::to_string(&webhook.payload).unwrap_or_default();

    match transport.post(&webhook.endpoint, &headers, body).await {
        Ok(resp) if resp.is_success() => {
            webhook.mark_succeeded(resp.status, resp.body);
            false
        }
        Ok(resp) => {
            let retrying = (webhook.retries + 1) < Webhook::RETRY_LIMIT;
            webhook.mark_unsuccessful(Some(resp.status), resp.body, retrying);
            retrying
        }
        Err(e) => {
            let retrying = (webhook.retries + 1) < Webhook::RETRY_LIMIT;
            webhook.mark_unsuccessful(None, e.to_string(), retrying);
            retrying
        }
    }
}
