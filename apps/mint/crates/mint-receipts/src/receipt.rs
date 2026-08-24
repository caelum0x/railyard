//! Receipt value types: the hashed/signed [`ReceiptBody`], the integrity
//! newtypes [`ReceiptHash`] / [`ReceiptSig`] / [`SignerKeyId`], the [`ReceiptOp`]
//! verb, and the assembled [`Receipt`] (AITP-402 §6).
//!
//! Hash rule (faithful to §6 "blake3(this ⊕ prev_hash)"):
//! `d = blake3(canonical_json_bytes(body))`, then
//! `hash = blake3(d XOR prev_hash)` with a 32-byte-wide XOR. Genesis
//! (`prev_hash == ZERO`) collapses to `hash = blake3(d)`.

use mint_core::{AccountId, Micros, OpHash};
use serde::{Deserialize, Serialize};

/// Render 32 raw bytes as lowercase hex.
fn to_hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse 64 lowercase-or-mixed hex chars into 32 bytes.
fn from_hex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    if s.len() != N * 2 {
        return Err(format!("expected {} hex chars, got {}", N * 2, s.len()));
    }
    let mut out = [0u8; N];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[2 * i] as char).to_digit(16).ok_or("bad hex")?;
        let lo = (bytes[2 * i + 1] as char).to_digit(16).ok_or("bad hex")?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

/// A 32-byte BLAKE3 receipt hash (or `prev_hash`). Serializes as lowercase hex
/// so a receipt's canonical body is a compact, deterministic JSON object.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct ReceiptHash([u8; 32]);

impl ReceiptHash {
    /// The genesis `prev_hash`: 32 zero bytes. A per-account chain's first
    /// receipt always carries `prev_hash == ZERO`.
    pub const ZERO: ReceiptHash = ReceiptHash([0u8; 32]);

    /// Wrap raw hash bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        ReceiptHash(bytes)
    }

    /// The raw 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex rendering.
    pub fn to_hex(&self) -> String {
        to_hex32(&self.0)
    }
}

impl std::fmt::Debug for ReceiptHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReceiptHash({})", self.to_hex())
    }
}

impl Serialize for ReceiptHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ReceiptHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = from_hex::<32>(&s).map_err(serde::de::Error::custom)?;
        Ok(ReceiptHash(bytes))
    }
}

/// A 64-byte Ed25519 signature over a receipt hash. Serializes as lowercase hex.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct ReceiptSig([u8; 64]);

impl ReceiptSig {
    /// Wrap raw signature bytes.
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        ReceiptSig(bytes)
    }

    /// The raw 64 bytes.
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }

    /// Lowercase hex rendering.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(128);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

impl std::fmt::Debug for ReceiptSig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReceiptSig({})", self.to_hex())
    }
}

impl Serialize for ReceiptSig {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ReceiptSig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = from_hex::<64>(&s).map_err(serde::de::Error::custom)?;
        Ok(ReceiptSig(bytes))
    }
}

/// The id of the key that signed a receipt (e.g. `"mint-verifier-key-1"`). Held
/// inside the hashed body so key rotation is chain-visible.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignerKeyId(pub String);

impl SignerKeyId {
    /// Build a key id from anything string-like.
    pub fn new(s: impl Into<String>) -> Self {
        SignerKeyId(s.into())
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The receipt verb, mirroring the store's `OpKind`. Defined here (not imported
/// from `mint-store`) so this crate never depends on the store; the store maps
/// `OpKind → ReceiptOp` at the call site.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReceiptOp {
    /// AP0 opening balance (genesis receipt on an account chain).
    Seed,
    /// Double-entry transfer leg.
    Transfer,
    /// Reservation opened (available → held).
    HoldCreate,
    /// Hold settled to a payee (remainder released).
    HoldCapture,
    /// Hold freed with no settlement.
    HoldRelease,
    /// Metered stream opened (cap reserved available → held on the payer).
    StreamOpen,
    /// Stream tick settled (`units*rate` moved held payer → available payee).
    StreamTick,
    /// Stream closed (final settle + release of the unused remainder).
    StreamClose,
    /// AIT created against a confirmed reserve deposit (reserve rises in lockstep).
    Mint,
    /// AIT burned against a payout (reserve falls in lockstep).
    Redeem,
}

/// The hashed, signed content of a receipt — everything EXCEPT `hash` and `sig`.
///
/// Field order here is irrelevant to the hash: [`ReceiptBody::compute_hash`]
/// canonicalizes to sorted-key JSON before hashing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptBody {
    /// Whose chain this row is on (primary-key part 1).
    pub account_id: AccountId,
    /// 0-based position in THIS account's chain (primary-key part 2). Distinct
    /// from the global journal `seq`.
    pub account_seq: u64,
    /// FK to the emitting journal row's `seq` (the §6 `seq`).
    pub journal_seq: u64,
    /// The operation verb.
    pub op: ReceiptOp,
    /// The shared `op_hash` of the emitting op (same across all its rows).
    pub op_hash: OpHash,
    /// The settled/moved amount for this leg.
    pub amount_micros: Micros,
    /// The debited counterparty (`None` for seed).
    pub from: Option<AccountId>,
    /// The credited counterparty (`None` for release).
    pub to: Option<AccountId>,
    /// This account's post-op available balance (§6 `balance_after_micros`).
    pub balance_after_available_micros: Micros,
    /// This account's post-op held balance (lets reserve proof sum avail+held).
    pub balance_after_held_micros: Micros,
    /// The SAME `clock.now()` stamped on the op's journal rows (unix millis).
    pub ts: i64,
    /// The previous receipt's `hash` on this account (`ZERO` at genesis).
    pub prev_hash: ReceiptHash,
    /// The signing key id (hashed, so rotation is chain-visible).
    pub signer: SignerKeyId,
}

