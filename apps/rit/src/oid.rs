//! Git object identifiers.
//!
//! An [`ObjectId`] is a 20-byte SHA-1 digest — the same identity Git assigns to
//! every object. The hex form is the familiar 40-character string printed by
//! `git rev-parse`.

use crate::error::{ForgeError, Result};
use std::fmt;

/// Number of raw bytes in a SHA-1 object id.
pub const OID_LEN: usize = 20;
/// Number of hex characters in a SHA-1 object id.
pub const OID_HEX_LEN: usize = OID_LEN * 2;

/// A 20-byte SHA-1 Git object identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId([u8; OID_LEN]);

impl ObjectId {
    /// Wrap 20 raw digest bytes.
    pub const fn from_bytes(bytes: [u8; OID_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw digest bytes (used when serializing tree entries).
    pub const fn as_bytes(&self) -> &[u8; OID_LEN] {
        &self.0
    }

    /// Parse a full 40-character lowercase/uppercase hex string.
    pub fn from_hex(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.len() != OID_HEX_LEN {
            return Err(ForgeError::InvalidOid(format!(
                "expected {OID_HEX_LEN} hex chars, got {}",
                s.len()
            )));
        }
        let mut bytes = [0u8; OID_LEN];
        let raw = s.as_bytes();
        for (i, byte) in bytes.iter_mut().enumerate() {
            let hi = hex_val(raw[i * 2])
                .ok_or_else(|| ForgeError::InvalidOid(format!("non-hex character in {s:?}")))?;
            let lo = hex_val(raw[i * 2 + 1])
                .ok_or_else(|| ForgeError::InvalidOid(format!("non-hex character in {s:?}")))?;
            *byte = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }

    /// Render the lowercase 40-character hex form.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(OID_HEX_LEN);
        for &b in &self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    /// The two-character sharded subdirectory (`objects/xx/...`).
    pub fn dir_prefix(&self) -> String {
        self.to_hex()[..2].to_string()
    }

    /// The remaining 38 hex characters (`objects/xx/<rest>`).
    pub fn file_suffix(&self) -> String {
        self.to_hex()[2..].to_string()
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

/// Map an ASCII hex digit to its numeric value, or `None` if not hex.
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrips() {
        let hex = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let oid = ObjectId::from_hex(hex).unwrap();
        assert_eq!(oid.to_hex(), hex);
        assert_eq!(oid.dir_prefix(), "e6");
        assert_eq!(oid.file_suffix(), "9de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn rejects_bad_length() {
        assert!(ObjectId::from_hex("abc").is_err());
    }

    #[test]
    fn rejects_non_hex() {
        let bad = "z69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        assert!(ObjectId::from_hex(bad).is_err());
    }
}
