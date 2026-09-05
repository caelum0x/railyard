//! Port of `config/kafka/consumer.go`.
//!
//! Faithful-port decision: franz-go's manual partition-consumer bookkeeping
//! (OnPartitionsAssigned/Lost goroutines, per-partition channels) is replaced by
//! rdkafka's own client-side consumer-group management. The *essential*
//! semantics are preserved exactly:
//!   * manual commit (auto-commit disabled),
//!   * records are processed per partition,
//!   * only the largest safely-commitable prefix of each batch is committed
//!     (`find_max_commitable_record`), so unprocessed records are re-consumed,
//!   * graceful shutdown drains in-flight work before closing.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::BorrowedMessage;
use rdkafka::{Message, Offset, TopicPartitionList};
use tokio_util::sync::CancellationToken;

use crate::utils::error_tracker;
use crate::utils::RagoError;

use super::client::ServerConfig;
use super::record::Record;

const MAX_POLL_RECORDS: usize = 10_000;
const BATCH_DRAIN_WINDOW: Duration = Duration::from_millis(50);

/// Async record-processing callback: given a batch, returns the subset that was
/// processed (and is therefore safe to commit). Mirrors `ProcessRecords`.
pub type ProcessRecordsFn =
    Arc<dyn Fn(Vec<Record>) -> BoxFuture<'static, Vec<Record>> + Send + Sync>;

pub struct ConsumerGroupConfig {
    pub topic: String,
    pub consumer_group: String,
    pub process_records: ProcessRecordsFn,
}

pub struct ConsumerGroup {
    consumer: StreamConsumer,
    topic: String,
    process_records: ProcessRecordsFn,
}

impl ConsumerGroup {
    /// `NewConsumerGroup`.
    pub fn new(
        server_config: &ServerConfig,
        cfg: ConsumerGroupConfig,
    ) -> std::result::Result<ConsumerGroup, rdkafka::error::KafkaError> {
        let group_id = format!("{}_{}", cfg.consumer_group, cfg.topic);

        let consumer: StreamConsumer = server_config
            .base_client_config()
            .set("group.id", group_id)
            .set("enable.auto.commit", "false")
            .set("auto.offset.reset", "latest")
            .create()?;

        consumer.subscribe(&[&cfg.topic])?;

        Ok(ConsumerGroup {
            consumer,
            topic: cfg.topic,
            process_records: cfg.process_records,
        })
    }

    /// `(*ConsumerGroup).Start` — poll/process/commit until `token` is cancelled.
    pub async fn start(&self, token: CancellationToken) {
        loop {
            if token.is_cancelled() {
                break;
            }

            let batch = match self.poll_batch(&token).await {
                Some(b) if !b.is_empty() => b,
                Some(_) => continue,
                None => break, // cancelled
            };

            // Dispatch per partition, mirroring franz-go's per-partition fetch.
            let mut by_partition: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
            for rec in batch {
                by_partition.entry(rec.partition).or_default().push(rec);
            }

            for (partition, records) in by_partition {
                self.process_and_commit(partition, records).await;
            }
        }

        tracing::info!("Gracefully shutting down consumer group");
        // Dropping the StreamConsumer leaves the group and commits nothing extra.
    }

    /// Gather up to `MAX_POLL_RECORDS` messages, blocking for the first one
    /// (respecting shutdown), then draining what is already available.
    async fn poll_batch(&self, token: &CancellationToken) -> Option<Vec<Record>> {
        let mut batch: Vec<Record> = Vec::new();

        // Block for the first record, or return on shutdown.
        tokio::select! {
            _ = token.cancelled() => return None,
            res = self.consumer.recv() => match res {
                Ok(msg) => batch.push(to_record(&msg)),
                Err(err) => {
                    tracing::error!(error = %err, "Fetch error");
                    error_tracker::capture_error(&RagoError::Message(err.to_string()));
                    return Some(batch);
                }
            }
        }

        // Drain further records already buffered, up to the cap.
        while batch.len() < MAX_POLL_RECORDS {
            match tokio::time::timeout(BATCH_DRAIN_WINDOW, self.consumer.recv()).await {
                Ok(Ok(msg)) => batch.push(to_record(&msg)),
                Ok(Err(err)) => {
                    tracing::error!(error = %err, "Fetch error");
                    error_tracker::capture_error(&RagoError::Message(err.to_string()));
                    break;
                }
                Err(_) => break, // drain window elapsed
            }
        }

        Some(batch)
    }

