//! KYC / identity-verification providers. The ONE place that talks to a KYC
//! provider's HTTP API, mirroring the payment adapters: a real client over the
//! injectable [`HttpTransport`] that resolves an account's *provider-verified*
//! status. `mint-reserve` owns the [`KycVerifier`] seam and the deny-by-default
//! [`mint_reserve::VerifiedKycGate`] that consumes it; here we implement the
//! concrete Persona and Stripe Identity lookups.
//!
//! Both are keyed on the account's stable `acct_…` id:
//! - **Persona**: `GET /api/v1/inquiries?filter[reference-id]=<acct>` with a
//!   `Bearer` API key + `Persona-Version` header. Each inquiry's `status` maps
//!   to a [`KycStatus`]; the strongest across all inquiries wins.
//! - **Stripe Identity**: `GET /v1/identity/verification_sessions/search` with
//!   `query=metadata['account_id']:'<acct>'` and a `Bearer` secret key. A
//!   `verified` session ⇒ approved.
//!
//! Env-key gated (BYOK): an empty key ⇒ [`KycError::NotConfigured`], and the
//! gate denies (fail closed). NO network + NO keys in tests (fake transport).

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::AccountId;
use mint_reserve::{KycError, KycStatus, KycTier, KycVerifier};
use serde_json::Value;

use crate::http::HttpTransport;

/// Persona REST base + pinned API version header value.
const PERSONA_API_BASE: &str = "https://withpersona.com/api/v1";
const PERSONA_API_VERSION: &str = "2023-01-05";
/// Stripe REST base (Identity verification sessions live under `/v1/identity`).
const STRIPE_API_BASE: &str = "https://api.stripe.com/v1";

/// A live [Persona](https://withpersona.com) identity verifier over an injected
/// [`HttpTransport`]. Resolves the account's inquiries by `reference-id`.
pub struct PersonaKycVerifier {
    transport: Arc<dyn HttpTransport>,
    api_key: String,
}

impl PersonaKycVerifier {
    /// Build the verifier from its transport + Persona API key.
    pub fn new(transport: Arc<dyn HttpTransport>, api_key: impl Into<String>) -> Self {
        PersonaKycVerifier {
            transport,
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl KycVerifier for PersonaKycVerifier {
    async fn verify(&self, account: &AccountId) -> Result<KycStatus, KycError> {
        if self.api_key.trim().is_empty() {
            return Err(KycError::NotConfigured);
        }
        let url = format!(
            "{PERSONA_API_BASE}/inquiries?filter%5Breference-id%5D={}&page%5Bsize%5D=25",
            pct_encode(account.as_str())
        );
        let headers = vec![
            (
                "authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            (
                "persona-version".to_string(),
                PERSONA_API_VERSION.to_string(),
            ),
            ("accept".to_string(), "application/json".to_string()),
        ];
        let resp = self
            .transport
            .get(&url, &headers)
            .await
            .map_err(|_| KycError::ProviderUnavailable)?;
        if !(200..300).contains(&resp.status) {
            return Err(KycError::ProviderUnavailable);
        }
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| KycError::MalformedResponse)?;
        let statuses = v["data"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|item| item["attributes"]["status"].as_str())
                .map(persona_status)
                .collect::<Vec<_>>()
        });
        Ok(best_status(statuses.unwrap_or_default()))
    }
}

/// Map a Persona inquiry `status` string to a [`KycStatus`].
fn persona_status(status: &str) -> KycStatus {
    match status {
        "approved" | "completed" => KycStatus::Approved(KycTier::Verified),
        "declined" | "failed" | "expired" => KycStatus::Declined,
        "pending" | "needs_review" | "created" => KycStatus::Pending,
        _ => KycStatus::Unverified,
    }
}

/// A live [Stripe Identity](https://stripe.com/identity) verifier over an
/// injected [`HttpTransport`]. Searches verification sessions by the
/// `metadata[account_id]` the product backend set when creating the session.
pub struct StripeIdentityKycVerifier {
    transport: Arc<dyn HttpTransport>,
    secret_key: String,
}

impl StripeIdentityKycVerifier {
    /// Build the verifier from its transport + Stripe secret key.
    pub fn new(transport: Arc<dyn HttpTransport>, secret_key: impl Into<String>) -> Self {
        StripeIdentityKycVerifier {
            transport,
            secret_key: secret_key.into(),
        }
    }
}

#[async_trait]
impl KycVerifier for StripeIdentityKycVerifier {
    async fn verify(&self, account: &AccountId) -> Result<KycStatus, KycError> {
        if self.secret_key.trim().is_empty() {
            return Err(KycError::NotConfigured);
        }
        let query = format!("metadata['account_id']:'{}'", account.as_str());
        let url = format!(
            "{STRIPE_API_BASE}/identity/verification_sessions/search?query={}",
            pct_encode(&query)
        );
        let headers = vec![
            (
                "authorization".to_string(),
                format!("Bearer {}", self.secret_key),
            ),
            ("accept".to_string(), "application/json".to_string()),
        ];
        let resp = self
            .transport
            .get(&url, &headers)
            .await
            .map_err(|_| KycError::ProviderUnavailable)?;
        if !(200..300).contains(&resp.status) {
            return Err(KycError::ProviderUnavailable);
        }
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| KycError::MalformedResponse)?;
        let statuses = v["data"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|item| item["status"].as_str())
                .map(stripe_status)
                .collect::<Vec<_>>()
        });
        Ok(best_status(statuses.unwrap_or_default()))
    }
}

/// Map a Stripe Identity verification-session `status` to a [`KycStatus`].
fn stripe_status(status: &str) -> KycStatus {
    match status {
        "verified" => KycStatus::Approved(KycTier::Verified),
        "requires_input" => KycStatus::Declined,
        "processing" => KycStatus::Pending,
        _ => KycStatus::Unverified,
    }
}

