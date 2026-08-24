//! # vend-keys — provisioning-key lifecycle.
//!
//! Mints `vk_live_`/`vk_test_` secrets ONCE (the plaintext is returned to the
//! caller and never persisted), stores `argon2id(secret)` plus a non-secret
//! prefix shard for lookup, verifies a `Bearer` key into a [`SubKeyRecord`],
//! supports revocation (in-flight holds still resolve), and provides the
//! check-and-reserve spend-cap gate.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use async_trait::async_trait;
use subtle::ConstantTimeEq;

use vend_core::error::VendError;
use vend_core::ids::{AppId, EndUserId, SubKeyId, TenantId};
use vend_core::ports::UsageRepo;
use vend_core::records::{KeyScope, KeyStatus, SubKeyRecord};

/// Number of secret-body characters folded into the non-secret lookup prefix.
///
/// The body is high-entropy random hex, so this many leading chars form a
/// collision-resistant, unique lookup shard (`PREFIX_SHARD_LEN * 4` bits). It is
/// derived from CSPRNG bytes — NEVER from a ULID, whose timestamp-encoded leading
/// characters collide for keys minted in the same millisecond and would violate
/// the `sub_keys.prefix` UNIQUE constraint.
const PREFIX_SHARD_LEN: usize = 12;

/// Bytes of CSPRNG entropy in a secret body (`SECRET_BODY_BYTES * 2` hex chars,
/// `SECRET_BODY_BYTES * 8` bits).
const SECRET_BODY_BYTES: usize = 24;

/// Live vs test key environment. Encoded as the human-visible secret prefix.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum KeyEnv {
    /// Production key: `vk_live_…`.
    Live,
    /// Sandbox key: `vk_test_…`.
    Test,
}

impl KeyEnv {
    /// The visible string prefix for this environment.
    pub const fn as_str(self) -> &'static str {
        match self {
            KeyEnv::Live => "vk_live_",
            KeyEnv::Test => "vk_test_",
        }
    }

    /// Detect the environment from a full bearer secret.
    pub fn detect(secret: &str) -> Option<KeyEnv> {
        if secret.starts_with(KeyEnv::Live.as_str()) {
            Some(KeyEnv::Live)
        } else if secret.starts_with(KeyEnv::Test.as_str()) {
            Some(KeyEnv::Test)
        } else {
            None
        }
    }
}

/// Parameters for minting a new provisioning key.
#[derive(Clone, Debug)]
pub struct MintParams {
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Bound app (required for App / EndUser scope).
    pub app_id: Option<AppId>,
    /// Bound end-user (required for EndUser scope).
    pub end_user_id: Option<EndUserId>,
    /// Key reach.
    pub scope: KeyScope,
    /// Live vs test.
    pub env: KeyEnv,
    /// Per-key daily spend cap, in µ$.
    pub daily_cap_micros: Option<i64>,
    /// Per-end-user daily spend cap, in µ$.
    pub per_end_user_daily_cap_micros: Option<i64>,
}

/// A freshly-minted key: the row to persist, plus the ONE-TIME plaintext secret.
pub struct MintedKey {
    /// The row to persist (contains only the argon2 hash + prefix, no secret).
    pub record: SubKeyRecord,
    /// The plaintext secret — shown to the caller exactly once, never stored.
    pub secret: String,
}

/// The persistence port for key rows. Implemented in `vend-store`.
#[async_trait]
pub trait KeyRepo: Send + Sync {
    /// Insert a newly minted key row.
    async fn insert(&self, record: &SubKeyRecord) -> Result<(), VendError>;
    /// Look up a key by its non-secret prefix shard.
    async fn find_by_prefix(&self, prefix: &str) -> Result<Option<SubKeyRecord>, VendError>;
    /// Mark a key `revoked`. In-flight holds still settle (idempotent).
    async fn revoke(&self, id: &SubKeyId) -> Result<(), VendError>;
}

