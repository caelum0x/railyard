use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CostEvent, CostFeedError, Result};

pub const DEFAULT_LIST_LIMIT: usize = 50;
pub const MAX_LIST_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostEventState {
    Pending,
    Published,
    DeadLettered,
}

impl CostEventState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Published => "published",
            Self::DeadLettered => "dead_lettered",
        }
    }

    fn matches(self, event: &PendingCostEvent) -> bool {
        match self {
            Self::Pending => event.published_at_ms.is_none() && event.dead_lettered_at_ms.is_none(),
            Self::Published => event.published_at_ms.is_some(),
            Self::DeadLettered => event.dead_lettered_at_ms.is_some(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEventListQuery {
    #[serde(default)]
    pub state: Option<CostEventState>,
    #[serde(default)]
    pub before: Option<Uuid>,
    #[serde(default = "default_list_limit")]
    pub limit: usize,
}

const fn default_list_limit() -> usize {
    DEFAULT_LIST_LIMIT
}

impl Default for CostEventListQuery {
    fn default() -> Self {
        Self {
            state: None,
            before: None,
            limit: DEFAULT_LIST_LIMIT,
        }
    }
}

impl CostEventListQuery {
    pub fn validate(self) -> Result<Self> {
        if self.limit == 0 || self.limit > MAX_LIST_LIMIT {
            return Err(CostFeedError::InvalidQuery(format!(
                "limit must be between 1 and {MAX_LIST_LIMIT}"
            )));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEventPage {
    pub items: Vec<PendingCostEvent>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCostEvent {
    pub event: CostEvent,
    pub attempts: u32,
    pub next_attempt_at_ms: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub published_at_ms: Option<i64>,
    pub dead_lettered_at_ms: Option<i64>,
    pub last_error: Option<String>,
}

#[async_trait]
pub trait CostEventStore: Send + Sync {
    async fn append(&self, event: CostEvent) -> Result<()>;
    async fn get(&self, owner_principal: &str, event_id: Uuid) -> Result<Option<PendingCostEvent>>;
    async fn list(&self, owner_principal: &str, query: CostEventListQuery)
    -> Result<CostEventPage>;
    async fn replay_dead_letter(
        &self,
        owner_principal: &str,
        event_id: Uuid,
        now_ms: i64,
    ) -> Result<PendingCostEvent>;
    /// Operator-scope listing across every principal. Stores that cannot
    /// aggregate across tenants keep the fail-closed default.
    async fn list_all(&self, _query: CostEventListQuery) -> Result<CostEventPage> {
        Err(CostFeedError::Store(
            "operator-wide cost-event listing is not supported by this store".into(),
        ))
    }
    /// Operator-scope lookup of one event regardless of its owner.
    async fn get_any(&self, _event_id: Uuid) -> Result<Option<PendingCostEvent>> {
        Err(CostFeedError::Store(
            "operator-wide cost-event lookup is not supported by this store".into(),
        ))
    }
    /// Operator-scope dead-letter replay regardless of the event owner.
    async fn replay_dead_letter_any(
        &self,
        _event_id: Uuid,
        _now_ms: i64,
    ) -> Result<PendingCostEvent> {
        Err(CostFeedError::Store(
            "operator-wide cost-event replay is not supported by this store".into(),
        ))
    }
    async fn claim(
        &self,
        worker_id: &str,
        now_ms: i64,
        lease_ms: i64,
        limit: usize,
    ) -> Result<Vec<PendingCostEvent>>;
    async fn mark_published(
        &self,
        event_id: Uuid,
        worker_id: &str,
        published_at_ms: i64,
    ) -> Result<()>;
    async fn reschedule(
        &self,
        event_id: Uuid,
        worker_id: &str,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<()>;
    async fn dead_letter(
        &self,
        event_id: Uuid,
        worker_id: &str,
        now_ms: i64,
        error: &str,
    ) -> Result<()>;
}

#[derive(Default)]
pub struct InMemoryCostEventStore {
    events: RwLock<HashMap<Uuid, PendingCostEvent>>,
}

#[async_trait]
impl CostEventStore for InMemoryCostEventStore {
    async fn append(&self, event: CostEvent) -> Result<()> {
        event
            .validate()
            .map_err(|error| CostFeedError::InvalidEvent(error.to_string()))?;
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        match events.get(&event.event_id) {
            Some(existing) if existing.event == event => Ok(()),
            Some(_) => Err(CostFeedError::Conflict),
            None => {
                events.insert(
                    event.event_id,
                    PendingCostEvent {
                        next_attempt_at_ms: event.settled_at_ms,
                        event,
                        attempts: 0,
                        lease_owner: None,
                        lease_expires_at_ms: None,
                        published_at_ms: None,
                        dead_lettered_at_ms: None,
                        last_error: None,
                    },
                );
                Ok(())
            }
        }
    }

    async fn get(&self, owner_principal: &str, event_id: Uuid) -> Result<Option<PendingCostEvent>> {
        self.events
            .read()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))
            .map(|events| {
                events
                    .get(&event_id)
                    .filter(|pending| pending.event.owner_principal == owner_principal)
                    .cloned()
            })
    }

    async fn list(
        &self,
        owner_principal: &str,
        query: CostEventListQuery,
    ) -> Result<CostEventPage> {
        let query = query.validate()?;
        let events = self
            .events
            .read()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let mut items = events
            .values()
            .filter(|pending| pending.event.owner_principal == owner_principal)
            .filter(|pending| query.state.is_none_or(|state| state.matches(pending)))
            .filter(|pending| {
                query
                    .before
                    .is_none_or(|before| pending.event.event_id < before)
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|pending| std::cmp::Reverse(pending.event.event_id));
        items.truncate(query.limit);
        let next_cursor = if items.len() == query.limit {
            items.last().map(|pending| pending.event.event_id)
        } else {
            None
        };
        Ok(CostEventPage { items, next_cursor })
    }

    async fn replay_dead_letter(
        &self,
        owner_principal: &str,
        event_id: Uuid,
        now_ms: i64,
    ) -> Result<PendingCostEvent> {
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let pending = events
            .get_mut(&event_id)
            .filter(|pending| pending.event.owner_principal == owner_principal)
            .ok_or(CostFeedError::NotFound)?;
        if pending.dead_lettered_at_ms.is_none() || pending.published_at_ms.is_some() {
            return Err(CostFeedError::NotReplayable);
        }
        pending.attempts = 0;
        pending.next_attempt_at_ms = now_ms;
        pending.lease_owner = None;
        pending.lease_expires_at_ms = None;
        pending.dead_lettered_at_ms = None;
        pending.last_error = None;
        Ok(pending.clone())
    }

    async fn claim(
        &self,
        worker_id: &str,
        now_ms: i64,
        lease_ms: i64,
        limit: usize,
    ) -> Result<Vec<PendingCostEvent>> {
        let lease_expires = now_ms
            .checked_add(lease_ms)
            .ok_or(CostFeedError::Overflow)?;
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let mut candidates: Vec<_> = events
            .values_mut()
            .filter(|pending| {
                pending.published_at_ms.is_none()
                    && pending.dead_lettered_at_ms.is_none()
                    && pending.next_attempt_at_ms <= now_ms
                    && pending
                        .lease_expires_at_ms
                        .is_none_or(|expiry| expiry <= now_ms)
            })
            .collect();
        candidates.sort_by_key(|pending| (pending.next_attempt_at_ms, pending.event.event_id));
        let claimed = candidates
            .into_iter()
            .take(limit)
            .map(|pending| {
                pending.lease_owner = Some(worker_id.into());
                pending.lease_expires_at_ms = Some(lease_expires);
                pending.attempts = pending.attempts.saturating_add(1);
                pending.clone()
            })
            .collect::<Vec<_>>();
        Ok(claimed)
    }

    async fn mark_published(
        &self,
        event_id: Uuid,
        worker_id: &str,
        published_at_ms: i64,
    ) -> Result<()> {
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let pending = events.get_mut(&event_id).ok_or(CostFeedError::NotFound)?;
        ensure_lease(pending, worker_id)?;
        pending.published_at_ms = Some(published_at_ms);
        pending.lease_owner = None;
        pending.lease_expires_at_ms = None;
        Ok(())
    }

    async fn reschedule(
        &self,
        event_id: Uuid,
        worker_id: &str,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<()> {
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let pending = events.get_mut(&event_id).ok_or(CostFeedError::NotFound)?;
        ensure_lease(pending, worker_id)?;
        pending.next_attempt_at_ms = next_attempt_at_ms;
        pending.last_error = Some(truncate_error(error));
        pending.lease_owner = None;
        pending.lease_expires_at_ms = None;
        Ok(())
    }

    async fn dead_letter(
        &self,
        event_id: Uuid,
        worker_id: &str,
        now_ms: i64,
        error: &str,
    ) -> Result<()> {
        let mut events = self
            .events
            .write()
            .map_err(|_| CostFeedError::Store("cost-event lock poisoned".into()))?;
        let pending = events.get_mut(&event_id).ok_or(CostFeedError::NotFound)?;
        ensure_lease(pending, worker_id)?;
        pending.dead_lettered_at_ms = Some(now_ms);
        pending.last_error = Some(truncate_error(error));
        pending.lease_owner = None;
        pending.lease_expires_at_ms = None;
        Ok(())
    }
}

fn ensure_lease(pending: &PendingCostEvent, worker_id: &str) -> Result<()> {
    if pending.lease_owner.as_deref() != Some(worker_id) {
        return Err(CostFeedError::LeaseOwnership);
    }
    Ok(())
}

fn truncate_error(error: &str) -> String {
    error.chars().take(1_024).collect()
}
