//! `Fees::ChargeService#init_fees` — every fee one usage charge produces for a
//! period: one per charge filter plus one for the events that match no filter,
//! and within each of those one per pricing group.
//!
//! Before this module the billing paths priced a charge over **all** of the
//! subscription's events with the charge's base properties, so a plan with
//! charge filters billed every filtered event at the default price, grouped
//! pricing was summed into one line, and prorated charges were billed in full.
//!
//! Faithful to Lago:
//! * bucket scoping is `ChargeFilters::MatchingAndIgnoredService` plus the
//!   store's `filters_scope` (an event belongs to a filter when its properties
//!   take one of the filter's values on every key, and it does not belong to a
//!   more specific child filter);
//! * the default bucket prices the events that match no filter with the
//!   charge's own properties;
//! * group keys are the bucket's `pricing_group_keys` (or `grouped_by`), plus
//!   `target_wallet_code` when the charge accepts a target wallet;
//! * prorated `sum_agg` / `unique_count_agg` charges use the prorated
//!   aggregations (`BillableMetrics::ProratedAggregations::*`), with the value
//!   persisted before the period billed for the whole window on recurring
//!   metrics, and the fee's units are the non-prorated units;
//! * on a charge with filters or groups, fees with no units, no amount and no
//!   events are not kept (`should_persist_fee?`); a charge with neither keeps
//!   its single fee, as before.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Charge, Event, Fee, Subscription};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use serde_json::Value;
use uuid::Uuid;

use crate::aggregation::{self, AggregationPeriod, AggregationResult, GroupedBy};
use crate::charge_models::{self, PricingStructure};
use crate::error::Result;
use crate::fees::build_charge_fee;

/// Property values per key: `{ "region": ["eu", "us"] }`.
pub type FilterValues = BTreeMap<String, Vec<String>>;

/// One stored charge filter, as billing needs it.
#[derive(Debug, Clone)]
pub struct FilterSpec {
    pub id: Uuid,
    /// Charge-model properties for the filtered events (they replace the
    /// charge's own properties for this bucket).
    pub properties: Value,
    pub values: FilterValues,
    pub created_at: DateTime<Utc>,
}

impl FilterSpec {
    /// From the stored row: `values` is the `{ key: [values] }` JSON object.
    /// Non-string values are compared through their JSON text, like
    /// `properties->>key` in the store.
    pub fn from_parts(id: Uuid, properties: Value, values: &Value, created_at: DateTime<Utc>) -> Self {
        let mut out = FilterValues::new();
        if let Some(obj) = values.as_object() {
            for (k, v) in obj {
                let vals: Vec<String> = v
                    .as_array()
                    .map(|a| a.iter().map(json_text).collect())
                    .unwrap_or_default();
                if !vals.is_empty() {
                    out.insert(k.clone(), vals);
                }
            }
        }
        Self {
            id,
            properties,
            values: out,
            created_at,
        }
    }

    /// `ChargeFilter#pricing_group_keys`.
    fn pricing_group_keys(&self) -> Option<Vec<String>> {
        group_keys_of(&self.properties)
    }
}

/// One fee-producing slice of a charge: a filter, or the default bucket
/// (`filter_id == None`) of events matching no filter.
#[derive(Debug, Clone)]
pub struct Bucket {
    pub filter_id: Option<Uuid>,
    pub properties: Value,
    pub group_keys: Vec<String>,
    pub matching: FilterValues,
    pub ignored: Vec<FilterValues>,
}

impl Bucket {
    /// `filters_scope`: every matching key holds one of its values, and the
    /// event matches none of the ignored filters (an ignored filter matches
    /// when every one of its non-empty keys holds one of its values; a missing
    /// property reads as the empty string, like `coalesce(.., '')`).
    pub fn contains(&self, event: &Event) -> bool {
        for (key, values) in &self.matching {
            match prop_text(event, key) {
                Some(v) if values.contains(&v) => {}
                _ => return false,
            }
        }
        for ignored in &self.ignored {
            let keys: Vec<(&String, &Vec<String>)> = ignored.iter().filter(|(_, v)| !v.is_empty()).collect();
            if keys.is_empty() {
                continue;
            }
            let hit = keys.iter().all(|(key, values)| {
                let v = prop_text(event, key).unwrap_or_default();
                values.contains(&v)
            });
            if hit {
                return false;
            }
        }
        true
    }
}

