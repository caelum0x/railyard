//! Port of `models/charge_filter_values.go`.

use serde::{Deserialize, Serialize};

use crate::utils::json::NestedJson;
use crate::utils::string::StringArray;
use crate::utils::time::NullTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChargeFilterValue {
    pub id: String,
    pub organization_id: String,
    pub charge_filter_id: String,
    pub billable_metric_filter_id: String,
    #[serde(default)]
    pub values: StringArray,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub deleted_at: NullTime,
}

impl NestedJson for ChargeFilterValue {
    const DOTTED_KEYS: &'static [&'static str] = &[];
}
