use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{CostEventPublisher, CostEventStore, CostFeedError, Result};

pub trait CostFeedClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCostFeedClock;

impl CostFeedClock for SystemCostFeedClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPolicy {
    pub batch_size: usize,
    pub lease_ms: i64,
    pub base_retry_ms: i64,
    pub max_retry_ms: i64,
    pub max_attempts: u32,
}

impl Default for WorkerPolicy {
    fn default() -> Self {
        Self {
            batch_size: 100,
            lease_ms: 30_000,
            base_retry_ms: 1_000,
            max_retry_ms: 300_000,
            max_attempts: 12,
        }
    }
}

impl WorkerPolicy {
    fn validate(self) -> Result<Self> {
        if self.batch_size == 0
            || self.lease_ms <= 0
            || self.base_retry_ms <= 0
            || self.max_retry_ms < self.base_retry_ms
            || self.max_attempts == 0
        {
            return Err(CostFeedError::Store("invalid worker policy".into()));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub claimed: usize,
    pub published: usize,
    pub rescheduled: usize,
    pub dead_lettered: usize,
}

pub struct CostFeedWorker {
    worker_id: String,
    store: Arc<dyn CostEventStore>,
    publisher: Arc<dyn CostEventPublisher>,
    clock: Arc<dyn CostFeedClock>,
    policy: WorkerPolicy,
}

impl CostFeedWorker {
    pub fn new(
        worker_id: impl Into<String>,
        store: Arc<dyn CostEventStore>,
        publisher: Arc<dyn CostEventPublisher>,
        clock: Arc<dyn CostFeedClock>,
        policy: WorkerPolicy,
    ) -> Result<Self> {
        let worker_id = worker_id.into();
        if worker_id.trim().is_empty() {
            return Err(CostFeedError::Store("worker id is empty".into()));
        }
        Ok(Self {
            worker_id,
            store,
            publisher,
            clock,
            policy: policy.validate()?,
        })
    }

    pub async fn drain_once(&self) -> Result<DrainReport> {
        let now_ms = self.clock.now_ms();
        let events = self
            .store
            .claim(
                &self.worker_id,
                now_ms,
                self.policy.lease_ms,
                self.policy.batch_size,
            )
            .await?;
        let mut report = DrainReport {
            claimed: events.len(),
            ..DrainReport::default()
        };
        for pending in events {
            match self.publisher.publish(&pending.event).await {
                Ok(()) => {
                    self.store
                        .mark_published(
                            pending.event.event_id,
                            &self.worker_id,
                            self.clock.now_ms(),
                        )
                        .await?;
                    report.published += 1;
                }
                Err(error) if pending.attempts >= self.policy.max_attempts => {
                    self.store
                        .dead_letter(
                            pending.event.event_id,
                            &self.worker_id,
                            self.clock.now_ms(),
                            &error.to_string(),
                        )
                        .await?;
                    report.dead_lettered += 1;
                }
                Err(error) => {
                    let delay = retry_delay(self.policy, pending.attempts)?;
                    let next_attempt = self
                        .clock
                        .now_ms()
                        .checked_add(delay)
                        .ok_or(CostFeedError::Overflow)?;
                    self.store
                        .reschedule(
                            pending.event.event_id,
                            &self.worker_id,
                            next_attempt,
                            &error.to_string(),
                        )
                        .await?;
                    report.rescheduled += 1;
                }
            }
        }
        Ok(report)
    }
}

fn retry_delay(policy: WorkerPolicy, attempts: u32) -> Result<i64> {
    let exponent = attempts.saturating_sub(1).min(30);
    let multiplier = 1_i64.checked_shl(exponent).ok_or(CostFeedError::Overflow)?;
    Ok(policy
        .base_retry_ms
        .saturating_mul(multiplier)
        .min(policy.max_retry_ms))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use relay_contract::{Microdollars, Usage};
    use uuid::Uuid;

    use super::*;
    use crate::{CostEvent, InMemoryCostEventStore, InferenceRoute};

    struct FixedClock;
    impl CostFeedClock for FixedClock {
        fn now_ms(&self) -> i64 {
            1_700_000_000_000
        }
    }

    struct FlakyPublisher(AtomicUsize);
    #[async_trait]
    impl CostEventPublisher for FlakyPublisher {
        async fn publish(&self, _event: &CostEvent) -> Result<()> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(CostFeedError::Publish("temporary".into()));
            }
            Ok(())
        }
    }

