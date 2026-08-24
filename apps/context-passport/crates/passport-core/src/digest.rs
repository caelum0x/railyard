//! Content-addressing for locked versions.
//!
//! Locking canonicalizes the version body and hashes it; that digest *is* the
//! version's identity (Git-like content addressing → dedup + tamper-evidence,
//! and a 1:1 map onto C2PA hard-binding hashes and Mint's receipt chain).
//!
//! The canonical form here is a pragmatic JSON Canonicalization (recursively
//! sorted object keys, compact separators). RFC 8785 (JCS) — including float and
//! Unicode-NFC normalization — plus an Ed25519 signature envelope is the Sx1
//! hardening target; the content hash is real and stable now.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A content address over a canonicalized value, formatted as an `ni` URI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionDigest(String);

impl VersionDigest {
    /// Compute the digest of any serializable body.
    pub fn of<T: Serialize>(body: &T) -> Self {
        let value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
        let canonical = canonicalize(&value);
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        let hex = hex::encode(hasher.finalize());
        Self(format!("ni:///sha-256;{hex}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Serialize with recursively sorted object keys and no insignificant whitespace.
fn canonicalize(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // `to_string` on a Value::String yields a correctly escaped JSON string.
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_is_stable_and_key_order_independent() {
        let a = json!({"b": 1, "a": [1, 2], "c": {"y": 2, "x": 1}});
        let b = json!({"c": {"x": 1, "y": 2}, "a": [1, 2], "b": 1});
        assert_eq!(VersionDigest::of(&a), VersionDigest::of(&b));
        assert!(VersionDigest::of(&a).as_str().starts_with("ni:///sha-256;"));
    }

    #[test]
    fn different_content_differs() {
        assert_ne!(
            VersionDigest::of(&json!({"a": 1})),
            VersionDigest::of(&json!({"a": 2}))
        );
    }
}
