//! Port of `models/billable_metric_filters.go`.

use serde::{Deserialize, Serialize};

use crate::utils::json::NestedJson;
use crate::utils::string::StringArray;
use crate::utils::time::NullTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillableMetricFilter {
    pub id: String,
    pub organization_id: String,
    pub billable_metric_id: String,
    pub key: String,
    #[serde(default)]
    pub values: StringArray,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub deleted_at: NullTime,
}

impl NestedJson for BillableMetricFilter {
    const DOTTED_KEYS: &'static [&'static str] = &[];
}
