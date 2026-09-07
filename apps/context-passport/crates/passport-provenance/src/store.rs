//! Provenance persistence port + in-memory implementation.

use std::sync::RwLock;

use async_trait::async_trait;

use passport_core::PassportError;

use crate::record::GenerationRecord;

/// Append-only provenance log. Postgres impl lives in `passport-store-postgres`.
#[async_trait]
pub trait ProvenanceLog: Send + Sync {
    async fn record(&self, record: &GenerationRecord) -> Result<(), PassportError>;
    async fn list(&self, passport_id: &str) -> Result<Vec<GenerationRecord>, PassportError>;

    /// Fetch a single record by its stable id, if any. Used to make the
    /// generate path idempotent: a retried settled generation reuses the
    /// already-recorded provenance instead of writing a second row.
    async fn get(&self, id: &str) -> Result<Option<GenerationRecord>, PassportError>;
}

/// In-memory provenance log for tests and embedded demos.
#[derive(Default)]
pub struct InMemoryProvenance {
    records: RwLock<Vec<GenerationRecord>>,
}

#[async_trait]
impl ProvenanceLog for InMemoryProvenance {
    async fn record(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        let mut records = self
            .records
            .write()
            .map_err(|_| PassportError::Storage("provenance lock poisoned".into()))?;
        if let Some(existing) = records.iter().find(|existing| existing.id == record.id) {
            return if existing == record {
                Ok(())
            } else {
                Err(PassportError::IdempotencyConflict)
            };
        }
        records.push(record.clone());
        Ok(())
    }

    async fn list(&self, passport_id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        let mut out: Vec<_> = self
            .records
            .read()
            .map_err(|_| PassportError::Storage("provenance lock poisoned".into()))?
            .iter()
            .filter(|r| r.passport_id == passport_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(out)
    }

    async fn get(&self, id: &str) -> Result<Option<GenerationRecord>, PassportError> {
        Ok(self
            .records
            .read()
            .map_err(|_| PassportError::Storage("provenance lock poisoned".into()))?
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }
}
