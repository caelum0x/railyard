//! Validated id newtypes for the Abacus domain.
//!
//! Two id families:
//!
//! - **ULID newtypes** (`AgentId`/`RunId`/`BudgetId`) — `<prefix><ULID>` where
//!   `<ULID>` is a 26-char Crockford base32 body; `generate()` mints a fresh id,
//!   `parse()` validates a wire string.
//! - **Mint account id** (`AccountId`) — the `acct_…` ledger identity minted by
//!   mint-server, parsed (prefix-checked) at the trust boundary but treated as an
//!   opaque token thereafter (its body is not a ULID).
//!
//! Provider `ait1…` payee addresses are a THIRD, distinct concept resolved to an
//! [`AccountId`] via the Mint address lookup — see [`Ait1Address`].

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::AbacusError;

macro_rules! ulid_id {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The id string prefix, including the trailing underscore.
            pub const PREFIX: &'static str = $prefix;

            /// Mint a fresh, always-valid id from a new ULID.
            pub fn generate() -> Self {
                $name(format!("{}{}", $prefix, Ulid::new()))
            }

            /// Build from an existing ULID. Always valid by construction.
            pub fn from_ulid(ulid: Ulid) -> Self {
                $name(format!("{}{}", $prefix, ulid))
            }

            /// Parse + validate at the boundary: correct prefix then a valid
            /// 26-char ULID body. A malformed id maps to [`AbacusError::BadId`].
            pub fn parse(s: &str) -> Result<Self, AbacusError> {
                let rest = s.strip_prefix($prefix).ok_or(AbacusError::BadId)?;
                Ulid::from_string(rest).map_err(|_| AbacusError::BadId)?;
                Ok($name(s.to_string()))
            }

            /// The underlying validated string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

ulid_id!(
    /// Autonomous agent id: `agent_<ULID>`.
    AgentId,
    "agent_"
);
ulid_id!(
    /// A single agent run / session id: `run_<ULID>`.
    RunId,
    "run_"
);
ulid_id!(
    /// A per-agent spend budget id: `budget_<ULID>`.
    BudgetId,
    "budget_"
);

/// A Mint ledger account id (`acct_…`), the funding/treasury/payee identity.
///
/// Minted by mint-server; the body is NOT a ULID, so we validate only the
/// `acct_` prefix + non-empty body at the boundary and otherwise treat it as an
/// opaque token.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    /// The mint account id prefix, including the trailing underscore.
    pub const PREFIX: &'static str = "acct_";

    /// Parse + validate at the boundary: `acct_` prefix with a non-empty body.
    pub fn parse(s: &str) -> Result<Self, AbacusError> {
        let rest = s.strip_prefix(Self::PREFIX).ok_or(AbacusError::BadId)?;
        if rest.is_empty() {
            return Err(AbacusError::BadId);
        }
        Ok(AccountId(s.to_string()))
    }

    /// The underlying validated string.
    pub fn as_str(&self) -> &str {
        &self.0
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

/// A provider payee address (`ait1…`), resolved to an [`AccountId`] via the Mint
/// address lookup before a capture settles to it (the aitp-client gap-#1 bridge).
///
/// Validated only for the `ait1` prefix + non-empty body at the boundary; the
/// checksum/bech32 body is authoritative on mint-server, not re-derived here.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ait1Address(String);

impl Ait1Address {
    /// The provider address human-readable prefix.
    pub const PREFIX: &'static str = "ait1";

    /// Parse + validate at the boundary: `ait1` prefix with a non-empty body.
    pub fn parse(s: &str) -> Result<Self, AbacusError> {
        let rest = s.strip_prefix(Self::PREFIX).ok_or(AbacusError::BadId)?;
        if rest.is_empty() {
            return Err(AbacusError::BadId);
        }
        Ok(Ait1Address(s.to_string()))
    }

    /// The underlying validated string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Ait1Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ait1Address({})", self.0)
    }
}

impl std::fmt::Display for Ait1Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_parse_round_trips() {
        let id = AgentId::generate();
        assert!(id.as_str().starts_with("agent_"));
        assert_eq!(AgentId::parse(id.as_str()).unwrap(), id);
    }

    #[test]
    fn cross_prefix_is_rejected() {
        let run = RunId::generate();
        assert!(AgentId::parse(run.as_str()).is_err());
        assert_eq!(BudgetId::parse("nope"), Err(AbacusError::BadId));
    }

    #[test]
    fn account_id_parses_opaque_body_at_boundary() {
        let a = AccountId::parse("acct_agentTreasury01").unwrap();
        assert_eq!(a.as_str(), "acct_agentTreasury01");
        assert_eq!(AccountId::parse("acct_"), Err(AbacusError::BadId));
        assert_eq!(AccountId::parse("nope_x"), Err(AbacusError::BadId));
    }

    #[test]
    fn ait1_address_parses_prefix_at_boundary() {
        let p = Ait1Address::parse("ait1qprovider0payee").unwrap();
        assert_eq!(p.as_str(), "ait1qprovider0payee");
        assert_eq!(Ait1Address::parse("ait1"), Err(AbacusError::BadId));
        assert_eq!(Ait1Address::parse("acct_x"), Err(AbacusError::BadId));
    }
}
