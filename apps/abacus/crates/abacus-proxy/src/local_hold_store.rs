//! Restart-safe, single-instance hold tracking.
//!
//! Every mutation rewrites one small JSON snapshot through an atomic rename.
//! Mint remains authoritative for money; this file only gives the startup reaper
//! enough information to release reservations orphaned by a process crash.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use abacus_core::error::AbacusError;
use abacus_core::hold_store::{HoldRecord, HoldStore};
use async_trait::async_trait;

pub struct LocalHoldStore {
    inner: Mutex<Snapshot>,
}

struct Snapshot {
    path: PathBuf,
    records: HashMap<String, HoldRecord>,
    leases: HashMap<String, (String, i64)>,
}

impl LocalHoldStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AbacusError> {
        let path = path.as_ref().to_path_buf();
        let records = match std::fs::read(&path) {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => HashMap::new(),
            Ok(bytes) => {
                let rows: Vec<HoldRecord> = serde_json::from_slice(&bytes).map_err(|error| {
                    AbacusError::Storage(format!(
                        "parsing hold snapshot {}: {error}",
                        path.display()
                    ))
                })?;
                let mut records = HashMap::with_capacity(rows.len());
                for row in rows {
                    if records.insert(row.hold_id.clone(), row).is_some() {
                        return Err(AbacusError::Storage(format!(
                            "duplicate hold id in {}",
                            path.display()
                        )));
                    }
                }
                records
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                return Err(AbacusError::Storage(format!(
                    "reading hold snapshot {}: {error}",
                    path.display()
                )))
            }
        };
        Ok(Self {
            inner: Mutex::new(Snapshot {
                path,
                records,
                leases: HashMap::new(),
            }),
        })
    }

    fn persist(snapshot: &Snapshot) -> Result<(), AbacusError> {
        if let Some(parent) = snapshot.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AbacusError::Storage(format!(
                    "creating hold snapshot directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let tmp = snapshot.path.with_extension("tmp");
        let mut rows: Vec<&HoldRecord> = snapshot.records.values().collect();
        rows.sort_by(|a, b| a.hold_id.cmp(&b.hold_id));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(|error| AbacusError::Storage(format!("opening {}: {error}", tmp.display())))?;
        serde_json::to_writer_pretty(&mut file, &rows)
            .map_err(|error| AbacusError::Storage(format!("serializing holds: {error}")))?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|error| AbacusError::Storage(format!("persisting holds: {error}")))?;
        std::fs::rename(&tmp, &snapshot.path).map_err(|error| {
            AbacusError::Storage(format!(
                "installing hold snapshot {}: {error}",
                snapshot.path.display()
            ))
        })?;
        Ok(())
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> AbacusError {
    AbacusError::Storage("hold snapshot lock poisoned".to_string())
}

#[async_trait]
impl HoldStore for LocalHoldStore {
    async fn create(&self, record: HoldRecord) -> Result<(), AbacusError> {
        let mut snapshot = self.inner.lock().map_err(lock_error)?;
        match snapshot.records.get(&record.hold_id) {
            Some(existing) if existing == &record => return Ok(()),
            Some(_) => return Err(AbacusError::IdempotencyConflict),
            None => {}
        }
        snapshot
            .records
            .insert(record.hold_id.clone(), record.clone());
        if let Err(error) = Self::persist(&snapshot) {
            snapshot.records.remove(&record.hold_id);
            return Err(error);
        }
        Ok(())
    }

    async fn get(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        Ok(self
            .inner
            .lock()
            .map_err(lock_error)?
            .records
            .get(hold_id)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<HoldRecord>, AbacusError> {
        let snapshot = self.inner.lock().map_err(lock_error)?;
        let mut rows: Vec<HoldRecord> = snapshot.records.values().cloned().collect();
        rows.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.hold_id.cmp(&b.hold_id))
        });
        Ok(rows)
    }

    async fn claim_expired(
        &self,
        worker_id: &str,
        now_ms: i64,
        ttl_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<HoldRecord>, AbacusError> {
        let mut snapshot = self.inner.lock().map_err(lock_error)?;
        let mut rows: Vec<HoldRecord> = snapshot
            .records
            .values()
            .filter(|record| record.is_expired(now_ms, ttl_ms))
            .filter(|record| {
                snapshot
                    .leases
                    .get(&record.hold_id)
                    .is_none_or(|(_, until)| *until <= now_ms)
            })
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.hold_id.cmp(&b.hold_id))
        });
        rows.truncate(limit);
        for record in &rows {
            snapshot.leases.insert(
                record.hold_id.clone(),
                (worker_id.to_string(), lease_until_ms),
            );
        }
        Ok(rows)
    }

    async fn complete_claim(
        &self,
        hold_id: &str,
        worker_id: &str,
    ) -> Result<Option<HoldRecord>, AbacusError> {
        let mut snapshot = self.inner.lock().map_err(lock_error)?;
        if !matches!(snapshot.leases.get(hold_id), Some((owner, _)) if owner == worker_id) {
            return Ok(None);
        }
        let record = snapshot.records.remove(hold_id);
        snapshot.leases.remove(hold_id);
        if let Err(error) = Self::persist(&snapshot) {
            if let Some(record) = record.clone() {
                snapshot.records.insert(hold_id.to_string(), record);
            }
            return Err(error);
        }
        Ok(record)
    }

    async fn remove(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        let mut snapshot = self.inner.lock().map_err(lock_error)?;
        let Some(record) = snapshot.records.remove(hold_id) else {
            return Ok(None);
        };
        snapshot.leases.remove(hold_id);
        if let Err(error) = Self::persist(&snapshot) {
            snapshot.records.insert(hold_id.to_string(), record);
            return Err(error);
        }
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abacus_core::hold_store::HoldReceiptContext;
    use abacus_core::ids::{AgentId, BudgetId, RunId};

    fn record() -> HoldRecord {
        HoldRecord {
            hold_id: "hold_restart".into(),
            idem_key: "request_restart".into(),
            reserved_micros: 42,
            created_at_ms: 100,
            deadline_ms: None,
            receipt_context: Some(HoldReceiptContext {
                agent_id: AgentId::generate(),
                run_id: RunId::generate(),
                budget_id: BudgetId::generate(),
                payer_account: "acct_restart".into(),
                request_id: "request_restart".into(),
            }),
        }
    }

    #[tokio::test]
    async fn survives_reopen_and_persists_remove() {
        let path = std::env::temp_dir().join(format!(
            "abacus-holds-{}-{}.json",
            std::process::id(),
            ulid::Ulid::new()
        ));
        let expected = record();
        LocalHoldStore::open(&path)
            .unwrap()
            .create(expected.clone())
            .await
            .unwrap();
        let reopened = LocalHoldStore::open(&path).unwrap();
        assert_eq!(reopened.get("hold_restart").await.unwrap(), Some(expected));
        reopened.remove("hold_restart").await.unwrap();
        assert!(LocalHoldStore::open(&path)
            .unwrap()
            .list()
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
