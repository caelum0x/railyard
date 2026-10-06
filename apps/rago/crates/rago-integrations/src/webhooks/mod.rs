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
pub mod jwt;
pub mod payloads;

pub use jwt::JwtSigner;

impl Signing<'_> {
    /// The signing for an endpoint's stored `signature_algo`: `jwt` uses the
    /// deployment key when one is configured, everything else HMAC.
    pub fn for_algo<'a>(algo: &str, hmac_key: &'a str, signer: Option<&'a JwtSigner>) -> Signing<'a> {
        match (algo, signer) {
            ("jwt", Some(s)) => Signing::Jwt(s),
            _ => Signing::Hmac(hmac_key),
        }
    }
}

/// How an endpoint's deliveries are signed (`webhook_endpoints.signature_algo`).
#[derive(Debug, Clone, Copy)]
pub enum Signing<'a> {
    /// `base64(HMAC-SHA256(org.hmac_key, payload))`.
    Hmac(&'a str),
    /// RS256 JWT over `{data, iss}` with the deployment key.
    Jwt(&'a JwtSigner),
}

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
        self.signed_headers(Signing::Hmac(hmac_key))
    }

    /// `Webhook#generate_headers` for either signature algorithm.
    pub fn signed_headers(&self, signing: Signing<'_>) -> Vec<(String, String)> {
        let (signature, algo) = match signing {
            Signing::Hmac(key) => (self.hmac_signature(key), "hmac"),
            Signing::Jwt(signer) => {
                (signer.sign(&serde_json::to_string(&self.payload).unwrap_or_default()), "jwt")
            }
        };
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Lago-Signature".into(), signature),
            ("X-Lago-Signature-Algorithm".into(), algo.into()),
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

/// What an outbound webhook endpoint may be: `http` or `https`, no
/// credentials, and a public address. `RAGO_WEBHOOK_ALLOW_PRIVATE=1` lifts the
/// address rule for self-hosted or local receivers (still pinned).
pub fn webhook_policy() -> egress_guard::Policy {
    let allow_private = matches!(
        std::env::var("RAGO_WEBHOOK_ALLOW_PRIVATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    egress_guard::Policy { allow_http: true, allow_private }
}

/// `Webhooks::SendHttpService#call` — deliver one webhook once, updating its
/// status. Returns whether the caller should re-enqueue a retry.
///
/// The endpoint is checked first (shape, and any address literal in all its
/// spellings); a refused endpoint fails without a retry. The POST goes
/// through [`HttpTransport::post_external`], which on the live transport
/// resolves once, refuses internal answers and pins the connection.
pub async fn deliver<T: HttpTransport>(
    transport: &T,
    webhook: &mut Webhook,
    hmac_key: &str,
) -> bool {
    deliver_signed(transport, webhook, Signing::Hmac(hmac_key)).await
}

/// [`deliver`] with the endpoint's chosen signature algorithm.
pub async fn deliver_signed<T: HttpTransport>(
    transport: &T,
    webhook: &mut Webhook,
    signing: Signing<'_>,
) -> bool {
    if let Err(refused) = egress_guard::check_url(&webhook.endpoint, webhook_policy()) {
        webhook.mark_unsuccessful(None, format!("blocked target: {refused}"), false);
        return false;
    }
    let headers = webhook.signed_headers(signing);
    let body = serde_json::to_string(&webhook.payload).unwrap_or_default();

    match transport.post_external(&webhook.endpoint, &headers, body).await {
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
