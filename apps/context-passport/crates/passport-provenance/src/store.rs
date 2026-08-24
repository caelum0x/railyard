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
}

/// In-memory provenance log for tests and embedded demos.
#[derive(Default)]
pub struct InMemoryProvenance {
    records: RwLock<Vec<GenerationRecord>>,
}

#[async_trait]
impl ProvenanceLog for InMemoryProvenance {
    async fn record(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        self.records
            .write()
            .map_err(|_| PassportError::Storage("provenance lock poisoned".into()))?
            .push(record.clone());
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
}
