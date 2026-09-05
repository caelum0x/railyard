//! Port of `models/charges.go`.

use serde::{Deserialize, Serialize};

use crate::utils::json::NestedJson;
use crate::utils::string::StringArray;
use crate::utils::time::NullTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Charge {
    pub id: String,
    pub organization_id: String,
    pub plan_id: String,
    pub billable_metric_id: String,
    #[serde(default)]
    pub pay_in_advance: bool,
    #[serde(default)]
    pub accepts_target_wallet: bool,
    // Go json tag is the literal "properties.pricing_group_keys".
    #[serde(rename = "properties.pricing_group_keys", default)]
    pub pricing_group_keys: StringArray,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub deleted_at: NullTime,
}

impl NestedJson for Charge {
    const DOTTED_KEYS: &'static [&'static str] = &["properties.pricing_group_keys"];
}
