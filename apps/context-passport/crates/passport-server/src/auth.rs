//! Constant-time `X-API-Key` check, fail-closed on a missing or empty key.
//!
//! Two kinds of key exist:
//! - the operator key (`PASSPORT_API_KEY`) — full access to every passport;
//! - per-owner tenant keys (`PASSPORT_OWNER_KEYS=owner:key,owner2:key2`) — a
//!   tenant key acts ONLY as its owner: creates are stamped with that owner,
//!   lists are filtered to it, and every id-scoped route rejects passports it
//!   does not own (reported as not-found so ids cannot be probed).

use axum::http::HeaderMap;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;
use passport_core::PassportError;

/// Who an authenticated request acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// Operator key: unrestricted.
    Operator,
    /// Tenant key bound to one owner account.
    Owner(String),
}

impl Principal {
    /// Whether this principal may read/modify a passport owned by `owner`.
    pub fn may_access(&self, owner: &str) -> bool {
        match self {
            Principal::Operator => true,
            Principal::Owner(me) => me == owner,
        }
    }

    /// The owner this principal is pinned to, if any.
    pub fn owner(&self) -> Option<&str> {
        match self {
            Principal::Operator => None,
            Principal::Owner(me) => Some(me),
        }
    }
}

/// Per-owner tenant keys.
#[derive(Clone, Debug, Default)]
pub struct OwnerKeys {
    entries: Vec<(String, String)>,
}

impl OwnerKeys {
    /// Parse `owner:key,owner2:key2`. Keys must be at least 24 characters.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut entries = Vec::new();
        for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (owner, key) = pair
                .split_once(':')
                .ok_or_else(|| format!("owner key entry `{pair}` must be owner:key"))?;
            let (owner, key) = (owner.trim(), key.trim());
            if owner.is_empty() {
                return Err("owner key entry has an empty owner".into());
            }
            if key.len() < 24 {
                return Err(format!("key for owner `{owner}` must be at least 24 characters"));
            }
            entries.push((owner.to_string(), key.to_string()));
        }
        Ok(Self { entries })
    }

    fn owner_for(&self, presented: &[u8]) -> Option<String> {
        // Compare against every entry (no early exit) to keep timing flat.
        let mut found = None;
        for (owner, key) in &self.entries {
            let hit: bool = key.as_bytes().ct_eq(presented).into();
            if hit && found.is_none() {
                found = Some(owner.clone());
            }
        }
        found
    }
}

/// Authenticate the request and return who it acts as.
pub fn authorize(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let presented = headers
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .as_bytes();
    if presented.is_empty() {
        return Err(ApiError(PassportError::Unauthorized));
    }
    let expected = state.api_key.as_bytes();
    // `ct_eq` returns 0 for unequal lengths, then compares bytes with no
    // early-exit timing leak on the shared prefix.
    let operator: bool = expected.ct_eq(presented).into();
    let owner = state.owner_keys.owner_for(presented);
    if operator && !expected.is_empty() {
        return Ok(Principal::Operator);
    }
    match owner {
        Some(owner) => Ok(Principal::Owner(owner)),
        None => Err(ApiError(PassportError::Unauthorized)),
    }
}

/// Authenticate AND require access to passport `id`.
pub async fn authorize_passport(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
) -> Result<Principal, ApiError> {
    let principal = authorize(state, headers)?;
    state.service.ensure_access(id, &principal).await?;
    Ok(principal)
}
