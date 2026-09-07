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
use std::time::Duration;

use aitp_client::codec;
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge};
use rand::RngCore;

use crate::challenge::ChallengeIssuer;
use crate::clock::now_ms;
use crate::error::TurnstileError;
use crate::journal::{InMemorySettlementJournal, SettlementJournal};
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
    settlements: Arc<dyn SettlementJournal>,
    settlement_lease: Duration,
}

impl Turnstile {
    #[must_use]
    pub fn new(
        issuer: ChallengeIssuer,
        verifier: Arc<dyn PaymentVerifier>,
        nonces: Arc<dyn NonceStore>,
    ) -> Self {
        Self::with_journal(
            issuer,
            verifier,
            nonces,
            Arc::new(InMemorySettlementJournal::new()),
            Duration::from_secs(30),
        )
    }

    #[must_use]
    pub fn with_journal(
        issuer: ChallengeIssuer,
        verifier: Arc<dyn PaymentVerifier>,
        nonces: Arc<dyn NonceStore>,
        settlements: Arc<dyn SettlementJournal>,
        settlement_lease: Duration,
    ) -> Self {
        Self {
            issuer,
            verifier,
            nonces,
            settlements,
            settlement_lease,
        }
    }

    fn lease_until(&self) -> i64 {
        now_ms().saturating_add(self.settlement_lease.as_millis() as i64)
    }

    fn owner_token() -> String {
        let mut bytes = [0_u8; 24];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    /// Issue a fresh challenge for `resource` (used for `402` responses).
    ///
    /// Fallible because registering the challenge's single-use nonce may touch a
    /// durable store; the in-memory store never errors.
    pub async fn challenge(&self, resource: &str) -> Result<PaymentChallenge, TurnstileError> {
        self.issuer.issue(resource).await
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
            return match self.issuer.issue(resource).await {
                Ok(challenge) => Admit::Challenge(challenge),
                Err(e) => Admit::Rejected(e),
            };
        };

        let auth = match codec::decode_authorization(header) {
            Ok(auth) => auth,
            Err(e) => return Admit::Rejected(TurnstileError::InvalidAuthorization(e.to_string())),
        };

        // Bind to the exact challenge owned by this nonce in shared state. A
        // replica's current config is not proof of what another replica issued.
        let issued = match self.nonces.get(&auth.nonce).await {
            Ok(Some(challenge)) => challenge,
            Ok(None) => return Admit::Rejected(TurnstileError::NonceReplayed(auth.nonce.clone())),
            Err(error) => return Admit::Rejected(error),
        };
        if let Err(e) = self.validate(&auth, resource, &issued) {
            return Admit::Rejected(e);
        }

        // Single-use replay guard at the edge, BEFORE the mint round-trip so a
        // replayed nonce is rejected even if it would otherwise verify.
        let consumed = match self.nonces.consume(&auth.nonce, now_ms()).await {
            Ok(challenge) => challenge,
            Err(error) => return Admit::Rejected(error),
        };
        if consumed != issued {
            return Admit::Rejected(TurnstileError::ChallengeMismatch(
                "challenge ownership changed while authorizing".to_string(),
            ));
        }

        // Persist the exact Mint verification command before the verifier can
        // create a hold. A recovery worker can replay this authorization after
        // the request lease expires without running the protected handler.
        let owner_token = Self::owner_token();
        if let Err(error) = self
            .settlements
            .begin_verification(&auth, &owner_token, self.lease_until())
            .await
        {
            return Admit::Rejected(error);
        }

        // Forward the agent's already-signed authorization to the live mint.
        match self.verifier.verify(&auth).await {
            Ok(verified) => {
                let record = match self
                    .settlements
                    .bind_verified(
                        &auth.nonce,
                        &owner_token,
                        verified.hold.as_ref(),
                        verified.scheme,
                        self.lease_until(),
                    )
                    .await
                {
                    Ok(record) => record,
                    Err(error) => return Admit::Rejected(error),
                };
                Admit::Admitted(Settled::new(
                    self.verifier.clone(),
                    self.settlements.clone(),
                    record,
                    owner_token,
                    self.settlement_lease,
                ))
            }
            Err(error) => {
                let text = error.to_string();
                let result = if verification_may_have_created_hold(&error) {
                    self.settlements
                        .record_error(&auth.nonce, &owner_token, &text)
                        .await
                } else {
                    self.settlements
                        .reject_verification(&auth.nonce, &owner_token, &text)
                        .await
                };
                Admit::Rejected(result.err().unwrap_or(error))
            }
        }
    }