/// The buckets of a charge: none of its filters means one bucket with the
/// charge's properties; otherwise one per filter plus the default bucket.
pub fn buckets(charge: &Charge, filters: &[FilterSpec]) -> Vec<Bucket> {
    let with_wallet = |mut keys: Vec<String>| {
        if charge.accepts_target_wallet && !keys.iter().any(|k| k == "target_wallet_code") {
            keys.push("target_wallet_code".to_string());
        }
        keys
    };
    let charge_keys = group_keys_of(&charge.properties).unwrap_or_default();
    if filters.is_empty() {
        return vec![Bucket {
            filter_id: None,
            properties: charge.properties.clone(),
            group_keys: with_wallet(charge_keys),
            matching: FilterValues::new(),
            ignored: Vec::new(),
        }];
    }
    let mut out: Vec<Bucket> = filters
        .iter()
        .map(|f| {
            let (matching, ignored) = matching_and_ignored(f.id, &f.values, Some(f.created_at), filters);
            Bucket {
                filter_id: Some(f.id),
                properties: f.properties.clone(),
                group_keys: with_wallet(f.pricing_group_keys().unwrap_or_default()),
                matching,
                ignored,
            }
        })
        .collect();
    let (matching, ignored) = matching_and_ignored(Uuid::nil(), &FilterValues::new(), None, filters);
    out.push(Bucket {
        filter_id: None,
        properties: charge.properties.clone(),
        group_keys: with_wallet(charge_keys),
        matching,
        ignored,
    });
    out
}

/// `ChargeFilters::MatchingAndIgnoredService#call` for the filter `(id,
/// values, created_at)`; `created_at == None` is the unsaved default bucket.
fn matching_and_ignored(
    id: Uuid,
    values: &FilterValues,
    created_at: Option<DateTime<Utc>>,
    filters: &[FilterSpec],
) -> (FilterValues, Vec<FilterValues>) {
    let matching = values.clone();
    let children = filters.iter().filter(|f| f.id != id).filter(|f| {
        matching
            .iter()
            .all(|(key, vals)| vals.iter().any(|v| f.values.get(key).is_some_and(|c| c.contains(v))))
    });
    let same_keys = |child: &FilterValues| {
        let a: Vec<&String> = child.keys().collect();
        let b: Vec<&String> = matching.keys().collect();
        a == b
    };
    let mut ignored = Vec::new();
    for child in children {
        let mut res = child.values.clone();
        if same_keys(&res) {
            let identical = res.iter().all(|(k, v)| {
                let mut a = v.clone();
                let mut b = matching.get(k).cloned().unwrap_or_default();
                a.sort();
                b.sort();
                a == b
            });
            let subset = res
                .iter()
                .all(|(k, v)| v.iter().all(|x| matching.get(k).is_some_and(|m| m.contains(x))));
            if identical {
                // Only the oldest of identical filters counts the events.
                let older = match created_at {
                    None => true,
                    Some(mine) => (child.created_at, child.id) < (mine, id),
                };
                if !older {
                    continue;
                }
            } else if !subset {
                for (k, v) in res.iter_mut() {
                    let mine = matching.get(k).cloned().unwrap_or_default();
                    v.retain(|x| !mine.contains(x));
                }
            }
        }
        ignored.push(res);
    }
    (matching, ignored)
}

/// What one usage charge needs to be billed for a period.
#[derive(Debug, Clone, Copy)]
pub struct ChargeUsage<'a> {
    pub metric: &'a BillableMetric,
    pub charge: &'a Charge,
    /// The charge's live filters (empty when it has none).
    pub filters: &'a [FilterSpec],
    /// The period's events for the metric.
    pub events: &'a [Event],
    /// The metric's events before the period: read by prorated charges on
    /// recurring metrics (their value is carried into the period), ignored
    /// otherwise.
    pub persisted_events: &'a [Event],
    pub period: Option<&'a AggregationPeriod>,
}

/// Whether the charge bills through a prorated aggregation (Lago
/// `AggregationFactory`: `sum_agg` and `unique_count_agg` when prorated).
pub fn uses_prorated_aggregation(metric: &BillableMetric, charge: &Charge) -> bool {
    charge.prorated
        && matches!(
            metric.aggregation_type,
            AggregationType::SumAgg | AggregationType::UniqueCountAgg
        )
}

/// Whether billing this charge reads the events before the period.
pub fn needs_persisted_events(metric: &BillableMetric, charge: &Charge) -> bool {
    uses_prorated_aggregation(metric, charge) && metric.recurring
}