    fn event() -> CostEvent {
        CostEvent::captured(
            Uuid::now_v7(),
            "acct_owner",
            "openai/gpt-4o",
            InferenceRoute::OpenAiChatCompletions,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                provider_cost_micros: Some(Microdollars::new(50)),
            },
            Microdollars::new(100),
            Microdollars::new(50),
            "receipt",
            1_700_000_000_000,
        )
    }

    #[tokio::test]
    async fn failed_publish_is_rescheduled_without_losing_event() {
        let store = Arc::new(InMemoryCostEventStore::default());
        store.append(event()).await.unwrap();
        let worker = CostFeedWorker::new(
            "worker-1",
            store,
            Arc::new(FlakyPublisher(AtomicUsize::new(0))),
            Arc::new(FixedClock),
            WorkerPolicy::default(),
        )
        .unwrap();
        let report = worker.drain_once().await.unwrap();
        assert_eq!(report.rescheduled, 1);
        assert_eq!(report.published, 0);
    }

    #[tokio::test]
    async fn event_append_is_idempotent_and_conflicts_fail_closed() {
        let store = InMemoryCostEventStore::default();
        let event = event();
        store.append(event.clone()).await.unwrap();
        store.append(event.clone()).await.unwrap();
        let mut conflict = event;
        conflict.receipt_hash = "different".into();
        assert_eq!(store.append(conflict).await, Err(CostFeedError::Conflict));
    }

    #[tokio::test]
    async fn dead_letter_can_be_inspected_and_replayed_only_by_its_owner() {
        let store = InMemoryCostEventStore::default();
        let event = event();
        let event_id = event.event_id;
        store.append(event).await.unwrap();
        store
            .claim("worker", 1_700_000_000_000, 1_000, 1)
            .await
            .unwrap();
        store
            .dead_letter(event_id, "worker", 1_700_000_000_001, "consumer rejected")
            .await
            .unwrap();
        assert!(store.get("acct_other", event_id).await.unwrap().is_none());
        assert_eq!(
            store
                .replay_dead_letter("acct_other", event_id, 1_700_000_000_002)
                .await,
            Err(CostFeedError::NotFound)
        );
        let replayed = store
            .replay_dead_letter("acct_owner", event_id, 1_700_000_000_002)
            .await
            .unwrap();
        assert_eq!(replayed.attempts, 0);
        assert_eq!(replayed.next_attempt_at_ms, 1_700_000_000_002);
        assert!(replayed.dead_lettered_at_ms.is_none());
        assert_eq!(
            store
                .replay_dead_letter("acct_owner", event_id, 1_700_000_000_003)
                .await,
            Err(CostFeedError::NotReplayable)
        );
    }

    #[tokio::test]
    async fn owner_listing_is_filtered_bounded_and_cursor_paginated() {
        let store = InMemoryCostEventStore::default();
        let mut first = event();
        first.event_id = Uuid::from_u128(1);
        first.request_id = first.event_id;
        let mut second = event();
        second.event_id = Uuid::from_u128(2);
        second.request_id = second.event_id;
        let mut third = event();
        third.event_id = Uuid::from_u128(3);
        third.request_id = third.event_id;
        let mut other = event();
        other.event_id = Uuid::from_u128(4);
        other.request_id = other.event_id;
        other.owner_principal = "acct_other".into();
        for item in [first, second, third, other] {
            store.append(item).await.unwrap();
        }
        store
            .claim("publisher", 1_700_000_000_000, 1_000, 1)
            .await
            .unwrap();
        store
            .mark_published(Uuid::from_u128(1), "publisher", 1_700_000_000_001)
            .await
            .unwrap();

        let first_page = store
            .list(
                "acct_owner",
                crate::CostEventListQuery {
                    limit: 2,
                    ..crate::CostEventListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            first_page
                .items
                .iter()
                .map(|item| item.event.event_id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(3), Uuid::from_u128(2)]
        );
        assert_eq!(first_page.next_cursor, Some(Uuid::from_u128(2)));

        let published = store
            .list(
                "acct_owner",
                crate::CostEventListQuery {
                    state: Some(crate::CostEventState::Published),
                    before: first_page.next_cursor,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(published.items.len(), 1);
        assert_eq!(published.items[0].event.event_id, Uuid::from_u128(1));
        assert_eq!(published.next_cursor, None);

        assert!(matches!(
            store
                .list(
                    "acct_owner",
                    crate::CostEventListQuery {
                        limit: 0,
                        ..crate::CostEventListQuery::default()
                    }
                )
                .await,
            Err(CostFeedError::InvalidQuery(_))
        ));
    }
}
