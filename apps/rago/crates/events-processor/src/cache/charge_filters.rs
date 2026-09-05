//! Port of `cache/charge_filters.go`.

use crate::models::ChargeFilter;
use crate::utils::result::Result;

use super::store::Cache;

pub const CHARGE_FILTER_PREFIX: &str = "cf";
pub const CHARGE_FILTER_MODEL_NAME: &str = "charge_filters";
pub const CHARGE_FILTER_TOPIC: &str = ".public.charge_filters";

impl Cache {
    pub fn build_charge_filter_key(
        &self,
        organization_id: &str,
        charge_id: &str,
        id: &str,
    ) -> String {
        format!("{CHARGE_FILTER_PREFIX}:{organization_id}:{charge_id}:{id}")
    }

    pub fn set_charge_filter(&self, cf: &ChargeFilter) -> Result<bool> {
        let key = self.build_charge_filter_key(&cf.organization_id, &cf.charge_id, &cf.id);
        self.set_json(&key, cf)
    }

    pub fn get_charge_filter(
        &self,
        organization_id: &str,
        charge_id: &str,
        id: &str,
    ) -> Result<ChargeFilter> {
        let key = self.build_charge_filter_key(organization_id, charge_id, id);
        self.get_json::<ChargeFilter>(&key)
    }

    pub fn search_charge_filter(
        &self,
        organization_id: &str,
        charge_id: &str,
    ) -> Result<Vec<ChargeFilter>> {
        let key = format!("{CHARGE_FILTER_PREFIX}:{organization_id}:{charge_id}:");
        self.search_json::<ChargeFilter>(&key)
    }

    pub fn delete_charge_filter(&self, cf: &ChargeFilter) -> Result<bool> {
        let key = self.build_charge_filter_key(&cf.organization_id, &cf.charge_id, &cf.id);
        self.delete(&key)
    }
}
