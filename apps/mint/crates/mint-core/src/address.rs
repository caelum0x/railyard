//! bech32 `ait1<type><data><checksum>` receive addresses.
//!
//! The human-readable part is `ait`; bech32's mandatory `1` separator yields the
//! `ait1…` prefix. The payload is `[kind_nibble] ++ data`, so decode recovers
//! both the [`AccountKind`] and the opaque data bytes, and the bech32 checksum
//! rejects any typo/mutation. Addresses are **display/routing only** — never an
//! auth credential.

use bech32::{Bech32, Hrp};
use serde::{Deserialize, Serialize};

use crate::error::MintError;
use crate::ids::AccountKind;

/// The human-readable part of every Mint address.
const HRP_STR: &str = "ait";

/// A bech32 `ait1…` address. Cloneable/hashable so it keys the address index.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Address(String);

impl Address {
    /// Encode `(kind, data)` into a checksummed `ait1…` address.
    pub fn encode(kind: AccountKind, data: &[u8]) -> Address {
        let hrp = Hrp::parse(HRP_STR).expect("static hrp is valid");
        let mut payload = Vec::with_capacity(1 + data.len());
        payload.push(kind.to_nibble());
        payload.extend_from_slice(data);
        let s = bech32::encode::<Bech32>(hrp, &payload).expect("bech32 encode never fails here");
        Address(s)
    }

    /// Parse & validate an address string (HRP + checksum + kind nibble),
    /// returning the wrapped [`Address`]. See [`Address::decode`] for the parts.
    pub fn parse(s: &str) -> Result<Address, MintError> {
        let addr = Address(s.to_string());
        addr.decode()?; // validates
        Ok(addr)
    }

    /// Decode into `(kind, data)`, verifying the bech32 checksum and HRP.
    /// Returns [`MintError::BadAddress`] on any failure.
    pub fn decode(&self) -> Result<(AccountKind, Vec<u8>), MintError> {
        let (hrp, payload) = bech32::decode(&self.0).map_err(|_| MintError::BadAddress)?;
        if hrp.as_str() != HRP_STR {
            return Err(MintError::BadAddress);
        }
        let (nibble, rest) = payload.split_first().ok_or(MintError::BadAddress)?;
        let kind = AccountKind::from_nibble(*nibble).ok_or(MintError::BadAddress)?;
        Ok((kind, rest.to_vec()))
    }

    /// The account kind encoded in this address.
    pub fn kind(&self) -> Result<AccountKind, MintError> {
        Ok(self.decode()?.0)
    }

    /// The raw address string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Address({})", self.0)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_kind() {
        let data = [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04];
        for kind in [
            AccountKind::User,
            AccountKind::Org,
            AccountKind::Agent,
            AccountKind::Provider,
            AccountKind::Model,
            AccountKind::Treasury,
        ] {
            let addr = Address::encode(kind, &data);
            assert!(addr.as_str().starts_with("ait1"));
            let (k, d) = addr.decode().unwrap();
            assert_eq!(k, kind);
            assert_eq!(d, data);
            assert_eq!(Address::parse(addr.as_str()).unwrap(), addr);
        }
    }

    #[test]
    fn mutated_checksum_is_rejected() {
        let addr = Address::encode(AccountKind::User, &[1, 2, 3, 4]);
        let s = addr.as_str().to_string();
        // Flip the final checksum character to a different valid bech32 char.
        let last = s.chars().last().unwrap();
        let replacement = if last == 'q' { 'p' } else { 'q' };
        let mut mutated: String = s[..s.len() - 1].to_string();
        mutated.push(replacement);
        assert_eq!(Address::parse(&mutated), Err(MintError::BadAddress));
    }

    #[test]
    fn wrong_hrp_is_rejected() {
        // A valid bech32 string but with a different HRP.
        let hrp = Hrp::parse("btc").unwrap();
        let s = bech32::encode::<Bech32>(hrp, &[0u8, 1, 2, 3]).unwrap();
        assert_eq!(Address::parse(&s), Err(MintError::BadAddress));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(Address::parse("not-an-address"), Err(MintError::BadAddress));
        assert_eq!(Address::parse(""), Err(MintError::BadAddress));
    }
}
