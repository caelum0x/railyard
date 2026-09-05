//! Port of `models/charge_filters.go`.

use serde::{Deserialize, Serialize};

use crate::utils::json::NestedJson;
use crate::utils::string::StringArray;
use crate::utils::time::NullTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChargeFilter {
    pub id: String,
    pub organization_id: String,
    pub charge_id: String,
    #[serde(rename = "properties.pricing_group_keys", default)]
    pub pricing_group_keys: StringArray,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub deleted_at: NullTime,
}

impl NestedJson for ChargeFilter {
    const DOTTED_KEYS: &'static [&'static str] = &["properties.pricing_group_keys"];
}
