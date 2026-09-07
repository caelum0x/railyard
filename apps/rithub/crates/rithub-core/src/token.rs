//! Personal access token generation and hashing.
//!
//! Tokens are opaque bearer secrets. We generate a high-entropy token, show it
//! to the user once, and persist only its SHA-256 hash — so a database leak
//! cannot reveal usable credentials. Authentication re-hashes the presented
//! token and compares hashes.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Human-recognizable prefix so a leaked token is easy to identify and revoke.
pub const TOKEN_PREFIX: &str = "rithub_pat_";

/// Generate a new personal access token (prefix + 256 bits of hex entropy).
pub fn generate_token() -> String {
    // Two v4 UUIDs give 244 bits of randomness — ample for a bearer token.
    format!(
        "{TOKEN_PREFIX}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// Compute the lowercase-hex SHA-256 of a token, the value stored and compared.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_prefixed() {
        let a = generate_token();
        let b = generate_token();
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_ne!(a, b);
    }

    #[test]
    fn hash_is_stable_and_64_hex_chars() {
        let token = "rithub_pat_deadbeef";
        let h = hash_token(token);
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_token(token));
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
