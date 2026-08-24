//! ULID id newtypes: `ten_`/`app_`/`eu_`/`chg_`/`sub_key_`.
//!
//! Each is a validated opaque `String` newtype — `<prefix><ULID>` where `<ULID>`
//! is a 26-char Crockford base32 ULID. `generate()` mints a fresh id; `parse()`
//! validates a wire string; a malformed id maps to [`VendError::BadId`].

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::VendError;

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

            /// Parse + validate: correct prefix then a valid 26-char ULID body.
            pub fn parse(s: &str) -> Result<Self, VendError> {
                let rest = s.strip_prefix($prefix).ok_or(VendError::BadId)?;
                Ulid::from_string(rest).map_err(|_| VendError::BadId)?;
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
    /// Tenant (reseller) id: `ten_<ULID>`.
    TenantId,
    "ten_"
);
ulid_id!(
    /// App id (a tenant's product surface): `app_<ULID>`.
    AppId,
    "app_"
);
ulid_id!(
    /// End-user id (an app's customer): `eu_<ULID>`.
    EndUserId,
    "eu_"
);
ulid_id!(
    /// Charge / metered-usage row id: `chg_<ULID>`.
    ChargeId,
    "chg_"
);
ulid_id!(
    /// Provisioning sub-key id: `sub_key_<ULID>`.
    SubKeyId,
    "sub_key_"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_parse_round_trips() {
        let id = TenantId::generate();
        assert!(id.as_str().starts_with("ten_"));
        assert_eq!(TenantId::parse(id.as_str()).unwrap(), id);
    }

    #[test]
    fn cross_prefix_is_rejected() {
        let app = AppId::generate();
        assert!(TenantId::parse(app.as_str()).is_err());
        assert_eq!(SubKeyId::parse("nope"), Err(VendError::BadId));
    }
}
