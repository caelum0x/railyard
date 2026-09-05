//! Port of `models/flat_filters.go` — the flattened charge/filter view and the
//! event-to-filter matching logic (`MatchingFilter`, `isBetterMatchThan`,
//! `IsMatchingEvent`). This must stay behaviourally identical to the Ruby
//! `ChargeFilters::EventMatchingService`, as the source comments stress.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::utils::result::Result;
use crate::utils::string::go_sprintf_v;

use super::event::EnrichedEvent;

pub type FlatFilterValues = HashMap<String, Vec<String>>;
pub type PricingGroupKeys = Vec<String>;

#[derive(Debug, Clone, Default)]
pub struct FlatFilter {
    pub organization_id: String,
    pub billable_metric_code: String,
    pub plan_id: String,
    pub charge_id: String,
    pub charge_updated_at: DateTime<Utc>,
    pub charge_filter_id: Option<String>,
    pub charge_filter_updated_at: Option<DateTime<Utc>>,
    pub filters: Option<FlatFilterValues>,
    pub pricing_group_keys: PricingGroupKeys,
    pub pay_in_advance: bool,
    pub accepts_target_wallet: bool,
}

impl FlatFilter {
    pub fn has_filters(&self) -> bool {
        self.filters
            .as_ref()
            .map(|f| !f.is_empty())
            .unwrap_or(false)
    }

    /// `(*FlatFilter).IsMatchingEvent`.
    pub fn is_matching_event(&self, event: &EnrichedEvent) -> Result<bool> {
        if !self.has_filters() {
            return Result::success(true);
        }

        let filters = self.filters.as_ref().unwrap();
        let mut matching = true;
        for (key, values) in filters {
            match event.properties.get(key) {
                None | Some(serde_json::Value::Null) => {
                    matching = false;
                    break;
                }
                Some(prop) => {
                    if !values.contains(&go_sprintf_v(prop)) {
                        matching = false;
                        break;
                    }
                }
            }
        }

        Result::success(matching)
    }

    /// `(*FlatFilter).ToDefaultFilter`.
    pub fn to_default_filter(&self) -> FlatFilter {
        FlatFilter {
            organization_id: self.organization_id.clone(),
            billable_metric_code: self.billable_metric_code.clone(),
            plan_id: self.plan_id.clone(),
            charge_id: self.charge_id.clone(),
            charge_updated_at: self.charge_updated_at,
            pay_in_advance: self.pay_in_advance,
            accepts_target_wallet: self.accepts_target_wallet,
            pricing_group_keys: self.pricing_group_keys.clone(),
            charge_filter_id: None,
            charge_filter_updated_at: None,
            filters: None,
        }
    }

    fn charge_filter_updated_at(&self) -> DateTime<Utc> {
        self.charge_filter_updated_at
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    }

    fn charge_filter_id_str(&self) -> &str {
        self.charge_filter_id.as_deref().unwrap_or("")
    }

    fn keys_len(&self) -> usize {
        self.filters.as_ref().map(|f| f.len()).unwrap_or(0)
    }

    /// `isBetterMatchThan`: more matched properties, or as many but older; then
    /// a deterministic tie-break on the charge-filter id.
    fn is_better_match_than(&self, other: &FlatFilter) -> bool {
        let (keys, other_keys) = (self.keys_len(), other.keys_len());
        if keys != other_keys {
            return keys > other_keys;
        }

        let (updated_at, other_updated_at) = (
            self.charge_filter_updated_at(),
            other.charge_filter_updated_at(),
        );
        if updated_at != other_updated_at {
            return updated_at < other_updated_at;
        }

        self.charge_filter_id_str() < other.charge_filter_id_str()
    }
}

/// `MatchingFilter(filters []FlatFilter, event *EnrichedEvent) *FlatFilter`.
///
/// Returns the best-matching filter, or the charge's default bucket
/// (`filters[0].ToDefaultFilter()`) when none matches.
pub fn matching_filter(filters: &[FlatFilter], event: &EnrichedEvent) -> FlatFilter {
    let mut best: Option<&FlatFilter> = None;

    for filter in filters {
        if !filter.has_filters() || !filter.is_matching_event(event).into_value() {
            continue;
        }
        match best {
            None => best = Some(filter),
            Some(current) if filter.is_better_match_than(current) => best = Some(filter),
            _ => {}
        }
    }

    match best {
        Some(f) => f.clone(),
        None => filters[0].to_default_filter(),
    }
}
