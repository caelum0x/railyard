//! Account identity types: [`AccountKind`] and [`AccountId`].
//!
//! An `AccountId` is a self-describing opaque string: `acct_<kind><ULID>` where
//! `<kind>` is a single hex nibble (`0`–`5`) and `<ULID>` is a 26-char Crockford
//! base32 ULID. The embedded kind nibble lets `kind()` be pure and lets the id
//! stay a single validated `String` newtype (no side table needed in AP0).

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::MintError;

/// Owner category of an account. Nibble values are stable and shared with the
/// [`crate::address::Address`] type nibble and the SQL `kind` column.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum AccountKind {
    /// End user. `0`.
    User,
    /// Organization. `1`.
    Org,
    /// Autonomous agent. `2`.
    Agent,
    /// Upstream provider. `3`.
    Provider,
    /// Model author / payout target. `4`.
    Model,
    /// Platform treasury. `5`.
    Treasury,
}

impl AccountKind {
    /// The stable nibble encoding (`0`–`5`).
    pub const fn to_nibble(self) -> u8 {
        match self {
            AccountKind::User => 0,
            AccountKind::Org => 1,
            AccountKind::Agent => 2,
            AccountKind::Provider => 3,
            AccountKind::Model => 4,
            AccountKind::Treasury => 5,
        }
    }

    /// Inverse of [`AccountKind::to_nibble`]. `None` for out-of-range nibbles.
    pub const fn from_nibble(n: u8) -> Option<AccountKind> {
        match n {
            0 => Some(AccountKind::User),
            1 => Some(AccountKind::Org),
            2 => Some(AccountKind::Agent),
            3 => Some(AccountKind::Provider),
            4 => Some(AccountKind::Model),
            5 => Some(AccountKind::Treasury),
            _ => None,
        }
    }
}

const PREFIX: &str = "acct_";

/// An opaque, validated account identifier. Immutable; ordered/hashable so it
/// keys balance maps and drives fixed lock-ordering.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    /// Build an id from a kind + ULID. Always valid by construction.
    pub fn new(kind: AccountKind, ulid: Ulid) -> Self {
        AccountId(format!("{}{:x}{}", PREFIX, kind.to_nibble(), ulid))
    }

    /// Parse and validate an id string: `acct_` prefix, a valid kind nibble,
    /// then a valid 26-char ULID. Returns [`MintError::BadAccountId`] on any
    /// deviation.
    pub fn parse(s: &str) -> Result<AccountId, MintError> {
        let rest = s.strip_prefix(PREFIX).ok_or(MintError::BadAccountId)?;
        let mut chars = rest.chars();
        let kind_ch = chars.next().ok_or(MintError::BadAccountId)?;
        let nibble = kind_ch.to_digit(16).ok_or(MintError::BadAccountId)? as u8;
        AccountKind::from_nibble(nibble).ok_or(MintError::BadAccountId)?;
        let ulid_str: String = chars.collect();
        Ulid::from_string(&ulid_str).map_err(|_| MintError::BadAccountId)?;
        Ok(AccountId(s.to_string()))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The embedded [`AccountKind`]. Infallible — an `AccountId` is only ever
    /// constructed through validated paths.
    pub fn kind(&self) -> AccountKind {
        let rest = &self.0[PREFIX.len()..];
        let nibble = rest
            .chars()
            .next()
            .and_then(|c| c.to_digit(16))
            .expect("validated AccountId has a kind nibble") as u8;
        AccountKind::from_nibble(nibble).expect("validated AccountId kind nibble in range")
    }
}

impl std::fmt::Debug for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AccountId({})", self.0)
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const HOLD_PREFIX: &str = "hold_";

/// An opaque, validated hold identifier: `hold_<ULID>`. Minted by the store
/// inside `commit_hold_create`'s critical section (like `seq` and `AccountId`).
/// Ordered/hashable so it keys the live holds map. Serializes as a bare string.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HoldId(String);

impl HoldId {
    /// Build an id from a ULID. Always valid by construction.
    pub fn new(ulid: Ulid) -> Self {
        HoldId(format!("{HOLD_PREFIX}{ulid}"))
    }

    /// Parse and validate an id string: `hold_` prefix then a valid 26-char
    /// ULID. Reuses [`MintError::BadAccountId`] to avoid renumbering the stable
    /// error codes (a malformed typed id is the same failure class).
    pub fn parse(s: &str) -> Result<HoldId, MintError> {
        let rest = s.strip_prefix(HOLD_PREFIX).ok_or(MintError::BadAccountId)?;
        Ulid::from_string(rest).map_err(|_| MintError::BadAccountId)?;
        Ok(HoldId(s.to_string()))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for HoldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HoldId({})", self.0)
    }
}

impl std::fmt::Display for HoldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const ALLOWANCE_PREFIX: &str = "alw_";

/// An opaque, validated allowance identifier: `alw_<ULID>`. Minted by the store
/// inside `commit_allowance_grant`'s critical section (like `seq`, `AccountId`,
/// and `HoldId`), so an idempotent grant replay returns the SAME original id.
/// Ordered/hashable so it keys the live allowances map. Serializes as a bare
/// string.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AllowanceId(String);

