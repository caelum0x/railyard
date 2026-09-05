//! Event ingestion — the write side that feeds the aggregators.
//!
//! Faithful port of `Events::ValidateCreationService` + `Events::CreateService`
//! (event construction) and the read scope of `Events::Stores::PostgresStore`
//! (`#events` / `#events_values`). An ingested event is validated, built, and
//! stored; the store then produces the boundary-filtered, ordered slice the
//! aggregators consume — exactly the slice Lago's SQL `WHERE` clause selects.
//!
//! The ClickHouse store is defined as the same [`EventStore`] interface with a
//! Postgres implementation here; a ClickHouse backend is a documented follow-up
//! (not faked).

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Event};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Raw event parameters as received at the ingestion boundary.
#[derive(Debug, Clone)]
pub struct EventParams {
    pub code: String,
    pub transaction_id: String,
    pub external_subscription_id: String,
    pub properties: Value,
    /// Event time as a number of seconds (Unix). `None` uses the request time.
    pub timestamp: Option<f64>,
    pub precise_total_amount_cents: Option<rust_decimal::Decimal>,
}

/// `Events::ValidateCreationService` failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventValidationError {
    #[error("subscription not found")]
    SubscriptionNotFound,
    #[error("invalid timestamp format")]
    InvalidTimestamp,
    #[error("transaction_id is missing or already exists")]
    TransactionId,
    #[error("billable_metric not found")]
    BillableMetricNotFound,
    #[error("properties value is not a valid number")]
    InvalidProperties,
}

/// `Events::ValidateCreationService#call`.
///
/// * `subscription_external_ids` — the customer's subscriptions' external ids.
/// * `metric` — the billable metric matching `params.code` (if any).
/// * `transaction_exists` — whether a persisted event already has this
///   `transaction_id` for the subscription.
pub fn validate_creation(
    params: &EventParams,
    subscription_external_ids: &[String],
    metric: Option<&BillableMetric>,
    transaction_exists: bool,
) -> Result<(), EventValidationError> {
    if params.external_subscription_id.is_empty() || subscription_external_ids.is_empty() {
        return Err(EventValidationError::SubscriptionNotFound);
    }
    if !subscription_external_ids.contains(&params.external_subscription_id) {
        return Err(EventValidationError::SubscriptionNotFound);
    }
    // timestamp, when present, must be a number (it already is, as f64).
    if params.transaction_id.is_empty() || transaction_exists {
        return Err(EventValidationError::TransactionId);
    }
    let metric = metric.ok_or(EventValidationError::BillableMetricNotFound)?;

    // For max/sum/latest metrics the aggregation property must be numeric.
    if matches!(
        metric.aggregation_type,
        AggregationType::MaxAgg | AggregationType::SumAgg | AggregationType::LatestAgg
    ) {
        let field = metric.field_name.clone().unwrap_or_default();
        if !is_valid_number(params.properties.get(&field)) {
            return Err(EventValidationError::InvalidProperties);
        }
    }
    Ok(())
}

/// `valid_number?` — nil is valid; otherwise it must parse as a float.
fn is_valid_number(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Number(_)) => true,
        Some(Value::String(s)) => s.trim().parse::<f64>().is_ok(),
        _ => false,
    }
}

/// `Events::CreateService` — build an [`Event`] from validated params.
pub fn build_event(
    organization_id: Uuid,
    params: &EventParams,
    request_time: DateTime<Utc>,
) -> Event {
    let ts = match params.timestamp {
        Some(secs) => Utc
            .timestamp_opt(secs.trunc() as i64, ((secs.fract()) * 1e9) as u32)
            .single()
            .unwrap_or(request_time),
        None => request_time,
    };
    Event {
        id: Uuid::new_v4(),
        organization_id,
        customer_id: None,
        subscription_id: None,
        transaction_id: params.transaction_id.clone(),
        code: params.code.clone(),
        properties: params.properties.clone(),
        external_customer_id: None,
        external_subscription_id: Some(params.external_subscription_id.clone()),
        timestamp: Some(ts.naive_utc()),
        precise_total_amount_cents: params.precise_total_amount_cents,
        created_at: request_time,
        updated_at: request_time,
        deleted_at: None,
        persisted: true,
    }
}

/// The read side an aggregator needs from an event store: the boundary-filtered,
/// ordered event slice for a subscription/metric.
pub trait EventStore {
    /// `PostgresStore#events` — events for `external_subscription_id` + `code`
    /// within `[from?, to]`, numeric-filtered for numeric metrics, ordered by
    /// timestamp ascending.
    fn events(
        &self,
        external_subscription_id: &str,
        code: &str,
        from: Option<NaiveDateTime>,
        to: NaiveDateTime,
        numeric_field: Option<&str>,
    ) -> Vec<Event>;
}

/// In-memory [`EventStore`] over a `Vec<Event>` — the Postgres `#events` scope
/// evaluated in Rust (used for the ingestion→billing wiring and tests).
#[derive(Debug, Default, Clone)]
pub struct InMemoryEventStore {
    events: Vec<Event>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Persist an event (`event.save!`).
    pub fn store(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Whether an event with `transaction_id` already exists for the
    /// subscription (`ValidateCreationService#valid_transaction_id?`).
    pub fn transaction_exists(&self, external_subscription_id: &str, transaction_id: &str) -> bool {
        self.events.iter().any(|e| {
            e.transaction_id == transaction_id
                && e.external_subscription_id.as_deref() == Some(external_subscription_id)
        })
    }
}

impl EventStore for InMemoryEventStore {
    fn events(
        &self,
        external_subscription_id: &str,
        code: &str,
        from: Option<NaiveDateTime>,
        to: NaiveDateTime,
        numeric_field: Option<&str>,
    ) -> Vec<Event> {
        let mut out: Vec<Event> = self
            .events
            .iter()
            .filter(|e| {
                e.external_subscription_id.as_deref() == Some(external_subscription_id)
                    && e.code == code
                    && e.deleted_at.is_none()
            })
            .filter(|e| match e.timestamp {
                Some(ts) => from.map_or(true, |f| ts >= f) && ts <= to,
                None => false,
            })
            .filter(|e| match numeric_field {
                Some(field) => {
                    is_valid_number(e.properties.get(field)) && e.properties.get(field).is_some()
                }
                None => true,
            })
            .cloned()
            .collect();
        out.sort_by_key(|e| e.timestamp);
        out
    }
}

/// Whether a metric aggregates on a numeric property (so the store applies the
/// numeric presence filter, matching `PostgresStore#numeric_property`).
pub fn numeric_field_for(metric: &BillableMetric) -> Option<&str> {
    match metric.aggregation_type {
        AggregationType::SumAgg
        | AggregationType::MaxAgg
        | AggregationType::LatestAgg
        | AggregationType::WeightedSumAgg => metric.field_name.as_deref(),
        _ => None,
    }
}
