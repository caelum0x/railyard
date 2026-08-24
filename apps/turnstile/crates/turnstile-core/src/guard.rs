//! The paywall guard: [`Turnstile::admit`].
//!
//! `admit` is the single framework-agnostic entry point. Given the inbound
//! `AITP-Authorization` header (if any) and the resource being requested it
//! returns one of three outcomes:
//! - [`Admit::Challenge`] — no/absent authorization → emit a `402` challenge;
//! - [`Admit::Admitted`] — a valid, replay-checked, mint-verified payment → a
//!   [`Settled`] hold the edge captures on success / releases on failure;
//! - [`Admit::Rejected`] — a malformed, mismatched, replayed, expired, or
//!   mint-rejected authorization.

use std::sync::Arc;

use aitp_client::codec;
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge};

use crate::challenge::ChallengeIssuer;
use crate::clock::now_ms;
use crate::error::TurnstileError;
use crate::nonce::NonceStore;
use crate::settle::Settled;
use crate::verifier::PaymentVerifier;

/// The outcome of [`Turnstile::admit`].
pub enum Admit {
    /// Respond `402 Payment Required` carrying this challenge.
    Challenge(PaymentChallenge),
    /// Payment accepted; run the handler, then capture/release this hold.
    Admitted(Settled),
    /// Payment present but invalid; respond `402` (with the reason) or an error.
    Rejected(TurnstileError),
}

/// The paywall engine: issues challenges, replay-checks nonces, forwards
/// authorizations to the mint, and hands back a settlement handle.
pub struct Turnstile {
    issuer: ChallengeIssuer,
    verifier: Arc<dyn PaymentVerifier>,
    nonces: Arc<dyn NonceStore>,
}

impl Turnstile {
    #[must_use]
    pub fn new(
        issuer: ChallengeIssuer,
        verifier: Arc<dyn PaymentVerifier>,
        nonces: Arc<dyn NonceStore>,
    ) -> Self {
        Self {
            issuer,
            verifier,
            nonces,
        }
    }

    /// Issue a fresh challenge for `resource` (used for `402` responses).
    #[must_use]
    pub fn challenge(&self, resource: &str) -> PaymentChallenge {
        self.issuer.issue(resource)
    }

    /// The quoted price per protected call.
    #[must_use]
    pub fn price(&self) -> AmountMicros {
        self.issuer.price()
    }

    /// Admit or challenge a request for `resource`.
    ///
    /// `authorization` is the raw `AITP-Authorization` header value, or `None`
    /// when the caller sent no payment (the common first-request case).
    pub async fn admit(&self, authorization: Option<&str>, resource: &str) -> Admit {
        let Some(header) = authorization else {
            return Admit::Challenge(self.issuer.issue(resource));
        };

        let auth = match codec::decode_authorization(header) {
            Ok(auth) => auth,
            Err(e) => return Admit::Rejected(TurnstileError::InvalidAuthorization(e.to_string())),
        };

        // Bind the authorization to what we actually challenged.
        if let Err(e) = self.validate(&auth, resource) {
            return Admit::Rejected(e);
        }

        // Single-use replay guard at the edge, BEFORE the mint round-trip so a
        // replayed nonce is rejected even if it would otherwise verify.
        if let Err(e) = self.nonces.consume(&auth.nonce, now_ms()) {
            return Admit::Rejected(e);
        }

        // Forward the agent's already-signed authorization to the live mint.
        match self.verifier.verify(&auth).await {
            Ok(verified) => Admit::Admitted(Settled::new(
                self.verifier.clone(),
                verified.hold,
                verified.scheme,
            )),
            Err(e) => Admit::Rejected(e),
        }
    }