impl AllowanceId {
    /// Build an id from a ULID. Always valid by construction.
    pub fn new(ulid: Ulid) -> Self {
        AllowanceId(format!("{ALLOWANCE_PREFIX}{ulid}"))
    }

    /// Parse and validate an id string: `alw_` prefix then a valid 26-char
    /// ULID. Reuses [`MintError::BadAccountId`] (a malformed typed id is the
    /// same failure class) to avoid renumbering the stable error codes.
    pub fn parse(s: &str) -> Result<AllowanceId, MintError> {
        let rest = s
            .strip_prefix(ALLOWANCE_PREFIX)
            .ok_or(MintError::BadAccountId)?;
        Ulid::from_string(rest).map_err(|_| MintError::BadAccountId)?;
        Ok(AllowanceId(s.to_string()))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AllowanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AllowanceId({})", self.0)
    }
}

impl std::fmt::Display for AllowanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const STREAM_PREFIX: &str = "strm_";

/// An opaque, validated stream identifier: `strm_<ULID>`. Minted by the store
/// inside `commit_stream_open`'s critical section (like `HoldId`), so an
/// idempotent replay returns the SAME original stream id. Ordered/hashable so it
/// keys the live streams map. Serializes as a bare string.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamId(String);

impl StreamId {
    /// Build an id from a ULID. Always valid by construction.
    pub fn new(ulid: Ulid) -> Self {
        StreamId(format!("{STREAM_PREFIX}{ulid}"))
    }

    /// Parse and validate an id string: `strm_` prefix then a valid 26-char
    /// ULID. Reuses [`MintError::BadAccountId`] (a malformed typed id is the same
    /// failure class) to avoid renumbering the stable error codes.
    pub fn parse(s: &str) -> Result<StreamId, MintError> {
        let rest = s
            .strip_prefix(STREAM_PREFIX)
            .ok_or(MintError::BadAccountId)?;
        Ulid::from_string(rest).map_err(|_| MintError::BadAccountId)?;
        Ok(StreamId(s.to_string()))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamId({})", self.0)
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_id_new_then_parse_round_trips() {
        let id = StreamId::new(Ulid::new());
        assert!(id.as_str().starts_with("strm_"));
        let parsed = StreamId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn stream_id_parse_rejects_malformed() {
        assert_eq!(StreamId::parse("nope"), Err(MintError::BadAccountId));
        assert_eq!(StreamId::parse("strm_"), Err(MintError::BadAccountId));
        assert_eq!(
            StreamId::parse("strm_notaulid"),
            Err(MintError::BadAccountId)
        );
        assert!(StreamId::parse("hold_001J000000000000000000000000").is_err());
    }

    #[test]
    fn nibble_round_trips_all_kinds() {
        for kind in [
            AccountKind::User,
            AccountKind::Org,
            AccountKind::Agent,
            AccountKind::Provider,
            AccountKind::Model,
            AccountKind::Treasury,
        ] {
            assert_eq!(AccountKind::from_nibble(kind.to_nibble()), Some(kind));
        }
        assert_eq!(AccountKind::from_nibble(6), None);
        assert_eq!(AccountKind::from_nibble(255), None);
    }

    #[test]
    fn new_then_parse_round_trips_and_preserves_kind() {
        let id = AccountId::new(AccountKind::Model, Ulid::new());
        assert!(id.as_str().starts_with("acct_4"));
        assert_eq!(id.kind(), AccountKind::Model);
        let parsed = AccountId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.kind(), AccountKind::Model);
    }

    #[test]
    fn parse_rejects_malformed_ids() {
        assert_eq!(AccountId::parse("nope"), Err(MintError::BadAccountId));
        assert_eq!(AccountId::parse("acct_"), Err(MintError::BadAccountId));
        // kind nibble 9 is out of range
        assert_eq!(
            AccountId::parse("acct_901J000000000000000000000000"),
            Err(MintError::BadAccountId)
        );
        // bad ULID body
        assert_eq!(
            AccountId::parse("acct_0notaulid"),
            Err(MintError::BadAccountId)
        );
    }

    #[test]
    fn allowance_id_new_then_parse_round_trips() {
        let id = AllowanceId::new(Ulid::new());
        assert!(id.as_str().starts_with("alw_"));
        let parsed = AllowanceId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn allowance_id_parse_rejects_malformed() {
        assert_eq!(AllowanceId::parse("nope"), Err(MintError::BadAccountId));
        assert_eq!(AllowanceId::parse("alw_"), Err(MintError::BadAccountId));
        assert_eq!(
            AllowanceId::parse("alw_notaulid"),
            Err(MintError::BadAccountId)
        );
        assert!(AllowanceId::parse("hold_001J000000000000000000000000").is_err());
    }

    #[test]
    fn hold_id_new_then_parse_round_trips() {
        let id = HoldId::new(Ulid::new());
        assert!(id.as_str().starts_with("hold_"));
        let parsed = HoldId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn hold_id_parse_rejects_malformed() {
        assert_eq!(HoldId::parse("nope"), Err(MintError::BadAccountId));
        assert_eq!(HoldId::parse("hold_"), Err(MintError::BadAccountId));
        assert_eq!(HoldId::parse("hold_notaulid"), Err(MintError::BadAccountId));
        // an account id is not a hold id
        assert!(HoldId::parse("acct_001J000000000000000000000000").is_err());
    }
}
