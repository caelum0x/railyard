//! [`InMemoryHoldStore`] — the single-instance implementation of the
//! [`abacus_core::hold_store::HoldStore`] port.
//!
//! A `Mutex<HashMap>` of open holds keyed by `hold_id`. This is the DEFAULT
//! backend for a single Abacus instance (mirroring `local_store::LocalStore`'s
//! role for policy/receipts): it survives handler panics because it lives outside
//! the request task, so the reaper can still find the orphan even after the caller
//! that opened the hold has unwound. It does NOT survive a process restart — that,
//! and cross-instance reaping, is what the future shared transactional backend
//! (swapped in behind the same trait) provides. See
//! [`abacus_core::hold_store`] for the seam rationale.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use abacus_core::error::AbacusError;
use abacus_core::hold_store::{HoldRecord, HoldStore};

/// An in-memory, mutex-guarded hold table for single-instance deployments.
pub struct InMemoryHoldStore {
    inner: Mutex<HashMap<String, HoldRecord>>,
}

impl Default for InMemoryHoldStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryHoldStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        InMemoryHoldStore {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Number of currently-tracked holds (best-effort; a poisoned lock reads 0).
    pub fn len(&self) -> usize {
        self.inner.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// `true` iff no holds are currently tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn lock_err(_poisoned: impl std::fmt::Debug) -> AbacusError {
    AbacusError::Storage("hold store lock poisoned".to_string())
}

#[async_trait]
impl HoldStore for InMemoryHoldStore {
    async fn create(&self, record: HoldRecord) -> Result<(), AbacusError> {
        let mut map = self.inner.lock().map_err(lock_err)?;
        match map.get(&record.hold_id) {
            // Idempotent re-registration of the identical row.
            Some(existing) if *existing == record => Ok(()),
            // A different row for the same hold id is a genuine conflict.
            Some(_) => Err(AbacusError::IdempotencyConflict),
            None => {
                map.insert(record.hold_id.clone(), record);
                Ok(())
            }
        }
    }

    async fn get(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        let map = self.inner.lock().map_err(lock_err)?;
        Ok(map.get(hold_id).cloned())
    }

    async fn list(&self) -> Result<Vec<HoldRecord>, AbacusError> {
        let map = self.inner.lock().map_err(lock_err)?;
        let mut records: Vec<HoldRecord> = map.values().cloned().collect();
        // Deterministic order (oldest first, then hold_id) so a reap sweep — and
        // the tests that assert on it — are reproducible despite HashMap's random
        // iteration order.
        records.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.hold_id.cmp(&b.hold_id))
        });
        Ok(records)
    }

    async fn remove(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        let mut map = self.inner.lock().map_err(lock_err)?;
        Ok(map.remove(hold_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hold_id: &str, created_at_ms: i64) -> HoldRecord {
        HoldRecord {
            hold_id: hold_id.to_string(),
            idem_key: format!("idem_{hold_id}"),
            reserved_micros: 2_500,
            created_at_ms,
            deadline_ms: None,
        }
    }

    #[tokio::test]
    async fn create_get_remove_roundtrip() {
        let store = InMemoryHoldStore::new();
        assert!(store.is_empty());

        store.create(rec("hold_a", 10)).await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("hold_a").await.unwrap(), Some(rec("hold_a", 10)));
        assert_eq!(store.get("hold_missing").await.unwrap(), None);

        let removed = store.remove("hold_a").await.unwrap();
        assert_eq!(removed, Some(rec("hold_a", 10)));
        assert!(store.is_empty());
        // Removing an absent hold is a clean None, never an error.
        assert_eq!(store.remove("hold_a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn create_is_idempotent_on_identical_record() {
        let store = InMemoryHoldStore::new();
        store.create(rec("hold_a", 10)).await.unwrap();
        // Same row again -> no-op Ok.
        store.create(rec("hold_a", 10)).await.unwrap();
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn create_conflicting_record_errors() {
        let store = InMemoryHoldStore::new();
        store.create(rec("hold_a", 10)).await.unwrap();
        let mut different = rec("hold_a", 10);
        different.reserved_micros = 999;
        let err = store.create(different).await.unwrap_err();
        assert_eq!(err, AbacusError::IdempotencyConflict);
    }

    #[tokio::test]
    async fn list_is_ordered_oldest_first() {
        let store = InMemoryHoldStore::new();
        store.create(rec("hold_c", 30)).await.unwrap();
        store.create(rec("hold_a", 10)).await.unwrap();
        store.create(rec("hold_b", 20)).await.unwrap();
        let ids: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.hold_id)
            .collect();
        assert_eq!(ids, vec!["hold_a", "hold_b", "hold_c"]);
    }
}
