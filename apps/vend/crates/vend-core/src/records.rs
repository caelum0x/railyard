//! Shared, storage-agnostic domain records used across tenancy/keys/store.
//!
//! These are the row shapes the repo ports read/write. Crypto (argon2 hashing,
//! bearer verification) lives in `vend-keys`; only the *shape* lives here so
//! both `vend-tenancy` (resolution) and `vend-keys` (verify) can name it without
//! a circular dependency.

use serde::{Deserialize, Serialize};

use crate::ids::{AppId, EndUserId, SubKeyId, TenantId};

/// The reach of a provisioning key. Nibble values are stable and mirror the SQL
/// `scope` column.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyScope {
    /// Scope 0: authority over the whole tenant.
    Tenant,
    /// Scope 1: authority over a single app.
    App,
    /// Scope 2: authority over a single end-user of an app.
    EndUser,
}

impl KeyScope {
    /// Stable nibble encoding (`0`–`2`).
    pub const fn to_u8(self) -> u8 {
        match self {
            KeyScope::Tenant => 0,
            KeyScope::App => 1,
            KeyScope::EndUser => 2,
        }
    }
    /// Inverse of [`KeyScope::to_u8`]; `None` for out-of-range values.
    pub const fn from_u8(n: u8) -> Option<KeyScope> {
        match n {
            0 => Some(KeyScope::Tenant),
            1 => Some(KeyScope::App),
            2 => Some(KeyScope::EndUser),
            _ => None,
        }
    }
}

/// Lifecycle status of a provisioning key. A revoked key stops authorizing new
/// work, but in-flight holds still resolve (settlement is idempotent).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    /// Key authorizes new requests.
    Active,
    /// Key is revoked; new requests are rejected as `Unauthorized`.
    Revoked,
}

/// A persisted provisioning-key row. The plaintext secret is NEVER stored — only
/// its argon2id hash plus the non-secret prefix shard used for lookup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubKeyRecord {
    /// Key id (`sub_key_<ULID>`).
    pub id: SubKeyId,
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Bound app (required for App / EndUser scope).
    pub app_id: Option<AppId>,
    /// Bound end-user (required for EndUser scope).
    pub end_user_id: Option<EndUserId>,
    /// Key reach.
    pub scope: KeyScope,
    /// Lifecycle status.
    pub status: KeyStatus,
    /// Non-secret lookup shard (e.g. `vk_live_ab12cd`), unique per key.
    pub prefix: String,
    /// `argon2id` hash of the full secret. Verified in constant time.
    pub secret_hash: String,
    /// Per-key daily spend cap over `SUM(charge_micros)`, in µ$.
    pub daily_cap_micros: Option<i64>,
    /// Per-end-user daily spend cap, in µ$.
    pub per_end_user_daily_cap_micros: Option<i64>,
}
