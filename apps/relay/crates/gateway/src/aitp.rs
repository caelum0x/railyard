//! AITP-402 — the machine-payment handshake emitted at the billing edge.
//!
//! This is a PURE GATEWAY concern (see `docs/mint/AITP-402-SPEC.md` §5.1): when
//! billing is enabled and a caller's account cannot cover the model-aware hold,
//! the middleware replies `402 Payment Required` with an `AITP-Challenge` header
//! (base64url of the challenge JSON) plus `AITP-Version`, instead of a bare 402.
//! The Mint verifies the caller's later `AITP-Authorization` intent — the gateway
//! only mints and serializes the challenge, it performs NO signature checks here.
//!
//! For metered inference we always use the `aitp-hold` scheme: reserve up to
//! `amount_micros`, capture the actual usage after. That maps 1:1 onto the
//! existing `relay-billing` reserve -> settle/release success-only lifecycle.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use relay_billing::now_ms;

/// AITP protocol version advertised on the challenge.
pub const AITP_VERSION: &str = "0.1";

/// Response header carrying the protocol version. Lowercase because HTTP header
/// names are case-insensitive and `HeaderName::from_static` requires lowercase.
pub const AITP_VERSION_HEADER: &str = "aitp-version";

/// Response header carrying the base64url-encoded challenge.
pub const AITP_CHALLENGE_HEADER: &str = "aitp-challenge";

/// Currency of the AITP ledger.
pub const AITP_CURRENCY: &str = "AIT";

/// The metered-inference scheme: hold up to `amount_micros`, capture after.
pub const AITP_SCHEME_HOLD: &str = "aitp-hold";

/// How long a minted challenge stays valid (5 minutes), in milliseconds.
pub const AITP_CHALLENGE_TTL_MS: u64 = 5 * 60 * 1_000;

/// The AITP-402 challenge body (§5.1). Serializes to the deterministic JSON that
/// is base64url-encoded into the `AITP-Challenge` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// The provider's receive address (`ait1q…`).
    pub payee: String,
    /// The price for this request, in integer microdollars.
    pub amount_micros: i64,
    /// Currency of the amount (always [`AITP_CURRENCY`]).
    pub currency: String,
    /// The protected resource, e.g. `POST /v1/chat/completions#<model>`.
    pub resource: String,
    /// Single-use nonce the caller must echo in its authorization intent.
    pub nonce: String,
    /// Absolute expiry as a unix-millisecond timestamp.
    pub expiry: u64,
    /// Payment scheme; [`AITP_SCHEME_HOLD`] for metered inference.
    pub scheme: String,
}

impl Challenge {
    /// Build an `aitp-hold` challenge for a metered-inference request with an
    /// explicitly-supplied `nonce` and `expiry` (deterministic; used by tests).
    pub fn hold(
        payee: impl Into<String>,
        amount_micros: i64,
        resource: impl Into<String>,
        nonce: impl Into<String>,
        expiry_ms: u64,
    ) -> Self {
        Self {
            payee: payee.into(),
            amount_micros,
            currency: AITP_CURRENCY.to_string(),
            resource: resource.into(),
            nonce: nonce.into(),
            expiry: expiry_ms,
            scheme: AITP_SCHEME_HOLD.to_string(),
        }
    }

    /// Mint a fresh `aitp-hold` challenge with a random single-use nonce and an
    /// expiry [`AITP_CHALLENGE_TTL_MS`] in the future.
    pub fn mint_hold(
        payee: impl Into<String>,
        amount_micros: i64,
        resource: impl Into<String>,
    ) -> Self {
        let now = now_ms().max(0) as u64;
        let expiry_ms = now.saturating_add(AITP_CHALLENGE_TTL_MS);
        Self::hold(
            payee,
            amount_micros,
            resource,
            Uuid::new_v4().to_string(),
            expiry_ms,
        )
    }

    /// Base64url-encode (no padding) the challenge's canonical JSON for the
    /// `AITP-Challenge` header value.
    pub fn encode(&self) -> String {
        // Serializing a fixed struct of primitives cannot fail.
        let json = serde_json::to_vec(self).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(json)
    }

    /// Decode a base64url header value back into a [`Challenge`] (used by callers
    /// and tests to verify a well-formed challenge).
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn decode(encoded: &str) -> Option<Self> {
        let json = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()?;
        serde_json::from_slice(&json).ok()
    }
}

/// Build the canonical `resource` string for a challenge: `METHOD PATH#model`.
pub fn challenge_resource(method: &str, path: &str, model: &str) -> String {
    format!("{method} {path}#{model}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_is_method_path_and_model() {
        assert_eq!(
            challenge_resource("POST", "/v1/chat/completions", "openrouter/openai/gpt-4o"),
            "POST /v1/chat/completions#openrouter/openai/gpt-4o"
        );
    }

    #[test]
    fn challenge_round_trips_through_base64url() {
        let challenge = Challenge::hold(
            "ait1qprovider",
            4200,
            "POST /v1/chat/completions#llama-3.1-70b",
            "9f2c",
            1_723_958_400_000,
        );
        let encoded = challenge.encode();
        // base64url-no-pad: never contains '+', '/', or '=' padding.
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));

        let decoded = Challenge::decode(&encoded).expect("challenge should decode");
        assert_eq!(decoded, challenge);
    }

    #[test]
    fn hold_challenge_has_spec_fields() {
        let challenge = Challenge::hold(
            "ait1qprovider",
            4200,
            "POST /v1/chat/completions#m",
            "nonce123",
            1_723_958_400_000,
        );
        assert_eq!(challenge.payee, "ait1qprovider");
        assert_eq!(challenge.amount_micros, 4200);
        assert_eq!(challenge.currency, "AIT");
        assert_eq!(challenge.scheme, "aitp-hold");
        assert_eq!(challenge.nonce, "nonce123");
        assert_eq!(challenge.expiry, 1_723_958_400_000);
    }

    #[test]
    fn mint_hold_sets_future_expiry_and_random_nonce() {
        let before = now_ms().max(0) as u64;
        let a = Challenge::mint_hold("ait1qprovider", 100, "POST /x#m");
        let b = Challenge::mint_hold("ait1qprovider", 100, "POST /x#m");
        assert!(a.expiry >= before + AITP_CHALLENGE_TTL_MS);
        // Fresh, single-use nonces are unique per mint.
        assert_ne!(a.nonce, b.nonce);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Challenge::decode("!!!not base64!!!").is_none());
        // Valid base64url but not challenge JSON.
        let not_json = URL_SAFE_NO_PAD.encode(b"hello");
        assert!(Challenge::decode(&not_json).is_none());
    }
}
