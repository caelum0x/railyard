//! Port of `cache/flat_filters.go` — `BuildFlatFilters`, which assembles the
//! flattened charge/filter view from the individually-cached entities (the
//! in-memory-cache equivalent of the `flat_filters` DB view).

use std::collections::HashMap;

use crate::models::{BillableMetricFilter, FlatFilter, FlatFilterValues};
use crate::utils::result::Result;

use super::store::Cache;

const ALL_FILTER_VALUES: &str = "__ALL_FILTER_VALUES__";

impl Cache {
    /// `BuildFlatFilters`.
    pub fn build_flat_filters(
        &self,
        organization_id: &str,
        billable_metric_code: &str,
        plan_id: &str,
    ) -> Result<Vec<FlatFilter>> {
        let mut flat_filters: Vec<FlatFilter> = Vec::new();

        let bm_result = self.get_billable_metric(organization_id, billable_metric_code);
        if bm_result.is_failure() {
            return Result::failed(bm_result.into_error().unwrap());
        }
        let billable_metric = bm_result.into_value();

        let bmf_result = self.search_billable_metric_filters(organization_id, &billable_metric.id);
        if bmf_result.is_failure() {
            return Result::failed(bmf_result.into_error().unwrap());
        }
        let billable_metric_filters = bmf_result.into_value();

        let bm_filter_map: HashMap<String, BillableMetricFilter> = billable_metric_filters
            .into_iter()
            .map(|bmf| (bmf.id.clone(), bmf))
            .collect();

        let ch_result = self.search_charge(organization_id, plan_id, &billable_metric.id);
        if ch_result.is_failure() {
            return Result::failed(ch_result.into_error().unwrap());
        }
        let charges = ch_result.into_value();

        for charge in &charges {
            let chf_result = self.search_charge_filter(organization_id, &charge.id);
            if chf_result.is_failure() {
                return Result::failed(chf_result.into_error().unwrap());
            }
            let charge_filters = chf_result.into_value();

            if charge_filters.is_empty() {
                flat_filters.push(FlatFilter {
                    organization_id: organization_id.to_string(),
                    billable_metric_code: billable_metric_code.to_string(),
                    plan_id: plan_id.to_string(),
                    charge_id: charge.id.clone(),
                    charge_updated_at: charge.updated_at.time(),
                    charge_filter_id: None,
                    charge_filter_updated_at: None,
                    filters: None,
                    pricing_group_keys: charge.pricing_group_keys.0.clone(),
                    pay_in_advance: charge.pay_in_advance,
                    accepts_target_wallet: charge.accepts_target_wallet,
                });
            } else {
                for charge_filter in &charge_filters {
                    let cfv_result =
                        self.search_charge_filter_value(organization_id, &charge_filter.id);
                    if cfv_result.is_failure() {
                        return Result::failed(cfv_result.into_error().unwrap());
                    }
                    let charge_filter_values = cfv_result.into_value();

                    let mut filters: FlatFilterValues = HashMap::new();
                    for cfv in &charge_filter_values {
                        let bm_filter = match bm_filter_map.get(&cfv.billable_metric_filter_id) {
                            Some(f) => f,
                            None => continue,
                        };

                        let key = bm_filter.key.clone();
                        let has_all = cfv.values.0.iter().any(|v| v == ALL_FILTER_VALUES);
                        let values = if has_all {
                            bm_filter.values.0.clone()
                        } else {
                            cfv.values.0.clone()
                        };
                        filters.insert(key, values);
                    }

                    flat_filters.push(FlatFilter {
                        organization_id: organization_id.to_string(),
                        billable_metric_code: billable_metric_code.to_string(),
                        plan_id: plan_id.to_string(),
                        charge_id: charge.id.clone(),
                        charge_updated_at: charge.updated_at.time(),
                        charge_filter_id: Some(charge_filter.id.clone()),
                        charge_filter_updated_at: Some(charge_filter.updated_at.time()),
                        filters: Some(filters),
                        pricing_group_keys: charge_filter.pricing_group_keys.0.clone(),
                        pay_in_advance: charge.pay_in_advance,
                        accepts_target_wallet: charge.accepts_target_wallet,
                    });
                }
            }
        }

        Result::success(flat_filters)
    }
}
