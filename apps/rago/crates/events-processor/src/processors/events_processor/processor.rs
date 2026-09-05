//! Port of `processors/events_processor/processor.go` — the batch entry point
//! `ProcessEvents` and the per-event pipeline `processEvent`.
//!
//! Go fans the batch out over an `errgroup` (one goroutine per record) and, for
//! a single event, fans the produce calls out too. Here `ProcessEvents` runs the
//! per-record futures concurrently via `join_all`; the per-event produce calls
//! are awaited in order (behaviourally identical, no ordering dependency).

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};

use crate::config::kafka::Record;
use crate::config::tracing;
use crate::models::Event;
use crate::utils::error_tracker;
use crate::utils::result::{failed_from, Result};

use super::cache_service::CacheService;
use super::enrichment_service::EventEnrichmentService;
use super::event_producer_service::EventProducerService;
use super::subscription_refresh_service::SubscriptionRefreshService;

/// Events older than this are dead-lettered rather than retried.
const MAX_RETRY_AGE: StdDuration = StdDuration::from_secs(12 * 60 * 60);

pub struct EventProcessor {
    pub enrichment_service: EventEnrichmentService,
    pub producer_service: EventProducerService,
    pub refresh_service: SubscriptionRefreshService,
    pub cache_service: CacheService,
}

impl EventProcessor {
    pub fn new(
        enrichment_service: EventEnrichmentService,
        producer_service: EventProducerService,
        refresh_service: SubscriptionRefreshService,
        cache_service: CacheService,
    ) -> EventProcessor {
        EventProcessor {
            enrichment_service,
            producer_service,
            refresh_service,
            cache_service,
        }
    }

    /// `ProcessEvents`.
    pub async fn process_events(&self, records: Vec<Record>) -> Vec<Record> {
        let mut span = tracing::start_span("PostProcess.ProcessEvents");
        span.set_attribute_i64("records.length", records.len() as i64);

        let futures = records.into_iter().map(|record| {
            // Isolate each record: a panic while processing one event (e.g. a
            // deep `unwrap` on an unexpected enriched shape) must not abort the
            // whole batch and take down the consumer — that would turn a single
            // poison event into a denial of ingestion for the entire topic.
            let recovery = record.clone();
            run_record_guarded(recovery, self.process_one_record(record))
        });
        let results = futures::future::join_all(futures).await;

        span.end();
        results.into_iter().flatten().collect()
    }

    async fn process_one_record(&self, record: Record) -> Option<Record> {
        let sp = tracing::start_span("PostProcess.ProcessOneEvent");

        let event: Event = match serde_json::from_slice(&record.value) {
            Ok(e) => e,
            Err(e) => {
                ::tracing::error!(error = %e, "Error unmarshalling message");
                error_tracker::capture_error(&crate::utils::RagoError::from(e));
                sp.end();
                // Unmarshalable records fail forever, so commit them.
                return Some(record);
            }
        };
        let event = Arc::new(event);

        let result = self.process_event(&event).await;
        if result.is_failure() {
            ::tracing::error!(
                error_code = %result.error_code(),
                error = %result.error_msg(),
                "{}",
                result.error_message()
            );

            if result.is_capturable() {
                let extra = serde_json::to_value(&*event).ok();
                error_tracker::capture_error_result_with_extra(&result, "event", extra);
            }

            let age = Utc::now().signed_duration_since(event.ingested_at.time());
            let within_retry_window = age < Duration::from_std(MAX_RETRY_AGE).unwrap();

            if result.is_retryable() && within_retry_window {
                // Retryable: leave uncommitted for reprocessing.
                sp.end();
                return None;
            }

            self.producer_service
                .produce_to_dead_letter_queue((*event).clone(), &result)
                .await;
        }

        sp.end();
        Some(record)
    }

