//! The live AITP-402 payer-key registry.
//!
//! Payer verifying keys come from two places:
//! - the DB (`payer_keys`, managed from `POST /v1/payer-keys` and the operator
//!   console), mirrored into an in-process map so the synchronous
//!   [`PayerKeys`] lookup inside `Aitp402::verify_and_settle` never blocks on
//!   I/O;
//! - the optional static `MINT_PAYER_KEYS_JSON` bootstrap set (the fallback).
//!
//! A key revoked in the DB is refused even when the same `(payer, pubkey_id)`
//! is still present in the static fallback, so revocation always wins. The
//! mirror is refreshed on every API mutation and periodically by the
//! background sweeper, so multi-instance deployments converge within one
//! sweep interval.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use ed25519_dalek::VerifyingKey;
use mint_core::MintError;
use mint_protocol::PayerKeys;
use mint_store::{LedgerStore, PayerKeyRecord};

type KeyId = (String, String);

#[derive(Default)]
struct Mirror {
    active: HashMap<KeyId, VerifyingKey>,
    revoked: HashSet<KeyId>,
}

/// DB-backed payer keys (mirrored) layered over a static fallback registry.
pub struct PayerKeyRegistry {
    fallback: Arc<dyn PayerKeys>,
    mirror: RwLock<Mirror>,
}

impl PayerKeyRegistry {
    /// Wrap the static (env / test) registry. The DB mirror starts empty until
    /// [`PayerKeyRegistry::reload`] runs.
    pub fn new(fallback: Arc<dyn PayerKeys>) -> Self {
        PayerKeyRegistry {
            fallback,
            mirror: RwLock::new(Mirror::default()),
        }
    }

    /// Replace the DB mirror from a full listing of payer-key records.
    /// Records whose stored public key no longer parses are skipped (and
    /// logged) rather than poisoning the whole registry.
    pub fn replace_all(&self, records: &[PayerKeyRecord]) {
        let mut next = Mirror::default();
        for rec in records {
            let id = (rec.payer.clone(), rec.pubkey_id.clone());
            if rec.revoked_ms.is_some() {
                if !next.active.contains_key(&id) {
                    next.revoked.insert(id);
                }
                continue;
            }
            match parse_public_key_hex(&rec.public_key_hex) {
                Ok(key) => {
                    next.revoked.remove(&id);
                    next.active.insert(id, key);
                }
                Err(e) => tracing::error!(
                    payer = %rec.payer,
                    pubkey_id = %rec.pubkey_id,
                    error = %e,
                    "stored AITP payer key is invalid; skipped"
                ),
            }
        }
        match self.mirror.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Reload the mirror from the store. Returns the number of ACTIVE keys.
    pub async fn reload<S: LedgerStore + ?Sized>(&self, store: &S) -> Result<usize, MintError> {
        let records = store.list_payer_keys(None).await?;
        self.replace_all(&records);
        Ok(self.active_count())
    }

    /// Number of active DB-backed keys in the mirror.
    pub fn active_count(&self) -> usize {
        match self.mirror.read() {
            Ok(g) => g.active.len(),
            Err(p) => p.into_inner().active.len(),
        }
    }
}

impl PayerKeys for PayerKeyRegistry {
    fn verifying_key(&self, payer: &str, pubkey_id: &str) -> Option<VerifyingKey> {
        let id = (payer.to_string(), pubkey_id.to_string());
        {
            let guard = match self.mirror.read() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if let Some(key) = guard.active.get(&id) {
                return Some(*key);
            }
            if guard.revoked.contains(&id) {
                return None;
            }
        }
        self.fallback.verifying_key(payer, pubkey_id)
    }
}

/// Parse a lowercase/uppercase hex Ed25519 public key (32 bytes).
pub fn parse_public_key_hex(raw: &str) -> Result<VerifyingKey, String> {
    let decoded = hex::decode(raw.trim()).map_err(|_| "public_key_hex must be hex".to_string())?;
    let bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| "public_key_hex must decode to 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| "public_key_hex is not a valid Ed25519 key".into())
}
