//! Canonical-JSON → blake3 `op_hash` helper (AITP-402 §3).
//!
//! Deterministic serialization: object keys sorted recursively, no insignificant
//! whitespace, integer amounts. The resulting bytes are hashed with blake3 to a
//! 32-byte [`OpHash`]. Computed for forward-compat (receipt chaining / signing
//! land in AP2/AP3); no signing is wired in AP0.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A 32-byte blake3 operation hash. Reproducible for a given canonical body,
/// independent of input field order.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpHash([u8; 32]);

impl OpHash {
    /// Wrap raw hash bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        OpHash(bytes)
    }

    /// The raw 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex rendering.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

impl std::fmt::Debug for OpHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpHash({})", self.to_hex())
    }
}

/// Recursively rebuild a JSON value with object keys sorted, guaranteeing a
/// deterministic byte encoding regardless of the `serde_json` `preserve_order`
/// feature.
fn canonicalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// Canonical (sorted-key, compact) JSON bytes for `v`.
pub fn canonical_json_bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(&canonicalize(v)).expect("in-memory Value serializes")
}

/// Compute the [`OpHash`] of a canonical operation body.
pub fn op_hash(v: &Value) -> OpHash {
    OpHash(*blake3::hash(&canonical_json_bytes(v)).as_bytes())
}

/// Derive 16 stable bytes from ordered string parts via blake3 (used for AP0
/// address seeds — no KMS). Deterministic for the same parts.
pub fn seed_bytes16(parts: &[&str]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    for p in parts {
        hasher.update(p.as_bytes());
        hasher.update(&[0u8]); // length-agnostic separator
    }
    let full = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn op_hash_is_field_order_independent() {
        let a = json!({ "op": "transfer", "from": "a", "to": "b", "amount_micros": 4200 });
        let b = json!({ "amount_micros": 4200, "to": "b", "from": "a", "op": "transfer" });
        assert_eq!(op_hash(&a), op_hash(&b));
        assert_eq!(op_hash(&a).as_bytes().len(), 32);
    }

    #[test]
    fn op_hash_changes_with_content() {
        let a = json!({ "amount_micros": 4200 });
        let b = json!({ "amount_micros": 4201 });
        assert_ne!(op_hash(&a), op_hash(&b));
    }

    #[test]
    fn canonical_json_is_sorted_and_compact() {
        let v = json!({ "b": 1, "a": { "d": 2, "c": 3 } });
        let bytes = canonical_json_bytes(&v);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"a":{"c":3,"d":2},"b":1}"#
        );
    }

    #[test]
    fn seed_bytes_are_deterministic_and_purpose_sensitive() {
        assert_eq!(
            seed_bytes16(&["acct_1", "payout"]),
            seed_bytes16(&["acct_1", "payout"])
        );
        assert_ne!(
            seed_bytes16(&["acct_1", "payout"]),
            seed_bytes16(&["acct_1", "tips"])
        );
    }

    #[test]
    fn hex_round_trip_len() {
        let h = op_hash(&json!({"x": 1}));
        assert_eq!(h.to_hex().len(), 64);
    }
}
