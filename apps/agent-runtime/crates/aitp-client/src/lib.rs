//! AITP-402 payment seam for the agent runtime.
//!
//! This crate models ONLY the caller side of the AITP-402 handshake
//! (`docs/mint/AITP-402-SPEC.md` §5) as a Rust trait, [`PaymentGateway`], plus
//! the wire types and a header codec. It has **no dependency on rig or Mint** —
//! that decoupling is the whole point. A [`StubPaymentGateway`] is the default
//! (and only) implementor today; a Mint-backed one drops in later behind the
//! same trait.

pub mod amount;
pub mod codec;
pub mod error;
pub mod gateway;
pub mod mint;
pub mod signer;
pub mod stub;
pub mod transport;
pub mod types;

pub use amount::{AmountMicros, MICROS_PER_AIT};
pub use error::PayError;
pub use gateway::{Authorized, PaymentGateway};
pub use mint::MintPaymentGateway;
pub use signer::{op_hash, op_hash_hex, AuthSigner, AuthorizationIntent, Ed25519Signer};
pub use stub::{StubCalls, StubPaymentGateway};
pub use transport::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub use types::{HoldRef, PaymentAuthorization, PaymentChallenge, Receipt, Scheme};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_challenge() -> PaymentChallenge {
        PaymentChallenge {
            payee: "ait1qprovider".into(),
            amount_micros: AmountMicros(4200),
            currency: "AIT".into(),
            resource: "POST /v1/chat/completions#llama-3.1-70b".into(),
            nonce: "9f2c".into(),
            expiry: 1_723_958_400_000,
            scheme: Scheme::Hold,
        }
    }

    #[test]
    fn challenge_roundtrips_through_base64url_header() {
        let challenge = sample_challenge();
        let header = codec::encode_challenge(&challenge).unwrap();
        let decoded = codec::decode_challenge(&header).unwrap();
        assert_eq!(challenge, decoded);
    }

    #[tokio::test]
    async fn stub_authorizes_and_captures() {
        let gw = StubPaymentGateway::default();
        let challenge = sample_challenge();

        let authorized = gw.authorize(&challenge).await.unwrap();
        assert_eq!(authorized.authorization.amount_micros, AmountMicros(4200));
        assert_eq!(authorized.authorization.nonce, "9f2c");
        let hold = authorized.hold.expect("hold scheme yields a HoldRef");

        let receipt = gw.capture(&hold, AmountMicros(3900)).await.unwrap();
        assert_eq!(receipt.amount_micros, AmountMicros(3900));

        gw.with_calls(|c| {
            assert_eq!(c.authorized.len(), 1);
            assert_eq!(c.captured.len(), 1);
        });
    }

    #[tokio::test]
    async fn stub_can_reject_authorization() {
        let gw = StubPaymentGateway::always_failing(|| PayError::InsufficientFunds);
        let err = gw.authorize(&sample_challenge()).await.unwrap_err();
        assert!(matches!(err, PayError::InsufficientFunds));
    }
}
