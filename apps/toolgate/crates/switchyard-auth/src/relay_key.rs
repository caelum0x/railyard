//! Relay `sk-t0-` keys, shared by the model rail and the tool rail.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use rand::distributions::{Alphanumeric, DistString};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use sqlx::Row;
use subtle::ConstantTimeEq;
use switchyard_core::ApiError;

pub const PUBLIC_ID_LENGTH: usize = 12;
pub const LONG_KEY_LENGTH: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKey {
    pub public_id: String,
    /// Relay organization / workspace the key was minted under.
    pub organization: String,
    pub workspace: String,
}

/// `(public_id, sha256_hex(long_key))` or a format error.
pub fn parse(raw: &str) -> Result<(String, String), ApiError> {
    let parts: Vec<&str> = raw.trim().split('-').collect();
    let [sk, t0, public_id, long_key] = parts.as_slice() else {
        return Err(ApiError::unauthorized("API key must look like sk-t0-<public_id>-<long_key>"));
    };
    if *sk != "sk" || *t0 != "t0" || public_id.len() != PUBLIC_ID_LENGTH || long_key.len() != LONG_KEY_LENGTH {
        return Err(ApiError::unauthorized("API key must look like sk-t0-<public_id>-<long_key>"));
    }
    if !public_id.chars().all(|c| c.is_ascii_alphanumeric()) || !long_key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ApiError::unauthorized("API key has invalid characters"));
    }
    Ok((public_id.to_string(), hex::encode(Sha256::digest(long_key.as_bytes()))))
}

/// A fresh key in Relay's format: `(raw, public_id, hash)`.
pub fn generate() -> (SecretString, String, String) {
    let mut rng = rand::rngs::OsRng;
    let public_id = Alphanumeric.sample_string(&mut rng, PUBLIC_ID_LENGTH);
    let long_key = Alphanumeric.sample_string(&mut rng, LONG_KEY_LENGTH);
    let hash = hex::encode(Sha256::digest(long_key.as_bytes()));
    (SecretString::from(format!("sk-t0-{public_id}-{long_key}")), public_id, hash)
}

#[async_trait]
pub trait KeyVerifier: Send + Sync {
    async fn verify(&self, raw: &str) -> Result<VerifiedKey, ApiError>;
    /// Mint a key in Relay's store so it works on both rails.
    async fn mint(&self, organization: &str, workspace: &str, description: &str) -> Result<(SecretString, String), ApiError>;
    async fn disable(&self, public_id: &str) -> Result<(), ApiError>;
}

/// Relay's Postgres key table (`tensorzero_auth_api_key`), pinned to the
/// schema of Relay's `tensorzero-auth` crate.
pub struct PgRelayKeys {
    pool: PgPool,
}

impl PgRelayKeys {
    pub fn new(pool: PgPool) -> Self {
        PgRelayKeys { pool }
    }
}

#[async_trait]
impl KeyVerifier for PgRelayKeys {
    async fn verify(&self, raw: &str) -> Result<VerifiedKey, ApiError> {
        let (public_id, hash) = parse(raw)?;
        let row = sqlx::query(
            "SELECT public_id, organization, workspace, disabled_at, expires_at FROM tensorzero_auth_api_key WHERE public_id = $1 AND hash = $2",
        )
        .bind(&public_id)
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ApiError::new(503, "auth_unavailable", format!("relay key store: {e}")))?
        .ok_or_else(|| ApiError::unauthorized("unknown API key"))?;
        let disabled: Option<chrono::DateTime<chrono::Utc>> = row.get("disabled_at");
        let expires: Option<chrono::DateTime<chrono::Utc>> = row.get("expires_at");
        if disabled.is_some() {
            return Err(ApiError::unauthorized("API key is disabled"));
        }
        if expires.map(|e| e <= chrono::Utc::now()).unwrap_or(false) {
            return Err(ApiError::unauthorized("API key has expired"));
        }
        Ok(VerifiedKey { public_id, organization: row.get("organization"), workspace: row.get("workspace") })
    }

    async fn mint(&self, organization: &str, workspace: &str, description: &str) -> Result<(SecretString, String), ApiError> {
        let (raw, public_id, hash) = generate();
        sqlx::query("INSERT INTO tensorzero_auth_api_key (organization, workspace, description, public_id, hash, expires_at) VALUES ($1,$2,$3,$4,$5,NULL)")
            .bind(organization)
            .bind(workspace)
            .bind(description)
            .bind(&public_id)
            .bind(&hash)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::new(503, "auth_unavailable", format!("relay key store: {e}")))?;
        Ok((raw, public_id))
    }

    async fn disable(&self, public_id: &str) -> Result<(), ApiError> {
        sqlx::query("UPDATE tensorzero_auth_api_key SET disabled_at = now(), updated_at = now() WHERE public_id = $1")
            .bind(public_id)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::new(503, "auth_unavailable", format!("relay key store: {e}")))?;
        Ok(())
    }
}

