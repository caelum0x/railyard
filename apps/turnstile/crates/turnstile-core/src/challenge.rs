//! Challenge issuance: mint a fresh [`PaymentChallenge`] for a protected
//! resource and register its single-use nonce (SPEC §5.1).

use std::sync::Arc;

use aitp_client::{AmountMicros, PaymentChallenge, Scheme};
use rand::RngCore;

use crate::clock::now_ms;
use crate::config::TurnstileConfig;
use crate::nonce::NonceStore;

/// Builds `402` challenges from config and registers each nonce for later
/// single-use verification.
pub struct ChallengeIssuer {
    config: TurnstileConfig,
    nonces: Arc<dyn NonceStore>,
}

impl ChallengeIssuer {
    #[must_use]
    pub fn new(config: TurnstileConfig, nonces: Arc<dyn NonceStore>) -> Self {
        Self { config, nonces }
    }

    /// The configuration backing this issuer (used by the guard to validate).
    #[must_use]
    pub fn config(&self) -> &TurnstileConfig {
        &self.config
    }

    /// The quoted price per protected call.
    #[must_use]
    pub fn price(&self) -> AmountMicros {
        self.config.price_micros
    }

    /// Issue a challenge for `resource` (e.g. `"POST /v1/echo"`), registering its
    /// nonce so a later authorization can be replay-checked and expired.
    ///
    /// The scheme is always `aitp-hold` — Turnstile reserves up to the quoted
    /// price and captures the ACTUAL usage after the handler runs (success-only).
    #[must_use]
    pub fn issue(&self, resource: &str) -> PaymentChallenge {
        let now = now_ms();
        let expiry = now + self.config.challenge_ttl_ms;
        let nonce = generate_nonce();
        self.nonces.register(&nonce, expiry);

        PaymentChallenge {
            payee: self.config.payee.clone(),
            amount_micros: self.config.price_micros,
            currency: self.config.currency.clone(),
            resource: resource.to_string(),
            nonce,
            expiry,
            scheme: Scheme::Hold,
        }
    }
}

/// A 128-bit random nonce as lowercase hex.
fn generate_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nonce::InMemoryNonceStore;

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

    #[test]
    fn issues_hold_challenge_and_registers_nonce() {
        let nonces = Arc::new(InMemoryNonceStore::new());
        let issuer = ChallengeIssuer::new(config(), nonces.clone());
        let ch = issuer.issue("POST /v1/echo");

        assert_eq!(ch.scheme, Scheme::Hold);
        assert_eq!(ch.payee, "ait1qpayee");
        assert_eq!(ch.amount_micros, AmountMicros(1_000));
        assert_eq!(ch.nonce.len(), 32);
        // Nonce is live and single-use.
        assert!(nonces.consume(&ch.nonce, now_ms()).is_ok());
    }
}