    /// `processEvent`.
    async fn process_event(&self, event: &Arc<Event>) -> Result<()> {
        let enriched_result = self.enrichment_service.enrich_event(event).await;
        if enriched_result.is_failure() {
            return failed_from(
                &enriched_result,
                enriched_result.error_code(),
                enriched_result.error_message(),
            );
        }
        let enriched_events = enriched_result.into_value();

        if event.is_reprocess() {
            // Reprocessing only re-produces enriched expanded events.
            for ev in &enriched_events {
                if ev.charge_id.is_some() {
                    self.producer_service
                        .produce_enriched_expanded_event(ev)
                        .await;
                }
            }
            return Result::success(());
        }

        self.producer_service
            .produce_enriched_event(&enriched_events[0])
            .await;

        for ev in &enriched_events {
            if ev.charge_id.is_some() {
                self.producer_service
                    .produce_enriched_expanded_event(ev)
                    .await;
            }
        }

        if enriched_events[0].subscription.is_some() && event.not_api_post_processed() {
            let pay_in_advance = enriched_events.iter().any(|ev| {
                ev.flat_filter
                    .as_ref()
                    .map(|f| f.pay_in_advance)
                    .unwrap_or(false)
            });

            if pay_in_advance {
                self.producer_service
                    .produce_charged_in_advance_event(&enriched_events[0])
                    .await;
            }

            let flag_result = self
                .refresh_service
                .flag_subscription_refresh(&enriched_events[0])
                .await;
            if flag_result.is_failure() {
                return failed_from(
                    &flag_result,
                    "flag_subscription_refresh",
                    "Error flagging subscription refresh",
                );
            }

            // Expire cache at charge and charge-filter level.
            self.cache_service.expire_cache(&enriched_events).await;
        }

        Result::success(())
    }
}

/// Runs one record's processing future under panic isolation.
///
/// A panic inside the per-record pipeline would otherwise propagate through
/// `join_all` and abort the consumer loop, so a single malformed/poison event
/// could deny ingestion for the whole topic. Here a panic is logged, captured
/// to Sentry, and the record is treated as processed (committed) — mirroring
/// the existing "unmarshalable records fail forever, so commit them" rule — so
/// the consumer advances past the bad offset instead of crash-looping on it.
///
/// The happy path is untouched: whatever `Option<Record>` the inner future
/// yields (retryable `None`, or committed `Some`) is returned verbatim.
async fn run_record_guarded<F>(record: Record, fut: F) -> Option<Record>
where
    F: std::future::Future<Output = Option<Record>>,
{
    use futures::future::FutureExt;

    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(out) => out,
        Err(panic) => {
            let detail = panic_detail(panic.as_ref());
            ::tracing::error!(
                panic = %detail,
                partition = record.partition,
                offset = record.offset,
                "Panic while processing record; skipping it to keep the consumer alive"
            );
            error_tracker::capture_error(&crate::utils::RagoError::Message(format!(
                "panic while processing record: {detail}"
            )));
            // Commit past the poison record so the consumer does not crash-loop.
            Some(record)
        }
    }
}

/// Best-effort extraction of a human-readable message from a panic payload.
fn panic_detail(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::FutureExt;

    fn rec(offset: i64) -> Record {
        Record {
            key: Some(format!("k{offset}").into_bytes()),
            value: vec![],
            topic: "raw_events".into(),
            partition: 0,
            offset,
        }
    }

    #[tokio::test]
    async fn panicking_record_is_skipped_and_committed() {
        let record = rec(7);
        // A record whose processing panics must be swallowed and returned as
        // "processed" (committed) so the consumer advances instead of crashing.
        let out = run_record_guarded(record.clone(), async {
            panic!("boom: poison event");
        })
        .await;
        assert_eq!(out, Some(record));
    }

    #[tokio::test]
    async fn healthy_record_results_pass_through_unchanged() {
        let record = rec(1);
        // Retryable outcome (None) is preserved verbatim.
        assert_eq!(
            run_record_guarded(record.clone(), async { None }).await,
            None
        );
        // Committed outcome (Some) is preserved verbatim.
        assert_eq!(
            run_record_guarded(record.clone(), async { Some(record.clone()) }).await,
            Some(record)
        );
    }

    #[tokio::test]
    async fn consumer_keeps_processing_after_a_poison_event() {
        // Simulate a batch: record 0 panics, record 1 is healthy. The guard must
        // let the batch complete with both records committed (poison skipped,
        // next event processed) rather than aborting the whole batch.
        let poison = rec(0);
        let healthy = rec(1);
        let results = futures::future::join_all(vec![
            run_record_guarded(poison.clone(), async { panic!("bad event") }).boxed_local(),
            run_record_guarded(healthy.clone(), async { Some(healthy.clone()) }).boxed_local(),
        ])
        .await;
        let committed: Vec<Record> = results.into_iter().flatten().collect();
        assert_eq!(committed, vec![poison, healthy]);
    }
}
