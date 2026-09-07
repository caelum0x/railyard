use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use relay_contract::AitpChallenge;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AitpError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeState {
    Issued,
    Authorized { operation_hash: String },
    Consumed { receipt_hash: String },
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeRecord {
    pub request_id: Uuid,
    pub owner_principal: String,
    pub challenge: AitpChallenge,
    pub state: ChallengeState,
    pub issued_at_ms: i64,
    pub updated_at_ms: i64,
}

#[async_trait]
pub trait ChallengeStore: Send + Sync {
    async fn issue(&self, record: ChallengeRecord) -> Result<()>;
    async fn get(&self, request_id: Uuid) -> Result<Option<ChallengeRecord>>;
    async fn authorize(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord>;
    async fn consume(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        receipt_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord>;
    /// Delete bounded stale challenge rows. Active challenges whose expiry is
    /// newer than `cutoff_ms` are never eligible.
    async fn purge_before(&self, cutoff_ms: i64, limit: usize) -> Result<u64>;
}

#[derive(Default)]
pub struct InMemoryChallengeStore {
    records: RwLock<HashMap<Uuid, ChallengeRecord>>,
}

#[async_trait]
impl ChallengeStore for InMemoryChallengeStore {
    async fn issue(&self, record: ChallengeRecord) -> Result<()> {
        let mut records = self
            .records
            .write()
            .map_err(|_| AitpError::Store("challenge lock poisoned".into()))?;
        match records.get(&record.request_id) {
            Some(existing) if existing == &record => Ok(()),
            Some(_) => Err(AitpError::ConflictingReplay),
            None => {
                records.insert(record.request_id, record);
                Ok(())
            }
        }
    }

    async fn get(&self, request_id: Uuid) -> Result<Option<ChallengeRecord>> {
        self.records
            .read()
            .map_err(|_| AitpError::Store("challenge lock poisoned".into()))
            .map(|records| records.get(&request_id).cloned())
    }

    async fn authorize(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord> {
        let mut records = self
            .records
            .write()
            .map_err(|_| AitpError::Store("challenge lock poisoned".into()))?;
        let record = records.get_mut(&request_id).ok_or(AitpError::NotFound)?;
        ensure_owner(record, owner_principal)?;
        if i64::try_from(record.challenge.expiry).unwrap_or(i64::MAX) <= now_ms {
            record.state = ChallengeState::Expired;
            record.updated_at_ms = now_ms;
            return Err(AitpError::Expired);
        }
        match &record.state {
            ChallengeState::Issued => {
                record.state = ChallengeState::Authorized {
                    operation_hash: operation_hash.into(),
                };
                record.updated_at_ms = now_ms;
            }
            ChallengeState::Authorized {
                operation_hash: existing,
            } if existing == operation_hash => {}
            ChallengeState::Authorized { .. } => return Err(AitpError::ConflictingReplay),
            ChallengeState::Consumed { .. } => return Err(AitpError::AlreadyConsumed),
            ChallengeState::Expired => return Err(AitpError::Expired),
        }
        Ok(record.clone())
    }

    async fn consume(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        receipt_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord> {
        let mut records = self
            .records
            .write()
            .map_err(|_| AitpError::Store("challenge lock poisoned".into()))?;
        let record = records.get_mut(&request_id).ok_or(AitpError::NotFound)?;
        ensure_owner(record, owner_principal)?;
        match &record.state {
            ChallengeState::Authorized {
                operation_hash: existing,
            } if existing == operation_hash => {
                record.state = ChallengeState::Consumed {
                    receipt_hash: receipt_hash.into(),
                };
                record.updated_at_ms = now_ms;
            }
            ChallengeState::Consumed {
                receipt_hash: existing,
            } if existing == receipt_hash => {}
            ChallengeState::Issued | ChallengeState::Authorized { .. } => {
                return Err(AitpError::AuthorizationMismatch);
            }
            ChallengeState::Consumed { .. } => return Err(AitpError::ConflictingReplay),
            ChallengeState::Expired => return Err(AitpError::Expired),
        }
        Ok(record.clone())
    }

    async fn purge_before(&self, cutoff_ms: i64, limit: usize) -> Result<u64> {
        if limit == 0 {
            return Err(AitpError::InvalidRetentionLimit);
        }
        let mut records = self
            .records
            .write()
            .map_err(|_| AitpError::Store("challenge lock poisoned".into()))?;
        let doomed: Vec<_> = records
            .iter()
            .filter(|(_, record)| stale_before(record, cutoff_ms))
            .map(|(request_id, _)| *request_id)
            .take(limit)
            .collect();
        for request_id in &doomed {
            records.remove(request_id);
        }
        u64::try_from(doomed.len())
            .map_err(|_| AitpError::Store("purged challenge count overflow".into()))
    }
}

fn stale_before(record: &ChallengeRecord, cutoff_ms: i64) -> bool {
    match &record.state {
        ChallengeState::Consumed { .. } | ChallengeState::Expired => {
            record.updated_at_ms <= cutoff_ms
        }
        ChallengeState::Issued | ChallengeState::Authorized { .. } => {
            i64::try_from(record.challenge.expiry).is_ok_and(|expiry| expiry <= cutoff_ms)
        }
    }
}

fn ensure_owner(record: &ChallengeRecord, principal: &str) -> Result<()> {
    if record.owner_principal != principal {
        return Err(AitpError::Ownership);
    }
    Ok(())
}