/// In-memory key store with the same semantics (tests and `memory` dev mode).
#[derive(Default)]
pub struct MemRelayKeys {
    keys: Mutex<HashMap<String, (String, VerifiedKey, bool)>>,
}

impl MemRelayKeys {
    pub fn new() -> Self {
        Self::default()
    }
    /// Register a raw key directly (tests, bootstrap).
    pub fn insert_raw(&self, raw: &str, organization: &str, workspace: &str) -> String {
        let (public_id, hash) = parse(raw).expect("valid test key");
        self.keys.lock().expect("keys poisoned").insert(
            public_id.clone(),
            (hash, VerifiedKey { public_id: public_id.clone(), organization: organization.into(), workspace: workspace.into() }, false),
        );
        public_id
    }
}

#[async_trait]
impl KeyVerifier for MemRelayKeys {
    async fn verify(&self, raw: &str) -> Result<VerifiedKey, ApiError> {
        let (public_id, hash) = parse(raw)?;
        let keys = self.keys.lock().expect("keys poisoned");
        match keys.get(&public_id) {
            Some((h, _, true)) if bool::from(h.as_bytes().ct_eq(hash.as_bytes())) => Err(ApiError::unauthorized("API key is disabled")),
            Some((h, v, false)) if bool::from(h.as_bytes().ct_eq(hash.as_bytes())) => Ok(v.clone()),
            _ => Err(ApiError::unauthorized("unknown API key")),
        }
    }
    async fn mint(&self, organization: &str, workspace: &str, _description: &str) -> Result<(SecretString, String), ApiError> {
        let (raw, _, _) = generate();
        let public_id = self.insert_raw(raw.expose_secret(), organization, workspace);
        Ok((raw, public_id))
    }
    async fn disable(&self, public_id: &str) -> Result<(), ApiError> {
        if let Some(e) = self.keys.lock().expect("keys poisoned").get_mut(public_id) {
            e.2 = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_matches_relay_format() {
        let (raw, public_id, hash) = generate();
        let (p, h) = parse(raw.expose_secret()).unwrap();
        assert_eq!((p, h), (public_id, hash));
        for bad in ["", "sk-t1-aaaaaaaaaaaa-x", "sk-t0-short-long", "Bearer sk-t0", "sk-t0-aaaaaaaaaaaa-bbb-ccc"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn mem_keys_verify_and_disable() {
        let s = MemRelayKeys::new();
        let (raw, id) = s.mint("acme", "prj_1", "t").await.unwrap();
        let v = s.verify(raw.expose_secret()).await.unwrap();
        assert_eq!((v.public_id.as_str(), v.workspace.as_str()), (id.as_str(), "prj_1"));
        let forged = format!("sk-t0-{id}-{}", "A".repeat(48));
        assert!(s.verify(&forged).await.is_err());
        s.disable(&id).await.unwrap();
        assert_eq!(s.verify(raw.expose_secret()).await.unwrap_err().message, "API key is disabled");
    }
}
