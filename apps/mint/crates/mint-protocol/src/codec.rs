//! TRANSPORT codec for the AITP-402 headers (SPEC §5) — the `base64url` +
//! compact-JSON envelope, mirroring `aitp-client/src/codec.rs` byte-for-byte.
//!
//! This is one of TWO independent encodings and must never be conflated with the
//! other (see [`crate::intent`]):
//! - **TRANSPORT (here):** `base64url(URL_SAFE_NO_PAD, serde_json::to_vec)` —
//!   compact JSON in struct *declaration order*, NOT sorted. Header framing only.
//! - **SIGNING ([`crate::intent`]):** sorted-key canonical JSON → blake3 →
//!   ed25519. Field-order independent. Never base64.
//!
//! Because `mint-protocol`'s wire structs match the client's field order and
//! `Scheme` renames, encoding a [`Challenge`]/[`Authorization`] here yields the
//! exact same bytes the agent produces, so both sides interoperate.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use mint_receipts::{Receipt, ReceiptOp};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::types::{Authorization, Challenge};

/// AITP protocol version this server speaks (`AITP-Version` header). Matches the
/// agent client's `AITP_VERSION` so version negotiation succeeds.
pub const AITP_VERSION: &str = "0.1";

/// Header names from SPEC §5 (identical to the client's).
pub const HEADER_VERSION: &str = "AITP-Version";
/// The `402` challenge header name.
pub const HEADER_CHALLENGE: &str = "AITP-Challenge";
/// The signed-authorization header name.
pub const HEADER_AUTHORIZATION: &str = "AITP-Authorization";
/// The settlement-receipt header name.
pub const HEADER_RECEIPT: &str = "AITP-Receipt";

/// A transport-layer (framing) error — distinct from the settlement
/// [`mint_core::MintError`]. Carries the underlying base64/JSON detail.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// The header was not valid URL-safe base64.
    #[error("aitp_codec_base64: {0}")]
    Base64(String),
    /// The decoded bytes were not the expected JSON shape.
    #[error("aitp_codec_json: {0}")]
    Json(String),
}