/// Generate a lowercase-hex secret body from [`SECRET_BODY_BYTES`] CSPRNG bytes.
fn random_hex_body() -> String {
    let mut raw = [0u8; SECRET_BODY_BYTES];
    OsRng.fill_bytes(&mut raw);
    let mut out = String::with_capacity(SECRET_BODY_BYTES * 2);
    for b in raw {
        // `from_digit` over 0..16 is always `Some`.
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// Mint a key: generate the secret, derive its lookup prefix, hash the secret
/// with argon2id, and assemble the row + one-time secret. The row is NOT yet
/// persisted — the caller passes `record` to [`KeyRepo::insert`].
pub fn mint(params: MintParams) -> Result<MintedKey, VendError> {
    // Secret entropy: CSPRNG bytes as lowercase hex. Random (not ULID/timestamp),
    // so the leading `PREFIX_SHARD_LEN` chars form a unique, collision-resistant
    // lookup shard.
    let body = random_hex_body();
    let secret = format!("{}{}", params.env.as_str(), body);
    let prefix = lookup_prefix(&secret).ok_or(VendError::Unauthorized)?;

    let salt = SaltString::generate(&mut OsRng);
    let secret_hash = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|e| VendError::Storage(format!("argon2 hash: {e}")))?
        .to_string();

    let record = SubKeyRecord {
        id: SubKeyId::generate(),
        tenant_id: params.tenant_id,
        app_id: params.app_id,
        end_user_id: params.end_user_id,
        scope: params.scope,
        status: KeyStatus::Active,
        prefix,
        secret_hash,
        daily_cap_micros: params.daily_cap_micros,
        per_end_user_daily_cap_micros: params.per_end_user_daily_cap_micros,
    };
    Ok(MintedKey { record, secret })
}

/// Derive the non-secret lookup prefix from a full secret: the env prefix plus
/// the first [`PREFIX_SHARD_LEN`] body chars. Returns `None` for a non-`vk_`
/// secret.
pub fn lookup_prefix(secret: &str) -> Option<String> {
    let env = KeyEnv::detect(secret)?;
    let body = &secret[env.as_str().len()..];
    if !body.is_ascii() || body.len() < PREFIX_SHARD_LEN {
        return None;
    }
    Some(format!("{}{}", env.as_str(), &body[..PREFIX_SHARD_LEN]))
}

/// Verify a `Bearer` provisioning secret: derive its prefix, load the row, and
/// constant-time argon2-verify the secret. Rejects malformed, unknown, revoked,
/// or non-matching secrets as [`VendError::Unauthorized`].
pub async fn verify<R: KeyRepo + ?Sized>(
    repo: &R,
    secret: &str,
) -> Result<SubKeyRecord, VendError> {
    let prefix = lookup_prefix(secret).ok_or(VendError::Unauthorized)?;
    let record = repo
        .find_by_prefix(&prefix)
        .await?
        .ok_or(VendError::Unauthorized)?;

    // Defense in depth: the stored prefix must match in constant time.
    if record
        .prefix
        .as_bytes()
        .ct_eq(prefix.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(VendError::Unauthorized);
    }

    let parsed = PasswordHash::new(&record.secret_hash)
        .map_err(|e| VendError::Storage(format!("argon2 parse: {e}")))?;
    Argon2::default()
        .verify_password(secret.as_bytes(), &parsed)
        .map_err(|_| VendError::Unauthorized)?;

    if record.status != KeyStatus::Active {
        return Err(VendError::Unauthorized);
    }
    Ok(record)
}

/// The FAST spend-cap pre-check (advisory under concurrency).
///
/// Compares projected `SUM(charge_micros)` for the UTC day against the key's
/// `daily_cap_micros` and, for a targeted end-user, `per_end_user_daily_cap_micros`,
/// using a bare SELECT-SUM. It exists to reject clearly-over-cap requests early —
/// BEFORE any Mint hold is opened — so no reserve is taken on a doomed call.
///
/// NOTE: this predicate is NOT atomic on its own: two concurrent requests can
/// read the same SUM and both pass. The AUTHORITATIVE, TOCTOU-safe gate is
/// [`UsageRepo::record_pending_within_cap`], which re-evaluates the projected
/// total against the caps and inserts the pending row in ONE serialized
/// transaction (per-key/per-end-user advisory lock in the Postgres store). Any
/// request that slips past this pre-check under a race is rejected there, and
/// its already-opened hold is released.
pub async fn check_spend_cap<U: UsageRepo + ?Sized>(
    usage: &U,
    key: &SubKeyRecord,
    end_user_id: Option<&EndUserId>,
    estimate_charge_micros: i64,
    utc_day: &str,
) -> Result<(), VendError> {
    if let Some(cap) = key.daily_cap_micros {
        let spent = usage.sum_charge_for_key_day(&key.id, utc_day).await?;
        if spent.saturating_add(estimate_charge_micros) > cap {
            return Err(VendError::SpendCapExceeded);
        }
    }
    if let (Some(cap), Some(eu)) = (key.per_end_user_daily_cap_micros, end_user_id) {
        let spent = usage.sum_charge_for_end_user_day(eu, utc_day).await?;
        if spent.saturating_add(estimate_charge_micros) > cap {
            return Err(VendError::SpendCapExceeded);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> MintParams {
        MintParams {
            tenant_id: TenantId::generate(),
            app_id: Some(AppId::generate()),
            end_user_id: None,
            scope: KeyScope::App,
            env: KeyEnv::Live,
            daily_cap_micros: None,
            per_end_user_daily_cap_micros: None,
        }
    }

    #[test]
    fn minted_secret_is_live_prefixed_and_hashed() {
        let minted = mint(params()).unwrap();
        assert!(minted.secret.starts_with("vk_live_"));
        assert!(minted.record.secret_hash.starts_with("$argon2"));
        assert_ne!(minted.record.secret_hash, minted.secret);
        assert_eq!(minted.record.prefix, lookup_prefix(&minted.secret).unwrap());
    }

    #[test]
    fn prefix_only_covers_env_plus_shard() {
        let minted = mint(params()).unwrap();
        assert_eq!(
            minted.record.prefix.len(),
            "vk_live_".len() + PREFIX_SHARD_LEN
        );
        assert!(minted.secret.starts_with(&minted.record.prefix));
    }

    #[test]
    fn malformed_unicode_secret_is_rejected_without_panicking() {
        assert_eq!(lookup_prefix("vk_live_éééééé"), None);
    }

    #[test]
    fn prefixes_are_unique_across_rapid_minting() {
        // Regression: a ULID-derived body shares a timestamp prefix for keys minted
        // in the same millisecond, colliding the lookup shard. Random hex must not.
        // A handful of back-to-back mints reliably lands in the same millisecond,
        // which is exactly what broke the old ULID-derived shard. argon2 is
        // deliberately slow, so keep the count small but > 1.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..16 {
            let minted = mint(params()).unwrap();
            assert!(
                seen.insert(minted.record.prefix.clone()),
                "duplicate prefix shard minted: {}",
                minted.record.prefix
            );
        }
    }

    struct MapRepo {
        by_prefix: std::sync::Mutex<std::collections::HashMap<String, SubKeyRecord>>,
    }

    #[async_trait]
    impl KeyRepo for MapRepo {
        async fn insert(&self, record: &SubKeyRecord) -> Result<(), VendError> {
            self.by_prefix
                .lock()
                .unwrap()
                .insert(record.prefix.clone(), record.clone());
            Ok(())
        }
        async fn find_by_prefix(&self, prefix: &str) -> Result<Option<SubKeyRecord>, VendError> {
            Ok(self.by_prefix.lock().unwrap().get(prefix).cloned())
        }
        async fn revoke(&self, _id: &SubKeyId) -> Result<(), VendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn mint_then_verify_round_trips() {
        let repo = MapRepo {
            by_prefix: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let minted = mint(params()).unwrap();
        let secret = minted.secret.clone();
        repo.insert(&minted.record).await.unwrap();
        let rec = verify(&repo, &secret).await.expect("verify should succeed");
        assert_eq!(rec.id, minted.record.id);
        // Wrong secret is rejected.
        assert_eq!(
            verify(&repo, "vk_live_deadbeefwrong").await.unwrap_err(),
            VendError::Unauthorized
        );
    }
}