    async fn process_and_commit(&self, partition: i32, records: Vec<Record>) {
        let span = crate::config::tracing::start_span("Consumer.Consume");

        let processed = (self.process_records)(records.clone()).await;

        let commit_offset = if processed.len() == records.len() {
            // Whole batch processed: commit through the last record.
            records.iter().map(|r| r.offset).max()
        } else {
            match find_max_commitable_record(&processed, &records) {
                Some(rec) => Some(rec.offset),
                None => {
                    tracing::warn!(
                        topic = %self.topic,
                        partition,
                        batch_size = records.len(),
                        processed = processed.len(),
                        "No commitable record in batch, skipping commit."
                    );
                    span.end();
                    return;
                }
            }
        };

        if let Some(offset) = commit_offset {
            if let Err(err) = self.commit(partition, offset) {
                tracing::error!(
                    error = %err,
                    topic = %self.topic,
                    partition,
                    "Error when committing offsets to kafka."
                );
                error_tracker::capture_error(&RagoError::Message(err.to_string()));
            }
        }

        span.end();
    }

    /// Commit `offset + 1` for a partition (CommitRecords semantics).
    fn commit(
        &self,
        partition: i32,
        offset: i64,
    ) -> std::result::Result<(), rdkafka::error::KafkaError> {
        let mut tpl = TopicPartitionList::new();
        tpl.add_partition_offset(&self.topic, partition, Offset::Offset(offset + 1))?;
        self.consumer
            .commit(&tpl, rdkafka::consumer::CommitMode::Sync)
    }
}

fn to_record(msg: &BorrowedMessage<'_>) -> Record {
    Record {
        key: msg.key().map(|k| k.to_vec()),
        value: msg.payload().map(|p| p.to_vec()).unwrap_or_default(),
        topic: msg.topic().to_string(),
        partition: msg.partition(),
        offset: msg.offset(),
    }
}

/// Port of `findMaxCommitableRecord`: the highest-offset processed record that
/// is strictly below the lowest-offset unprocessed record — the largest
/// commitable prefix of the batch. `None` when no such record exists.
pub fn find_max_commitable_record(processed: &[Record], records: &[Record]) -> Option<Record> {
    use std::collections::HashSet;

    let processed_set: HashSet<String> = processed.iter().map(|r| r.identity()).collect();

    // Minimum offset among unprocessed records.
    let mut min_unprocessed_offset: Option<i64> = None;
    for record in records {
        if !processed_set.contains(&record.identity()) {
            min_unprocessed_offset = Some(match min_unprocessed_offset {
                Some(cur) => cur.min(record.offset),
                None => record.offset,
            });
        }
    }
    let min_unprocessed_offset = min_unprocessed_offset.unwrap_or(i64::MAX);

    // Highest-offset processed record still below that boundary.
    let mut max_record: Option<&Record> = None;
    for record in processed {
        if record.offset < min_unprocessed_offset
            && max_record.map(|m| record.offset > m.offset).unwrap_or(true)
        {
            max_record = Some(record);
        }
    }

    max_record.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(offset: i64) -> Record {
        Record {
            key: Some(format!("k{offset}").into_bytes()),
            value: vec![],
            topic: "t".into(),
            partition: 0,
            offset,
        }
    }

    #[test]
    fn commits_full_prefix_when_all_processed() {
        let records = vec![rec(0), rec(1), rec(2)];
        let processed = records.clone();
        let got = find_max_commitable_record(&processed, &records).unwrap();
        assert_eq!(got.offset, 2);
    }

    #[test]
    fn stops_before_first_gap() {
        let records = vec![rec(0), rec(1), rec(2)];
        // offset 1 not processed -> commitable prefix is just offset 0
        let processed = vec![rec(0), rec(2)];
        let got = find_max_commitable_record(&processed, &records).unwrap();
        assert_eq!(got.offset, 0);
    }

    #[test]
    fn none_when_first_unprocessed() {
        let records = vec![rec(0), rec(1)];
        let processed = vec![rec(1)];
        assert!(find_max_commitable_record(&processed, &records).is_none());
    }
}
