//! Durable settlement outbox: the crash-visible state machine that makes the
//! hold+capture+COGS trio atomic-or-compensated.
//!
//! Each request's settlement is one [`SettlementSaga`] with a durable [`SagaState`]
//! and per-step [`StepState`]s. The coordinator in [`crate::settle`] persists the
//! saga *before* touching Mint and after every step transition, so a crash at any
//! point leaves a recoverable record: a later attempt either resumes the forward
//! path (idempotent, via per-step keys) or drives compensation to completion.
//!
//! [`OutboxStore`] is the persistence port. [`MemoryOutbox`] is the in-process
//! implementation used by tests and single-node runs. A transactional Postgres
//! implementation is the remaining production step (see the crate README).

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::SettlementRefs;
use crate::error::LedgerlineError;

/// Lifecycle of one settlement step (hold, capture, or COGS transfer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not yet attempted, or attempted-but-not-confirmed.
    Pending,
    /// Forward effect confirmed committed on Mint.
    Committed,
    /// Forward effect confirmed reversed on Mint.
    Compensated,
}

/// Lifecycle of the whole settlement saga for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaState {
    /// A forward attempt is in progress (or crashed mid-flight).
    Pending,
    /// All three steps committed. Terminal success.
    Settled,
    /// A forward step failed; committed steps are being reversed.
    Compensating,
    /// All committed steps have been reversed. No net money moved this attempt.
    Compensated,
}

/// Durable record of one request's settlement progress.
///
/// `attempt` is bumped on each fresh forward run after a compensated attempt, so
/// the coordinator can mint per-attempt idempotency keys. Reusing a compensated
/// attempt's keys would let Mint treat a fresh forward call as a no-op replay of
/// an already-reversed operation, so a new attempt gets new keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementSaga {
    pub request_id: String,
    pub attempt: u32,
    pub state: SagaState,
    pub hold_state: StepState,
    pub hold_id: Option<String>,
    pub hold_seq: Option<i64>,
    pub capture_state: StepState,
    pub revenue_seq: Option<i64>,
    pub cogs_state: StepState,
    pub cogs_seq: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

impl SettlementSaga {
    /// A brand-new saga for a request, on its first forward attempt.
    pub fn new(request_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            request_id: request_id.into(),
            attempt: 1,
            state: SagaState::Pending,
            hold_state: StepState::Pending,
            hold_id: None,
            hold_seq: None,
            capture_state: StepState::Pending,
            revenue_seq: None,
            cogs_state: StepState::Pending,
            cogs_seq: None,
            updated_at: now,
        }
    }

    /// Begin a fresh forward attempt after a fully compensated one. Step state is
    /// reset so the coordinator re-runs the trio with new per-attempt keys.
    pub fn next_attempt(&self, now: DateTime<Utc>) -> Self {
        Self {
            request_id: self.request_id.clone(),
            attempt: self.attempt.saturating_add(1),
            state: SagaState::Pending,
            hold_state: StepState::Pending,
            hold_id: None,
            hold_seq: None,
            capture_state: StepState::Pending,
            revenue_seq: None,
            cogs_state: StepState::Pending,
            cogs_seq: None,
            updated_at: now,
        }
    }

    /// The settlement references for a fully settled saga.
    pub fn refs(&self) -> SettlementRefs {
        SettlementRefs {
            revenue_hold_id: self.hold_id.clone(),
            revenue_sequence: self.revenue_seq,
            cogs_sequence: self.cogs_seq,
        }
    }
}

/// Persistence port for the settlement outbox.
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Load the saga for a request, if one exists.
    async fn load(&self, request_id: &str) -> Result<Option<SettlementSaga>, LedgerlineError>;

    /// Create or replace the saga for a request. Idempotent by `request_id`.
    async fn upsert(&self, saga: &SettlementSaga) -> Result<(), LedgerlineError>;
}

/// In-process outbox for tests and single-node runs.
#[derive(Default)]
pub struct MemoryOutbox {
    sagas: RwLock<HashMap<String, SettlementSaga>>,
}

impl MemoryOutbox {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OutboxStore for MemoryOutbox {
    async fn load(&self, request_id: &str) -> Result<Option<SettlementSaga>, LedgerlineError> {
        Ok(self
            .sagas
            .read()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?
            .get(request_id)
            .cloned())
    }

    async fn upsert(&self, saga: &SettlementSaga) -> Result<(), LedgerlineError> {
        self.sagas
            .write()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?
            .insert(saga.request_id.clone(), saga.clone());
        Ok(())
    }
}
