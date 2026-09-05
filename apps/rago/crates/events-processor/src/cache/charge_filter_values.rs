//! Port of `cache/charge_filter_values.go`.

use crate::models::ChargeFilterValue;
use crate::utils::result::Result;

use super::store::Cache;

pub const CHARGE_FILTER_VALUE_PREFIX: &str = "cfv";
pub const CHARGE_FILTER_VALUE_MODEL_NAME: &str = "charge_filter_values";
pub const CHARGE_FILTER_VALUE_TOPIC: &str = ".public.charge_filter_values";

impl Cache {
    pub fn build_charge_filter_value_key(
        &self,
        organization_id: &str,
        charge_filter_id: &str,
        billable_metric_filter_id: &str,
        id: &str,
    ) -> String {
        format!("{CHARGE_FILTER_VALUE_PREFIX}:{organization_id}:{charge_filter_id}:{billable_metric_filter_id}:{id}")
    }

    pub fn set_charge_filter_value(&self, cfv: &ChargeFilterValue) -> Result<bool> {
        let key = self.build_charge_filter_value_key(
            &cfv.organization_id,
            &cfv.charge_filter_id,
            &cfv.billable_metric_filter_id,
            &cfv.id,
        );
        self.set_json(&key, cfv)
    }

    pub fn get_charge_filter_value(
        &self,
        organization_id: &str,
        charge_filter_id: &str,
        billable_metric_filter_id: &str,
        id: &str,
    ) -> Result<ChargeFilterValue> {
        let key = self.build_charge_filter_value_key(
            organization_id,
            charge_filter_id,
            billable_metric_filter_id,
            id,
        );
        self.get_json::<ChargeFilterValue>(&key)
    }

    pub fn search_charge_filter_value(
        &self,
        organization_id: &str,
        charge_filter_id: &str,
    ) -> Result<Vec<ChargeFilterValue>> {
        let key = format!("{CHARGE_FILTER_VALUE_PREFIX}:{organization_id}:{charge_filter_id}:");
        self.search_json::<ChargeFilterValue>(&key)
    }

    pub fn delete_charge_filter_value(&self, cfv: &ChargeFilterValue) -> Result<bool> {
        let key = self.build_charge_filter_value_key(
            &cfv.organization_id,
            &cfv.charge_filter_id,
            &cfv.billable_metric_filter_id,
            &cfv.id,
        );
        self.delete(&key)
    }
}
