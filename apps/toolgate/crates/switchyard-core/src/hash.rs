//! Canonical JSON, request hashing and the audit hash chain.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Serialise a JSON value with object keys sorted at every depth, so two
/// semantically equal inputs always hash the same regardless of key order.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Hex SHA-256 of arbitrary bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Normalise a call input for hashing: strings are trimmed, `null` members are
/// dropped (absent and null mean the same to every adapter), keys are sorted.
pub fn normalise_input(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), normalise_input(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalise_input).collect()),
        Value::String(s) => Value::String(s.trim().to_string()),
        other => other.clone(),
    }
}

/// Request hash used by idempotency, cache and single-flight: the target id plus
/// the normalised canonical input.
pub fn request_hash(target: &str, input: &Value) -> String {
    let body = format!("{target}\n{}", canonical_json(&normalise_input(input)));
    sha256_hex(body.as_bytes())
}

/// The genesis link for every project's audit chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Next link of the audit chain: `sha256(prev_hash || "\n" || canonical(row))`.
pub fn chain_hash(prev_hash: &str, row: &Value) -> String {
    sha256_hex(format!("{prev_hash}\n{}", canonical_json(row)).as_bytes())
}

/// Verify an ordered slice of `(prev_hash, row_hash, row)` links.
pub fn verify_chain<'a>(links: impl IntoIterator<Item = (&'a str, &'a str, &'a Value)>) -> Result<(), usize> {
    let mut expected_prev = GENESIS_HASH.to_string();
    for (i, (prev, hash, row)) in links.into_iter().enumerate() {
        if prev != expected_prev || chain_hash(prev, row) != hash {
            return Err(i);
        }
        expected_prev = hash.to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_is_key_order_independent() {
        let a = json!({"b": 1, "a": {"y": [1, 2], "x": "s"}});
        let b = json!({"a": {"x": "s", "y": [1, 2]}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"x":"s","y":[1,2]},"b":1}"#);
    }

    #[test]
    fn request_hash_normalises_whitespace_and_nulls() {
        let a = request_hash("web.search", &json!({"q": " rust ", "limit": null}));
        let b = request_hash("web.search", &json!({"q": "rust"}));
        assert_eq!(a, b);
        assert_ne!(a, request_hash("web.read", &json!({"q": "rust"})));
        assert_ne!(a, request_hash("web.search", &json!({"q": "go"})));
    }

    #[test]
    fn chain_detects_tampering() {
        let r1 = json!({"id": "call_1", "cost": 10});
        let r2 = json!({"id": "call_2", "cost": 0});
        let h1 = chain_hash(GENESIS_HASH, &r1);
        let h2 = chain_hash(&h1, &r2);
        assert!(verify_chain(vec![(GENESIS_HASH, h1.as_str(), &r1), (h1.as_str(), h2.as_str(), &r2)]).is_ok());
        let forged = json!({"id": "call_1", "cost": 0});
        assert_eq!(
            verify_chain(vec![(GENESIS_HASH, h1.as_str(), &forged), (h1.as_str(), h2.as_str(), &r2)]),
            Err(0)
        );
        assert_eq!(verify_chain(vec![(GENESIS_HASH, h1.as_str(), &r1), (GENESIS_HASH, h2.as_str(), &r2)]), Err(1));
    }
}
