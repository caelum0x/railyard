//! Port of `models/billable_metrics.go`.

use serde::{Deserialize, Serialize};

use crate::utils::json::NestedJson;
use crate::utils::time::NullTime;

/// `AggregationType` — a Go `int` (iota) serialized as an integer in JSON.
/// The iota skips value 4, so we keep explicit discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AggregationType(pub i32);

pub const AGGREGATION_TYPE_COUNT: AggregationType = AggregationType(0);
pub const AGGREGATION_TYPE_SUM: AggregationType = AggregationType(1);
pub const AGGREGATION_TYPE_MAX: AggregationType = AggregationType(2);
pub const AGGREGATION_TYPE_UNIQUE_COUNT: AggregationType = AggregationType(3);
pub const AGGREGATION_TYPE_WEIGHTED_SUM: AggregationType = AggregationType(5);
pub const AGGREGATION_TYPE_LATEST: AggregationType = AggregationType(6);
pub const AGGREGATION_TYPE_CUSTOM: AggregationType = AggregationType(7);

impl AggregationType {
    /// Port of `(AggregationType).String()`.
    pub fn as_str(&self) -> &'static str {
        match self.0 {
            0 => "count",
            1 => "sum",
            2 => "max",
            3 => "unique_count",
            5 => "weighted_sum",
            6 => "latest",
            7 => "custom",
            _ => "",
        }
    }
}

impl std::fmt::Display for AggregationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillableMetric {
    pub id: String,
    pub organization_id: String,
    pub code: String,
    #[serde(default)]
    pub aggregation_type: AggregationType,
    #[serde(default)]
    pub recurring: bool,
    #[serde(default)]
    pub field_name: String,
    #[serde(default)]
    pub expression: String,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub deleted_at: NullTime,
}

impl NestedJson for BillableMetric {
    const DOTTED_KEYS: &'static [&'static str] = &[];
}