fn encode<T: Serialize>(value: &T) -> Result<String, CodecError> {
    // Compact JSON in declaration order (serde default) — NOT sorted.
    let json = serde_json::to_vec(value).map_err(|e| CodecError::Json(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode<T: DeserializeOwned>(header: &str) -> Result<T, CodecError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(header.trim())
        .map_err(|e| CodecError::Base64(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| CodecError::Json(e.to_string()))
}

/// Encode an `AITP-Challenge` header (server → caller).
pub fn encode_challenge(challenge: &Challenge) -> Result<String, CodecError> {
    encode(challenge)
}

/// Decode an incoming `AITP-Challenge` header.
pub fn decode_challenge(header: &str) -> Result<Challenge, CodecError> {
    decode(header)
}

/// Encode an `AITP-Authorization` header (caller → server; used by tests/fakes).
pub fn encode_authorization(auth: &Authorization) -> Result<String, CodecError> {
    encode(auth)
}

/// Decode an incoming `AITP-Authorization` header (server side).
pub fn decode_authorization(header: &str) -> Result<Authorization, CodecError> {
    decode(header)
}

/// The FLAT `AITP-Receipt` wire DTO. It mirrors the agent-runtime
/// `aitp-client`'s `Receipt` struct EXACTLY (field names, order, and JSON
/// shapes), so a Mint-issued header decodes on the client without drift.
///
/// The in-memory §6 [`Receipt`] is NESTED (`{ body: { … }, hash, sig }`) — the
/// client is flat and renames two fields (`journal_seq → seq`,
/// `balance_after_available_micros → balance_after_micros`). serde requires the
/// client's KNOWN top-level fields, so encoding the nested receipt directly
/// makes the client's `decode_receipt` fail with `missing field 'seq'`. We
/// therefore project the nested receipt onto this flat DTO before encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AitpReceipt {
    /// The emitting journal row's global `seq` (§6 `journal_seq`).
    pub seq: u64,
    /// The operation verb as a stable dotted string (e.g. `"hold.capture"`).
    pub op: String,
    /// The shared op_hash of the emitting op, lowercase hex.
    pub op_hash: String,
    /// The moved amount for this leg, integer micros.
    pub amount_micros: i64,
    /// This account's post-op AVAILABLE balance (§6
    /// `balance_after_available_micros`).
    pub balance_after_micros: i64,
    /// The previous receipt's hash on this account chain, lowercase hex.
    pub prev_hash: String,
    /// This receipt's chain hash, lowercase hex.
    pub hash: String,
    /// Ed25519 signature over `hash`, lowercase hex.
    pub sig: String,
}

impl AitpReceipt {
    /// Project the full §6 [`Receipt`] onto the flat client wire shape.
    pub fn from_receipt(r: &Receipt) -> Self {
        AitpReceipt {
            seq: r.body.journal_seq,
            op: op_wire_str(r.body.op).to_string(),
            op_hash: r.body.op_hash.to_hex(),
            amount_micros: r.body.amount_micros.get(),
            balance_after_micros: r.body.balance_after_available_micros.get(),
            prev_hash: r.body.prev_hash.to_hex(),
            hash: r.hash.to_hex(),
            sig: r.sig.to_hex(),
        }
    }
}

/// Stable dotted wire strings for the receipt verb, matching the client's
/// convention (see `aitp-client` stub, e.g. `"hold.capture"`).
fn op_wire_str(op: ReceiptOp) -> &'static str {
    match op {
        ReceiptOp::Seed => "seed",
        ReceiptOp::Transfer => "transfer",
        ReceiptOp::HoldCreate => "hold.create",
        ReceiptOp::HoldCapture => "hold.capture",
        ReceiptOp::HoldRelease => "hold.release",
        ReceiptOp::StreamOpen => "stream.open",
        ReceiptOp::StreamTick => "stream.tick",
        ReceiptOp::StreamClose => "stream.close",
        ReceiptOp::Mint => "mint",
        ReceiptOp::Redeem => "redeem",
    }
}

/// Encode the `AITP-Receipt` response header from the full §6 [`Receipt`],
/// projecting it onto the flat [`AitpReceipt`] the client actually decodes.
pub fn encode_receipt(receipt: &Receipt) -> Result<String, CodecError> {
    encode(&AitpReceipt::from_receipt(receipt))
}

/// Decode an `AITP-Receipt` header back into the flat [`AitpReceipt`] wire DTO.
pub fn decode_receipt(header: &str) -> Result<AitpReceipt, CodecError> {
    decode(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mint_core::{op_hash, AccountId, AccountKind, Micros};
    use mint_receipts::{ReceiptBody, ReceiptHash, ReceiptSig, SignerKeyId};
    use ulid::Ulid;

    /// A byte-for-byte mirror of the agent-runtime `aitp-client`'s FLAT
    /// `Receipt` (see `aitp-client/src/types.rs`). `amount_micros` /
    /// `balance_after_micros` there are `AmountMicros` (a `#[serde(transparent)]`
    /// `i64`), so a bare `i64` decodes the identical JSON. If the Mint DTO ever
    /// drifts from this shape, the round-trip test below fails to compile or
    /// decode.
    #[derive(Debug, PartialEq, Eq, serde::Deserialize)]
    struct ClientReceiptMirror {
        seq: u64,
        op: String,
        op_hash: String,
        amount_micros: i64,
        balance_after_micros: i64,
        prev_hash: String,
        hash: String,
        sig: String,
    }

    fn sample_receipt() -> Receipt {
        let acct = AccountId::new(
            AccountKind::User,
            Ulid::from_string("00000000000000000000000000").unwrap(),
        );
        let body = ReceiptBody {
            account_id: acct.clone(),
            account_seq: 3,
            journal_seq: 7,
            op: ReceiptOp::HoldCapture,
            op_hash: op_hash(&serde_json::json!({"op": "hold.capture"})),
            amount_micros: Micros::new(2_500_000),
            from: Some(acct.clone()),
            to: Some(acct),
            balance_after_available_micros: Micros::new(1_200_000),
            balance_after_held_micros: Micros::zero(),
            ts: 1_700_000_000_000,
            prev_hash: ReceiptHash::from_bytes([9u8; 32]),
            signer: SignerKeyId::new("mint-verifier-key-1"),
        };
        let hash = body.compute_hash();
        Receipt {
            body,
            hash,
            sig: ReceiptSig::from_bytes([5u8; 64]),
        }
    }

    #[test]
    fn encode_receipt_decodes_into_client_flat_shape() {
        // Mint encodes a full §6 receipt; the AITP-Receipt header must decode
        // into the client's FLAT struct with the renamed fields present at the
        // top level. This locks the cross-crate wire interop.
        let receipt = sample_receipt();
        let header = encode_receipt(&receipt).unwrap();

        let bytes = URL_SAFE_NO_PAD.decode(header.trim()).unwrap();
        let mirror =
            ClientReceiptMirror::deserialize(&mut serde_json::Deserializer::from_slice(&bytes))
                .expect("Mint AITP-Receipt must decode into the client's flat Receipt");

        assert_eq!(mirror.seq, 7); // journal_seq → seq
        assert_eq!(mirror.op, "hold.capture");
        assert_eq!(mirror.op_hash, receipt.body.op_hash.to_hex());
        assert_eq!(mirror.amount_micros, 2_500_000);
        assert_eq!(mirror.balance_after_micros, 1_200_000); // available balance
        assert_eq!(mirror.prev_hash, receipt.body.prev_hash.to_hex());
        assert_eq!(mirror.hash, receipt.hash.to_hex());
        assert_eq!(mirror.sig, receipt.sig.to_hex());
    }

    #[test]
    fn aitp_receipt_round_trips_through_the_header() {
        let receipt = sample_receipt();
        let header = encode_receipt(&receipt).unwrap();
        let decoded = decode_receipt(&header).unwrap();
        assert_eq!(decoded, AitpReceipt::from_receipt(&receipt));
    }
}