/// Every fee of one usage charge for the period, in bucket order (filters in
/// creation order, then the default bucket), groups in first-seen order, with
/// the unused lines of a filtered or grouped charge left out
/// ([`retain_used`]).
pub fn charge_fees(
    usage: &ChargeUsage<'_>,
    subscription: &Subscription,
    currency: Currency,
    invoice_id: Option<Uuid>,
) -> Result<Vec<Fee>> {
    let fees = charge_fees_all(usage, subscription, currency, invoice_id)?;
    Ok(retain_used(usage, fees, None))
}

/// [`charge_fees`] before unused lines are dropped: the invoice builder needs
/// the default bucket's fee as the parent of a minimum-commitment true-up even
/// when it has no usage.
pub fn charge_fees_all(
    usage: &ChargeUsage<'_>,
    subscription: &Subscription,
    currency: Currency,
    invoice_id: Option<Uuid>,
) -> Result<Vec<Fee>> {
    let charge = usage.charge;
    let mut fees = Vec::new();
    for bucket in &buckets(charge, usage.filters) {
        let current: Vec<Event> = usage.events.iter().filter(|e| bucket.contains(e)).cloned().collect();
        let persisted: Vec<Event> = if needs_persisted_events(usage.metric, charge) {
            usage
                .persisted_events
                .iter()
                .filter(|e| bucket.contains(e))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let ps = PricingStructure::from_charge(charge, currency).with_properties(bucket.properties.clone());
        for (grouped_by, agg) in aggregate_bucket(usage, bucket, &current, &persisted)? {
            let cmr = charge_models::compute(&ps, &agg)?;
            let full_units = agg.full_units_number;
            let mut fee = build_charge_fee(cmr, charge, subscription, invoice_id, currency);
            fee.charge_filter_id = bucket.filter_id;
            fee.grouped_by = grouped_json(&grouped_by);
            if charge.prorated {
                // In arrears a prorated fee shows the full (non-prorated) units.
                if let Some(full) = full_units.filter(|f| !f.is_sign_negative()) {
                    fee.units = full;
                    fee.total_aggregated_units = Some(full);
                }
            }
            fees.push(fee);
        }
    }
    Ok(fees)
}

/// `should_persist_fee?` for a filtered or grouped charge: keep the lines with
/// units, an amount or events, and `keep` (a true-up's parent). When none has
/// any, the default bucket's line stays so the charge still shows one (zero)
/// line. A charge with neither filters nor groups keeps its single fee.
pub fn retain_used(usage: &ChargeUsage<'_>, fees: Vec<Fee>, keep: Option<Uuid>) -> Vec<Fee> {
    let multi = !usage.filters.is_empty()
        || buckets(usage.charge, usage.filters)
            .iter()
            .any(|b| !b.group_keys.is_empty());
    if !multi {
        return fees;
    }
    let any_used = fees.iter().any(is_used);
    let default_idx = fees.iter().position(|f| f.charge_filter_id.is_none());
    fees.into_iter()
        .enumerate()
        .filter(|(i, f)| is_used(f) || Some(f.id) == keep || (!any_used && Some(*i) == default_idx))
        .map(|(_, f)| f)
        .collect()
}

fn is_used(f: &Fee) -> bool {
    !f.units.is_zero() || f.amount_cents != 0 || f.events_count.unwrap_or(0) != 0
}

/// Aggregate one bucket: one result when it has no group keys, else one per
/// group (`compute_grouped_by_aggregation`), each tagged with its group values.
fn aggregate_bucket(
    usage: &ChargeUsage<'_>,
    bucket: &Bucket,
    current: &[Event],
    persisted: &[Event],
) -> Result<Vec<(GroupedBy, AggregationResult)>> {
    if bucket.group_keys.is_empty() {
        return Ok(vec![(GroupedBy::new(), aggregate_one(usage, current, persisted)?)]);
    }
    let key_of = |e: &Event| -> Vec<Option<String>> { bucket.group_keys.iter().map(|k| prop_text(e, k)).collect() };
    let mut order: Vec<Vec<Option<String>>> = Vec::new();
    let mut cur: BTreeMap<Vec<Option<String>>, Vec<Event>> = BTreeMap::new();
    let mut per: BTreeMap<Vec<Option<String>>, Vec<Event>> = BTreeMap::new();
    for e in persisted.iter().chain(current.iter()) {
        let k = key_of(e);
        if !cur.contains_key(&k) && !per.contains_key(&k) {
            order.push(k.clone());
        }
        cur.entry(k.clone()).or_default();
        per.entry(k).or_default();
    }
    for e in current {
        cur.entry(key_of(e)).or_default().push(e.clone());
    }
    for e in persisted {
        per.entry(key_of(e)).or_default().push(e.clone());
    }
    let mut out = Vec::with_capacity(order.len());
    for k in order {
        let mut agg = aggregate_one(usage, &cur[&k], &per[&k])?;
        let grouped: GroupedBy = bucket.group_keys.iter().cloned().zip(k.into_iter()).collect();
        agg.grouped_by = grouped.clone();
        out.push((grouped, agg));
    }
    if out.is_empty() {
        // No event in the bucket: one empty result with null group values.
        let grouped: GroupedBy = bucket.group_keys.iter().map(|k| (k.clone(), None)).collect();
        let mut agg = aggregate_one(usage, &[], &[])?;
        agg.grouped_by = grouped.clone();
        out.push((grouped, agg));
    }
    Ok(out)
}

/// One aggregation over a bucket's (or group's) events.
fn aggregate_one(usage: &ChargeUsage<'_>, current: &[Event], persisted: &[Event]) -> Result<AggregationResult> {
    let metric = usage.metric;
    let charge = usage.charge;
    let free_units_per_events = crate::json::prop_i64(&charge.properties, "free_units_per_events");
    let free_units_per_total_aggregation =
        crate::json::prop_decimal(&charge.properties, "free_units_per_total_aggregation");
    let period = match usage.period {
        Some(p) if uses_prorated_aggregation(metric, charge) => p,
        _ => {
            return aggregation::aggregate_in_period(
                metric,
                current,
                free_units_per_events,
                free_units_per_total_aggregation,
                usage.period,
            )
        }
    };
    let duration = period.duration_days.max(1);
    let to_date: NaiveDate = period.to.date();
    match metric.aggregation_type {
        AggregationType::SumAgg => {
            let field = metric.field_name.clone().unwrap_or_default();
            let points: Vec<(NaiveDate, Decimal)> =
                current.iter().map(|e| (event_date(e), e.field_value(&field))).collect();
            let mut agg = aggregation::prorated_sum(&points, to_date, duration);
            // Persisted value: billed for the whole window (ratio 1 in Rago's
            // window-length periods, `persisted_duration / period_duration`).
            let persisted_sum: Decimal = persisted.iter().map(|e| e.field_value(&field)).sum();
            if !persisted_sum.is_zero() {
                let prorated_current: Decimal = agg.event_prorated_values.iter().copied().sum();
                agg.aggregation = aggregation::ceil_dp(prorated_current + persisted_sum, 5);
                agg.event_full_values.insert(0, persisted_sum);
                agg.event_prorated_values.insert(0, persisted_sum);
            }
            let full = agg.full_units_number.unwrap_or_default() + persisted_sum;
            agg.full_units_number = Some(full);
            agg.current_usage_units = Some(full);
            agg.count = (current.len() + persisted.len()) as i64;
            agg.precise_total_amount_cents = None;
            Ok(agg)
        }
        AggregationType::UniqueCountAgg => {
            let field = metric.field_name.clone().unwrap_or_default();
            let all: Vec<Event> = persisted.iter().chain(current.iter()).cloned().collect();
            let mut agg = aggregation::prorated_unique_count(&all, &field, period.from, period.to, duration);
            let full = aggregation::aggregate_in_period(
                metric,
                &all,
                free_units_per_events,
                free_units_per_total_aggregation,
                usage.period,
            )?;
            agg.full_units_number = Some(full.aggregation);
            agg.count = full.count;
            Ok(agg)
        }
        _ => aggregation::aggregate_in_period(
            metric,
            current,
            free_units_per_events,
            free_units_per_total_aggregation,
            usage.period,
        ),
    }
}

fn event_date(e: &Event) -> NaiveDate {
    e.timestamp.unwrap_or_else(|| e.created_at.naive_utc()).date()
}

fn grouped_json(g: &GroupedBy) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in g {
        m.insert(k.clone(), v.clone().map(Value::String).unwrap_or(Value::Null));
    }
    Value::Object(m)
}

fn group_keys_of(properties: &Value) -> Option<Vec<String>> {
    let v = properties
        .get("pricing_group_keys")
        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        .or_else(|| properties.get("grouped_by"))?;
    let keys: Vec<String> = v
        .as_array()?
        .iter()
        .filter_map(|k| k.as_str().map(str::to_string))
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// `properties->>key`: strings unquoted, other JSON values as their text.
fn prop_text(event: &Event, key: &str) -> Option<String> {
    event.properties.get(key).filter(|v| !v.is_null()).map(json_text)
}

fn json_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
