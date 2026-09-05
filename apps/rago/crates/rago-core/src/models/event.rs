use chrono::{DateTime, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// `events` — a raw usage event. (Lago `app/models/event.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Option<Uuid>,
    pub subscription_id: Option<Uuid>,
    pub transaction_id: String,
    pub code: String,
    pub properties: Value,
    pub external_customer_id: Option<String>,
    pub external_subscription_id: Option<String>,
    pub timestamp: Option<NaiveDateTime>,
    pub precise_total_amount_cents: Option<Decimal>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// In-memory flag: whether the event has been persisted (Lago `event.persisted`).
    #[serde(default = "default_true")]
    pub persisted: bool,
}

fn default_true() -> bool {
    true
}

impl Event {
    /// The numeric value of a metric's `field_name` on this event, defaulting
    /// to `0` when absent or non-numeric — mirrors
    /// `(event.properties || {})[billable_metric.field_name] || 0` followed by
    /// BigDecimal coercion.
    pub fn field_value(&self, field_name: &str) -> Decimal {
        match self.properties.get(field_name) {
            Some(Value::Number(n)) => n
                .as_f64()
                .and_then(Decimal::from_f64_retain)
                .unwrap_or(Decimal::ZERO),
            Some(Value::String(s)) => s.parse::<Decimal>().unwrap_or(Decimal::ZERO),
            _ => Decimal::ZERO,
        }
    }
}

/// `quantified_events` — cached recurring aggregation state. (Lago
/// `app/models/quantified_event.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantifiedEvent {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub billable_metric_id: Option<Uuid>,
    pub external_subscription_id: String,
    pub external_id: Option<String>,
    pub added_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub properties: Value,
    pub grouped_by: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
