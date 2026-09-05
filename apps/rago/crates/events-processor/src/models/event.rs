//! Port of `models/event.go`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::utils::result::Result;
use crate::utils::time::{self, CustomTime};

use super::billable_metrics::BillableMetric;
use super::flat_filters::FlatFilter;
use super::subscriptions::Subscription;

pub const HTTP_RUBY: &str = "http_ruby";
pub const TARGET_WALLET_CODE: &str = "target_wallet_code";

/// A raw event consumed from the `events_raw` topic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub external_subscription_id: String,
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub properties: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub precise_total_amount_cents: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default)]
    pub timestamp: serde_json::Value,
    #[serde(default)]
    pub source_metadata: Option<SourceMetadata>,
    #[serde(default)]
    pub ingested_at: CustomTime,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceMetadata {
    #[serde(rename = "api_post_processed", default)]
    pub api_post_process: bool,
    #[serde(default)]
    pub reprocess: bool,
}

impl Event {
    pub fn is_reprocess(&self) -> bool {
        self.source_metadata
            .as_ref()
            .map(|m| m.reprocess)
            .unwrap_or(false)
    }

    pub fn not_api_post_processed(&self) -> bool {
        if self.source != HTTP_RUBY {
            return true;
        }
        match &self.source_metadata {
            None => true,
            Some(m) => !m.api_post_process,
        }
    }

    /// `ToEnrichedEvent`.
    pub fn to_enriched_event(self: &Arc<Event>) -> Result<EnrichedEvent> {
        let mut er = EnrichedEvent {
            initial_event: Some(self.clone()),
            organization_id: self.organization_id.clone(),
            external_subscription_id: self.external_subscription_id.clone(),
            transaction_id: self.transaction_id.clone(),
            code: self.code.clone(),
            properties: self.properties.clone(),
            precise_total_amount_cents: self.precise_total_amount_cents.clone(),
            source: self.source.clone(),
            grouped_by: Default::default(),
            ..EnrichedEvent::empty()
        };

        let timestamp_result = time::to_float64_timestamp(&self.timestamp);
        if timestamp_result.is_failure() {
            return Result::failed(timestamp_result.into_error().unwrap()).non_retryable();
        }
        er.timestamp = *timestamp_result.value();
        er.timestamp_str = format!("{:.6}", er.timestamp);

        let time_result = time::to_time(&self.timestamp);
        if time_result.is_failure() {
            return Result::failed(time_result.into_error().unwrap()).non_retryable();
        }
        er.time = time_result.into_value();

        Result::success(er)
    }
}

/// An event enriched with billable-metric, subscription and charge context.
///
/// Fields tagged `json:"-"` in Go are runtime-only here (`#[serde(skip)]`), and
/// the struct only needs `Serialize` (it is produced to Kafka, never consumed).
#[derive(Debug, Clone, Serialize)]
pub struct EnrichedEvent {
    #[serde(skip)]
    pub initial_event: Option<Arc<Event>>,
    #[serde(skip)]
    pub billable_metric: Option<Arc<BillableMetric>>,
    #[serde(skip)]
    pub subscription: Option<Arc<Subscription>>,
    #[serde(skip)]
    pub flat_filter: Option<FlatFilter>,

    pub organization_id: String,
    pub external_subscription_id: String,
    pub subscription_id: String,
    pub plan_id: String,
    pub transaction_id: String,
    pub code: String,
    pub aggregation_type: String,
    pub properties: serde_json::Map<String, serde_json::Value>,
    pub precise_total_amount_cents: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    pub value: Option<String>,
    pub timestamp: f64,
    #[serde(skip)]
    pub timestamp_str: String,
    #[serde(skip)]
    pub time: DateTime<Utc>,
    pub charge_id: Option<String>,
    pub charge_updated_at: Option<DateTime<Utc>>,
    pub charge_filter_id: Option<String>,
    pub charge_filter_updated_at: Option<DateTime<Utc>>,
    pub grouped_by: std::collections::BTreeMap<String, String>,
    pub target_wallet_code: Option<String>,
}

impl EnrichedEvent {
    fn empty() -> Self {
        EnrichedEvent {
            initial_event: None,
            billable_metric: None,
            subscription: None,
            flat_filter: None,
            organization_id: String::new(),
            external_subscription_id: String::new(),
            subscription_id: String::new(),
            plan_id: String::new(),
            transaction_id: String::new(),
            code: String::new(),
            aggregation_type: String::new(),
            properties: serde_json::Map::new(),
            precise_total_amount_cents: String::new(),
            source: String::new(),
            value: None,
            timestamp: 0.0,
            timestamp_str: String::new(),
            time: DateTime::<Utc>::MIN_UTC,
            charge_id: None,
            charge_updated_at: None,
            charge_filter_id: None,
            charge_filter_updated_at: None,
            grouped_by: std::collections::BTreeMap::new(),
            target_wallet_code: None,
        }
    }
}

/// A failed event pushed to the dead-letter topic.
#[derive(Debug, Clone, Serialize)]
pub struct FailedEvent {
    pub event: Event,
    pub initial_error_message: String,
    pub error_message: String,
    pub error_code: String,
    pub failed_at: DateTime<Utc>,
}