/// Rank a status so the strongest standing across several provider records wins:
/// an approval anywhere beats a pending, which beats an explicit decline, which
/// beats "no record".
fn rank(status: &KycStatus) -> u8 {
    match status {
        KycStatus::Approved(_) => 3,
        KycStatus::Pending => 2,
        KycStatus::Declined => 1,
        KycStatus::Unverified => 0,
    }
}

/// Reduce a provider's list of per-record statuses to the account's strongest
/// standing. Empty (no records) ⇒ [`KycStatus::Unverified`].
fn best_status(statuses: Vec<KycStatus>) -> KycStatus {
    statuses
        .into_iter()
        .max_by_key(rank)
        .unwrap_or(KycStatus::Unverified)
}

/// Percent-encode a query-string value, escaping everything outside the RFC 3986
/// unreserved set. Keeps `filter[reference-id]` / Stripe search queries safe
/// without pulling in a URL crate.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto_tr::test_support::FakeTransport;
    use mint_reserve::{KycGate, VerifiedKycGate};

    fn acct() -> AccountId {
        // A fixed, valid `acct_` id (user kind `0` + a canonical ULID) so the
        // tests need no `ulid` dev-dependency.
        AccountId::parse("acct_001ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid account id")
    }

    #[tokio::test]
    async fn persona_approved_inquiry_is_verified() {
        let transport = FakeTransport::new()
            .route(
                "withpersona.com",
                200,
                serde_json::json!({"data":[{"attributes":{"status":"approved"}}]}),
            )
            .arc();
        let verifier = PersonaKycVerifier::new(transport, "persona_live_key");
        assert_eq!(
            verifier.verify(&acct()).await,
            Ok(KycStatus::Approved(KycTier::Verified))
        );
    }

    #[tokio::test]
    async fn persona_no_inquiries_is_unverified() {
        let transport = FakeTransport::new()
            .route("withpersona.com", 200, serde_json::json!({"data":[]}))
            .arc();
        let verifier = PersonaKycVerifier::new(transport, "persona_live_key");
        assert_eq!(verifier.verify(&acct()).await, Ok(KycStatus::Unverified));
    }

    #[tokio::test]
    async fn persona_picks_strongest_status() {
        // A stale declined inquiry plus a fresh approval ⇒ approved.
        let transport = FakeTransport::new()
            .route(
                "withpersona.com",
                200,
                serde_json::json!({"data":[
                    {"attributes":{"status":"declined"}},
                    {"attributes":{"status":"approved"}}
                ]}),
            )
            .arc();
        let verifier = PersonaKycVerifier::new(transport, "persona_live_key");
        assert_eq!(
            verifier.verify(&acct()).await,
            Ok(KycStatus::Approved(KycTier::Verified))
        );
    }

    #[tokio::test]
    async fn missing_key_is_not_configured_and_makes_no_call() {
        let transport = FakeTransport::failing();
        let verifier = PersonaKycVerifier::new(transport, "");
        assert_eq!(verifier.verify(&acct()).await, Err(KycError::NotConfigured));
    }

    #[tokio::test]
    async fn transport_outage_is_provider_unavailable() {
        let verifier = PersonaKycVerifier::new(FakeTransport::failing(), "persona_live_key");
        assert_eq!(
            verifier.verify(&acct()).await,
            Err(KycError::ProviderUnavailable)
        );
    }

    #[tokio::test]
    async fn stripe_identity_verified_session_is_approved() {
        let transport = FakeTransport::new()
            .route(
                "api.stripe.com",
                200,
                serde_json::json!({"object":"search_result","data":[{"status":"verified"}]}),
            )
            .arc();
        let verifier = StripeIdentityKycVerifier::new(transport, "sk_live_x");
        assert_eq!(
            verifier.verify(&acct()).await,
            Ok(KycStatus::Approved(KycTier::Verified))
        );
    }

    #[tokio::test]
    async fn stripe_identity_requires_input_is_declined() {
        let transport = FakeTransport::new()
            .route(
                "api.stripe.com",
                200,
                serde_json::json!({"data":[{"status":"requires_input"}]}),
            )
            .arc();
        let verifier = StripeIdentityKycVerifier::new(transport, "sk_live_x");
        assert_eq!(verifier.verify(&acct()).await, Ok(KycStatus::Declined));
    }

    #[tokio::test]
    async fn end_to_end_verified_gate_over_a_live_verifier() {
        // The production wiring: a VerifiedKycGate driving a real provider
        // verifier admits an approved account and denies an unverified one, with
        // NO trust placed in the caller-presented tier.
        let approved = FakeTransport::new()
            .route(
                "withpersona.com",
                200,
                serde_json::json!({"data":[{"attributes":{"status":"approved"}}]}),
            )
            .arc();
        let gate = VerifiedKycGate::new(
            PersonaKycVerifier::new(approved, "persona_live_key"),
            KycTier::Verified,
            1_000_000_000,
        );
        assert!(gate
            .authorize(&acct(), KycTier::None, mint_core::Micros::new(10_000_000))
            .await
            .is_ok());

        let none = FakeTransport::new()
            .route("withpersona.com", 200, serde_json::json!({"data":[]}))
            .arc();
        let gate = VerifiedKycGate::new(
            PersonaKycVerifier::new(none, "persona_live_key"),
            KycTier::Verified,
            1_000_000_000,
        );
        assert_eq!(
            gate.authorize(&acct(), KycTier::Verified, mint_core::Micros::new(1))
                .await,
            Err(mint_core::MintError::KycRequired)
        );
    }
}