    /// Check the authorization matches the resource/payee/price we quoted and is
    /// not expired. Cryptographic verification is the mint's job (forwarded).
    fn validate(&self, auth: &PaymentAuthorization, resource: &str) -> Result<(), TurnstileError> {
        let config = self.issuer.config();

        if auth.resource != resource {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "resource {:?} != requested {:?}",
                auth.resource, resource
            )));
        }
        if auth.payee != config.payee {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "payee {:?} != configured {:?}",
                auth.payee, config.payee
            )));
        }
        if auth.amount_micros < config.price_micros {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "amount {} below price {}",
                auth.amount_micros.get(),
                config.price_micros.get()
            )));
        }
        if auth.expiry < now_ms() {
            return Err(TurnstileError::Pay(PayError::AuthorizationExpired));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TurnstileConfig;
    use crate::nonce::InMemoryNonceStore;
    use crate::verifier::FakeVerifier;
    use aitp_client::codec::encode_authorization;
    use aitp_client::PaymentAuthorization;

    fn config() -> TurnstileConfig {
        TurnstileConfig {
            mint_server_url: "http://mint.internal".into(),
            api_key: "k".into(),
            payee: "ait1qpayee".into(),
            price_micros: AmountMicros(1_000),
            currency: "AIT".into(),
            challenge_ttl_ms: 60_000,
        }
    }

    fn gate(verifier: Arc<FakeVerifier>) -> Turnstile {
        let nonces = Arc::new(InMemoryNonceStore::new());
        let issuer = ChallengeIssuer::new(config(), nonces.clone());
        Turnstile::new(issuer, verifier, nonces)
    }

    fn authorization_for(ch: &PaymentChallenge) -> PaymentAuthorization {
        PaymentAuthorization {
            payer: "acct_payer".into(),
            payee: ch.payee.clone(),
            amount_micros: ch.amount_micros,
            nonce: ch.nonce.clone(),
            resource: ch.resource.clone(),
            expiry: ch.expiry,
            idempotency_key: "idem_1".into(),
            pubkey_id: "key_1".into(),
            op_hash: String::new(),
            sig: String::new(),
        }
    }

    #[tokio::test]
    async fn missing_authorization_yields_a_challenge() {
        let gate = gate(Arc::new(FakeVerifier::with_hold("hold_1")));
        match gate.admit(None, "POST /v1/echo").await {
            Admit::Challenge(ch) => assert_eq!(ch.resource, "POST /v1/echo"),
            _ => panic!("expected a challenge"),
        }
    }

    #[tokio::test]
    async fn valid_authorization_is_admitted_and_captures_actual() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier.clone());

        let challenge = gate.challenge("POST /v1/echo");
        let header = encode_authorization(&authorization_for(&challenge)).unwrap();

        match gate.admit(Some(&header), "POST /v1/echo").await {
            Admit::Admitted(settled) => {
                let receipt = settled.capture(AmountMicros(900)).await.unwrap();
                assert_eq!(receipt.unwrap().amount_micros, AmountMicros(900));
            }
            _ => panic!("expected admitted"),
        }

        let calls = verifier.snapshot();
        assert_eq!(calls.verified, 1);
        assert_eq!(calls.captured, vec![("hold_1".to_string(), 900)]);
    }

    #[tokio::test]
    async fn replayed_nonce_is_rejected() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier);

        let challenge = gate.challenge("POST /v1/echo");
        let header = encode_authorization(&authorization_for(&challenge)).unwrap();

        // First use consumes the nonce.
        assert!(matches!(
            gate.admit(Some(&header), "POST /v1/echo").await,
            Admit::Admitted(_)
        ));
        // Second use with the same nonce is a replay.
        assert!(matches!(
            gate.admit(Some(&header), "POST /v1/echo").await,
            Admit::Rejected(TurnstileError::NonceReplayed(_))
        ));
    }

    #[tokio::test]
    async fn resource_mismatch_is_rejected() {
        let gate = gate(Arc::new(FakeVerifier::with_hold("hold_1")));
        let challenge = gate.challenge("POST /v1/echo");
        let header = encode_authorization(&authorization_for(&challenge)).unwrap();

        assert!(matches!(
            gate.admit(Some(&header), "POST /v1/other").await,
            Admit::Rejected(TurnstileError::ChallengeMismatch(_))
        ));
    }

    #[tokio::test]
    async fn expired_authorization_is_rejected_pre_mint() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier.clone());

        let challenge = gate.challenge("POST /v1/echo");
        let mut auth = authorization_for(&challenge);
        auth.expiry = now_ms() - 1; // already expired
        let header = encode_authorization(&auth).unwrap();

        assert!(matches!(
            gate.admit(Some(&header), "POST /v1/echo").await,
            Admit::Rejected(TurnstileError::Pay(PayError::AuthorizationExpired))
        ));
        // Rejected at the edge — no mint round-trip.
        assert_eq!(verifier.snapshot().verified, 0);
    }

    #[tokio::test]
    async fn malformed_authorization_header_is_rejected() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier.clone());

        assert!(matches!(
            gate.admit(Some("not-base64url!!"), "POST /v1/echo").await,
            Admit::Rejected(TurnstileError::InvalidAuthorization(_))
        ));
        assert_eq!(verifier.snapshot().verified, 0);
    }
}