impl ReceiptBody {
    /// Compute this body's [`ReceiptHash`] per §6:
    /// `hash = blake3( blake3(canonical_json(body)) XOR prev_hash )`.
    pub fn compute_hash(&self) -> ReceiptHash {
        let body_val = serde_json::to_value(self).expect("ReceiptBody serializes to JSON");
        let canonical = mint_core::hash::canonical_json_bytes(&body_val);
        let digest = blake3::hash(&canonical);
        let d = digest.as_bytes();
        let prev = self.prev_hash.as_bytes();
        let mut xored = [0u8; 32];
        for i in 0..32 {
            xored[i] = d[i] ^ prev[i];
        }
        ReceiptHash(*blake3::hash(&xored).as_bytes())
    }
}

/// A complete receipt: the hashed body plus its chain `hash` and Ed25519 `sig`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// The signed content.
    pub body: ReceiptBody,
    /// `blake3(d XOR prev_hash)` over the body.
    pub hash: ReceiptHash,
    /// Ed25519 signature over `hash.as_bytes()`.
    pub sig: ReceiptSig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_hex_round_trips_through_serde() {
        let h = ReceiptHash::from_bytes([7u8; 32]);
        let j = serde_json::to_string(&h).unwrap();
        assert_eq!(j, format!("\"{}\"", "07".repeat(32)));
        let back: ReceiptHash = serde_json::from_str(&j).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn zero_is_all_zeroes() {
        assert_eq!(ReceiptHash::ZERO.as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn genesis_hash_is_blake3_of_digest_when_prev_is_zero() {
        let body = sample_body(ReceiptHash::ZERO);
        let canonical =
            mint_core::hash::canonical_json_bytes(&serde_json::to_value(&body).unwrap());
        let d = blake3::hash(&canonical);
        // prev == ZERO ⇒ XOR is identity ⇒ hash == blake3(d).
        let expected = ReceiptHash::from_bytes(*blake3::hash(d.as_bytes()).as_bytes());
        assert_eq!(body.compute_hash(), expected);
    }

    #[test]
    fn hash_changes_with_prev_hash() {
        let a = sample_body(ReceiptHash::ZERO).compute_hash();
        let b = sample_body(ReceiptHash::from_bytes([1u8; 32])).compute_hash();
        assert_ne!(a, b);
    }

    fn sample_body(prev: ReceiptHash) -> ReceiptBody {
        use mint_core::AccountKind;
        use ulid::Ulid;
        let acct = AccountId::new(
            AccountKind::User,
            Ulid::from_string("00000000000000000000000000").unwrap(),
        );
        ReceiptBody {
            account_id: acct.clone(),
            account_seq: 0,
            journal_seq: 0,
            op: ReceiptOp::Seed,
            op_hash: mint_core::op_hash(&serde_json::json!({"op": "seed"})),
            amount_micros: Micros::new(1_000_000),
            from: None,
            to: Some(acct),
            balance_after_available_micros: Micros::new(1_000_000),
            balance_after_held_micros: Micros::zero(),
            ts: 1_700_000_000_000,
            prev_hash: prev,
            signer: SignerKeyId::new("mint-verifier-key-1"),
        }
    }
}