    /// Check the authorization matches the resource/payee/price we quoted and is
    /// not expired. Cryptographic verification is the mint's job (forwarded).
    fn validate(
        &self,
        auth: &PaymentAuthorization,
        resource: &str,
        issued: &PaymentChallenge,
    ) -> Result<(), TurnstileError> {
        if issued.resource != resource || auth.resource != issued.resource {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "resource {:?} does not own nonce for {:?}",
                auth.resource, issued.resource
            )));
        }
        if auth.payee != issued.payee {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "payee {:?} != challenged {:?}",
                auth.payee, issued.payee
            )));
        }
        if auth.amount_micros != issued.amount_micros {
            return Err(TurnstileError::ChallengeMismatch(format!(
                "amount {} != challenged {}",
                auth.amount_micros.get(),
                issued.amount_micros.get()
            )));
        }
        if auth.scheme != issued.scheme {
            return Err(TurnstileError::ChallengeMismatch(
                "authorization scheme differs from challenge".to_string(),
            ));
        }
        if auth.expiry != issued.expiry {
            return Err(TurnstileError::ChallengeMismatch(
                "authorization expiry differs from challenge".to_string(),
            ));
        }
        if auth.expiry < now_ms() {
            return Err(TurnstileError::Pay(PayError::AuthorizationExpired));
        }
        Ok(())
    }
}

pub(crate) fn verification_may_have_created_hold(error: &TurnstileError) -> bool {
    matches!(
        error,
        TurnstileError::Pay(PayError::Gateway(_) | PayError::Codec(_)) | TurnstileError::Storage(_)
    )
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
            scheme: ch.scheme,
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

        let challenge = gate.challenge("POST /v1/echo").await.unwrap();
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
    async fn successful_zero_usage_releases_instead_of_creating_a_zero_capture() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_zero"));
        let gate = gate(verifier.clone());
        let challenge = gate.challenge("POST /v1/echo").await.unwrap();
        let header = encode_authorization(&authorization_for(&challenge)).unwrap();

        match gate.admit(Some(&header), "POST /v1/echo").await {
            Admit::Admitted(settled) => {
                assert!(settled.capture(AmountMicros::ZERO).await.unwrap().is_none());
            }
            _ => panic!("expected admitted"),
        }

        let calls = verifier.snapshot();
        assert!(calls.captured.is_empty());
        assert_eq!(calls.released, vec!["hold_zero"]);
    }

    #[tokio::test]
    async fn replayed_nonce_is_rejected() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier);

        let challenge = gate.challenge("POST /v1/echo").await.unwrap();
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
        let challenge = gate.challenge("POST /v1/echo").await.unwrap();
        let header = encode_authorization(&authorization_for(&challenge)).unwrap();

        assert!(matches!(
            gate.admit(Some(&header), "POST /v1/other").await,
            Admit::Rejected(TurnstileError::ChallengeMismatch(_))
        ));
    }

    #[tokio::test]
    async fn altered_expiry_is_rejected_without_burning_owned_challenge() {
        let verifier = Arc::new(FakeVerifier::with_hold("hold_1"));
        let gate = gate(verifier.clone());

        let challenge = gate.challenge("POST /v1/echo").await.unwrap();
        let mut auth = authorization_for(&challenge);
        auth.expiry += 30_000;
        let altered = encode_authorization(&auth).unwrap();

        assert!(matches!(
            gate.admit(Some(&altered), "POST /v1/echo").await,
            Admit::Rejected(TurnstileError::ChallengeMismatch(_))
        ));
        // The mismatch did not consume another caller's valid owned challenge.
        let legitimate = encode_authorization(&authorization_for(&challenge)).unwrap();
        assert!(matches!(
            gate.admit(Some(&legitimate), "POST /v1/echo").await,
            Admit::Admitted(_)
        ));
        assert_eq!(verifier.snapshot().verified, 1);
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
