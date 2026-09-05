//! Port of `cache/billable_metric_filters.go`.

use crate::models::BillableMetricFilter;
use crate::utils::result::Result;

use super::store::Cache;

pub const BILLABLE_METRIC_FILTER_PREFIX: &str = "bmf";
pub const BILLABLE_METRIC_FILTER_MODEL_NAME: &str = "billable_metric_filters";
pub const BILLABLE_METRIC_FILTER_TOPIC: &str = ".public.billable_metric_filters";

impl Cache {
    pub fn build_billable_metric_filter_key(
        &self,
        organization_id: &str,
        billable_metric_id: &str,
        id: &str,
    ) -> String {
        format!("{BILLABLE_METRIC_FILTER_PREFIX}:{organization_id}:{billable_metric_id}:{id}")
    }

    pub fn set_billable_metric_filter(&self, bmf: &BillableMetricFilter) -> Result<bool> {
        let key = self.build_billable_metric_filter_key(
            &bmf.organization_id,
            &bmf.billable_metric_id,
            &bmf.id,
        );
        self.set_json(&key, bmf)
    }

    pub fn get_billable_metric_filter(
        &self,
        organization_id: &str,
        billable_metric_id: &str,
        id: &str,
    ) -> Result<BillableMetricFilter> {
        let key = self.build_billable_metric_filter_key(organization_id, billable_metric_id, id);
        self.get_json::<BillableMetricFilter>(&key)
    }

    pub fn search_billable_metric_filters(
        &self,
        organization_id: &str,
        billable_metric_id: &str,
    ) -> Result<Vec<BillableMetricFilter>> {
        let prefix =
            format!("{BILLABLE_METRIC_FILTER_PREFIX}:{organization_id}:{billable_metric_id}:");
        self.search_json::<BillableMetricFilter>(&prefix)
    }

    pub fn delete_billable_metric_filter(&self, bmf: &BillableMetricFilter) -> Result<bool> {
        let key = self.build_billable_metric_filter_key(
            &bmf.organization_id,
            &bmf.billable_metric_id,
            &bmf.id,
        );
        self.delete(&key)
    }
}
