//! Port of `cache/billable_metrics.go`.

use crate::models::BillableMetric;
use crate::utils::result::Result;

use super::store::Cache;

pub const BILLABLE_METRIC_PREFIX: &str = "bm";
pub const BILLABLE_METRIC_MODEL_NAME: &str = "billable_metrics";
pub const BILLABLE_METRIC_TOPIC: &str = ".public.billable_metrics";

impl Cache {
    pub fn build_billable_metric_key(&self, organization_id: &str, code: &str) -> String {
        format!("{BILLABLE_METRIC_PREFIX}:{organization_id}:{code}")
    }

    pub fn set_billable_metric(&self, bm: &BillableMetric) -> Result<bool> {
        let key = self.build_billable_metric_key(&bm.organization_id, &bm.code);
        self.set_json(&key, bm)
    }

    pub fn get_billable_metric(&self, organization_id: &str, code: &str) -> Result<BillableMetric> {
        let key = self.build_billable_metric_key(organization_id, code);
        self.get_json::<BillableMetric>(&key)
    }

    pub fn delete_billable_metric(&self, bm: &BillableMetric) -> Result<bool> {
        let key = self.build_billable_metric_key(&bm.organization_id, &bm.code);
        self.delete(&key)
    }
}
