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

/// `Events::CalculateExpressionService` — when the event's billable metric has
/// an `expression`, evaluate it on `{ code, timestamp, properties }` and write
/// the result to `properties[field_name]` (as text, like the Kafka path), so
/// every aggregation reads the computed value. A metric without an expression
/// (or without a field name to write to) leaves the event unchanged.
pub fn with_expression(
    metric: Option<&BillableMetric>,
    event: Event,
) -> Result<Event, crate::expression::ExprError> {
    let Some(metric) = metric else { return Ok(event) };
    let (Some(source), Some(field)) = (
        metric.expression.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        metric.field_name.as_deref().filter(|f| !f.is_empty()),
    ) else {
        return Ok(event);
    };
    let expr = crate::expression::Expression::parse(source)?;
    let timestamp = event.timestamp.map(|t| t.and_utc().timestamp()).unwrap_or_else(|| event.created_at.timestamp());
    let input = serde_json::json!({ "code": event.code, "timestamp": timestamp, "properties": event.properties });
    let value = expr.evaluate(&input)?.to_string();
    let mut properties = match &event.properties {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    properties.insert(field.to_string(), Value::String(value));
    Ok(Event { properties: Value::Object(properties), ..event })
}

/// Whether `expression` is valid for a billable metric (`validate_expression`).
pub fn validate_expression(expression: &str) -> bool {
    crate::expression::Expression::parse(expression.trim()).is_ok()
}

#[cfg(test)]
mod expression_tests {
    use super::*;
    use serde_json::json;

    fn metric(expression: Option<&str>, field: Option<&str>) -> BillableMetric {
        let now = Utc::now();
        BillableMetric {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            name: "m".into(),
            code: "compute".into(),
            description: None,
            aggregation_type: AggregationType::SumAgg,
            field_name: field.map(str::to_string),
            recurring: false,
            expression: expression.map(str::to_string),
            rounding_function: None,
            rounding_precision: None,
            custom_aggregator: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn event(props: Value) -> Event {
        let params = EventParams {
            code: "compute".into(),
            transaction_id: "t".into(),
            external_subscription_id: "s".into(),
            properties: props,
            timestamp: Some(1_700_000_000.0),
            precise_total_amount_cents: None,
        };
        build_event(Uuid::nil(), &params, Utc::now())
    }

    #[test]
    fn the_expression_result_is_written_to_the_field() {
        let m = metric(
            Some("round((event.properties.ended_at - event.properties.started_at) * event.properties.units / 3600, 2)"),
            Some("hours"),
        );
        let e = with_expression(Some(&m), event(json!({ "started_at": 0, "ended_at": 5400, "units": 2 }))).unwrap();
        assert_eq!(e.properties["hours"], json!("3"));
        assert_eq!(e.field_value("hours"), rust_decimal::Decimal::from(3));
        // The event's own properties are kept.
        assert_eq!(e.properties["units"], json!(2));
    }

    #[test]
    fn the_event_timestamp_is_readable() {
        let m = metric(Some("event.timestamp"), Some("at"));
        let e = with_expression(Some(&m), event(json!({}))).unwrap();
        assert_eq!(e.properties["at"], json!("1700000000"));
    }

    #[test]
    fn no_expression_or_no_field_leaves_the_event_alone() {
        let raw = json!({ "units": 2 });
        assert_eq!(with_expression(Some(&metric(None, Some("x"))), event(raw.clone())).unwrap().properties, raw);
        assert_eq!(with_expression(Some(&metric(Some("event.properties.units * 2"), None)), event(raw.clone())).unwrap().properties, raw);
        assert_eq!(with_expression(None, event(raw.clone())).unwrap().properties, raw);
    }

    #[test]
    fn a_missing_property_is_an_error() {
        let m = metric(Some("event.properties.units * event.properties.price"), Some("cost"));
        assert!(with_expression(Some(&m), event(json!({ "units": 2 }))).is_err());
    }

    #[test]
    fn expressions_are_validated() {
        assert!(validate_expression("ceil(event.properties.units / 10)"));
        assert!(!validate_expression("event.properties.units *"));
    }
}
