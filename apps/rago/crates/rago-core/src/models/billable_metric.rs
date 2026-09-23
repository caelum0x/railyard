use crate::enums::AggregationType;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `billable_metrics` — describes how raw events are aggregated into billable
/// units. (Lago `app/models/billable_metric.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillableMetric {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub aggregation_type: AggregationType,
    /// Event property summed/max'd/etc. by non-count aggregations.
    pub field_name: Option<String>,
    pub recurring: bool,
    pub expression: Option<String>,
    /// One of `round` / `ceil` / `floor` (Lago `ROUNDING_FUNCTIONS`).
    pub rounding_function: Option<String>,
    pub rounding_precision: Option<i32>,
    /// `custom_agg` metrics: expression evaluated per event (same grammar as
    /// `expression`); the per-event results are summed into the charge amount.
    #[serde(default)]
    pub custom_aggregator: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl BillableMetric {
    /// `BillableMetric#count_agg?` etc. — whether this metric counts events.
    pub fn is_count(&self) -> bool {
        matches!(self.aggregation_type, AggregationType::CountAgg)
    }
}
