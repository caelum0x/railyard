//! Usage aggregation.
//!
//! Faithful port of `BillableMetrics::Aggregations::*`. Implements the
//! `count_agg`, `sum_agg`, `max_agg`, `latest_agg`, `unique_count_agg` and
//! `weighted_sum_agg` aggregators, plus `ApplyRoundingService`, over an
//! in-memory slice of [`Event`] (the Lago aggregators run the equivalent SQL
//! `SUM`/`COUNT`/`MAX`/window queries against an event store).
//!
//! [`AggregationResult`] mirrors `BillableMetrics::Aggregations::BaseService::Result`.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, NaiveDateTime};
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Event};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::error::{BillingError, Result};

/// Group key → value map for a grouped aggregation (`grouped_by`).
pub type GroupedBy = BTreeMap<String, Option<String>>;

/// `BillableMetrics::Aggregations::BaseService::Result`.
#[derive(Debug, Clone, Default)]
pub struct AggregationResult {
    pub aggregation: Decimal,
    pub current_usage_units: Option<Decimal>,
    pub full_units_number: Option<Decimal>,
    pub count: i64,
    pub total_aggregated_units: Option<Decimal>,
    pub running_total: Vec<Decimal>,
    pub per_event_values: Vec<Decimal>,
    /// Sum aggregation: precise amount cents for dynamic pricing.
    pub precise_total_amount_cents: Option<Decimal>,
    /// Custom aggregation resulting amount (`custom_aggregation[:amount]`).
    pub custom_amount: Option<Decimal>,
    /// Weighted-sum: net change in active units over the period.
    pub variation: Option<Decimal>,
    /// Grouped scenario: one sub-result per group.
    pub aggregations: Vec<AggregationResult>,
    /// The group keys/values this (sub-)result is scoped to.
    pub grouped_by: GroupedBy,
    /// Prorated aggregation: per-event full and prorated unit values (for the
    /// prorated graduated charge model).
    pub event_full_values: Vec<Decimal>,
    pub event_prorated_values: Vec<Decimal>,
    // --- pay-in-advance fields (BaseService::Result) ---
    /// Units contributed by the single in-advance event.
    pub pay_in_advance_aggregation: Option<Decimal>,
    /// Whether the triggering event is already persisted.
    pub pay_in_advance_event_persisted: bool,
    pub units_applied: Option<Decimal>,
    pub current_aggregation: Option<Decimal>,
    pub max_aggregation: Option<Decimal>,
}

/// Compute an [`AggregationResult`] for a metric over a set of events.
pub fn aggregate(
    metric: &BillableMetric,
    events: &[Event],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> Result<AggregationResult> {
    let mut result = match metric.aggregation_type {
        AggregationType::SumAgg => sum_aggregation(
            metric,
            events,
            free_units_per_events,
            free_units_per_total_aggregation,
        ),
        AggregationType::CountAgg => count_aggregation(
            events,
            free_units_per_events,
            free_units_per_total_aggregation,
        ),
        AggregationType::MaxAgg => max_aggregation(metric, events),
        AggregationType::LatestAgg => latest_aggregation(metric, events),
        AggregationType::UniqueCountAgg => unique_count_aggregation(
            metric,
            events,
            free_units_per_events,
            free_units_per_total_aggregation,
        ),
        AggregationType::WeightedSumAgg | AggregationType::CustomAgg => {
            return Err(BillingError::AggregationNotImplemented(
                metric.aggregation_type.as_str().to_string(),
            ))
        }
    };

    apply_rounding(metric, &mut result);
    Ok(result)
}

/// A charge filter / billing-period filter predicate: a property key that must
/// take one of `values` for an event to be in scope. Mirrors a single entry of
/// Lago's `matching_filters` (`Events::Stores::BaseStore#filters_scope` builds
/// `properties->>'key' IN (values)` conditions).
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub key: String,
    pub values: Vec<String>,
}

/// Whether an event matches all of `matching` and none of `ignored` — the
/// in-memory twin of Lago's charge-filter store scoping. `matching` narrows to
/// events whose property is in the allowed set; `ignored` excludes events that
/// match a more-specific competing filter.
pub fn event_matches_filters(
    event: &Event,
    matching: &[PropertyFilter],
    ignored: &[PropertyFilter],
) -> bool {
    let prop_str = |key: &str| -> Option<String> {
        event.properties.get(key).map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    };
    for f in matching {
        match prop_str(&f.key) {
            Some(v) if f.values.contains(&v) => {}
            _ => return false,
        }
    }
    // ignored: an event is excluded if it matches *every* key of an ignored
    // filter (i.e. it belongs to a competing, more specific filter).
    if !ignored.is_empty() {
        let matches_all = ignored.iter().all(|f| match prop_str(&f.key) {
            Some(v) => f.values.contains(&v),
            None => false,
        });
        if matches_all {
            return false;
        }
    }
    true
}

/// `BillableMetrics::Aggregations::BaseService` grouped path — split events by
/// the tuple of `group_keys` property values and aggregate each group, mirroring
/// the store's `grouped_*` queries (`GROUP BY g_0, g_1, …`).
///
/// Returns a parent [`AggregationResult`] whose `aggregations` holds one
/// sub-result per group (each carrying its `grouped_by` values), plus a
/// convenience top-level `aggregation` equal to the sum of the groups. A missing
/// group property yields a `None` group value (Lago's `NULL` group).
pub fn aggregate_grouped(
    metric: &BillableMetric,
    events: &[Event],
    group_keys: &[String],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> Result<AggregationResult> {
    // Preserve first-seen order of groups.
    let mut order: Vec<Vec<Option<String>>> = Vec::new();
    let mut buckets: std::collections::HashMap<Vec<Option<String>>, Vec<Event>> =
        std::collections::HashMap::new();

    for e in events {
        let key: Vec<Option<String>> = group_keys
            .iter()
            .map(|g| {
                e.properties.get(g).map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            })
            .collect();
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(e.clone());
    }

    let mut sub_results = Vec::with_capacity(order.len());
    let mut total = Decimal::ZERO;
    let mut total_count: i64 = 0;
    for key in &order {
        let group_events = &buckets[key];
        let mut sub = aggregate(
            metric,
            group_events,
            free_units_per_events,
            free_units_per_total_aggregation,
        )?;
        sub.grouped_by = group_keys
            .iter()
            .cloned()
            .zip(key.iter().cloned())
            .collect();
        total += sub.aggregation;
        total_count += sub.count;
        sub_results.push(sub);
    }

    Ok(AggregationResult {
        aggregation: total,
        current_usage_units: Some(total),
        count: total_count,
        aggregations: sub_results,
        ..Default::default()
    })
}

fn field_values(metric: &BillableMetric, events: &[Event]) -> Vec<Decimal> {
    let field = metric.field_name.clone().unwrap_or_default();
    events.iter().map(|e| e.field_value(&field)).collect()
}

/// `SumService#compute_aggregation`.
fn sum_aggregation(
    metric: &BillableMetric,
    events: &[Event],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> AggregationResult {
    let values = field_values(metric, events);
    let sum: Decimal = values.iter().copied().sum();
    let running_total = running_total(
        &values,
        free_units_per_events,
        free_units_per_total_aggregation,
    );
    AggregationResult {
        aggregation: sum,
        current_usage_units: Some(sum),
        count: events.len() as i64,
        running_total,
        per_event_values: values,
        ..Default::default()
    }
}

/// `CountService#compute_aggregation`.
fn count_aggregation(
    events: &[Event],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> AggregationResult {
    let count = events.len() as i64;
    let aggregation = Decimal::from(count);
    let running_total = if free_units_per_events == 0 && free_units_per_total_aggregation.is_zero()
    {
        Vec::new()
    } else {
        (1..=count).map(Decimal::from).collect()
    };
    AggregationResult {
        aggregation,
        current_usage_units: Some(aggregation),
        count,
        running_total,
        per_event_values: (0..count).map(|_| Decimal::ONE).collect(),
        ..Default::default()
    }
}

/// `MaxService#compute_aggregation`.
fn max_aggregation(metric: &BillableMetric, events: &[Event]) -> AggregationResult {
    let values = field_values(metric, events);
    let max = values.iter().copied().max().unwrap_or(Decimal::ZERO);
    AggregationResult {
        aggregation: max,
        current_usage_units: Some(max),
        count: events.len() as i64,
        per_event_values: values,
        ..Default::default()
    }
}

/// `LatestService#compute_aggregation` — last event's value (clamped ≥ 0).
fn latest_aggregation(metric: &BillableMetric, events: &[Event]) -> AggregationResult {
    let values = field_values(metric, events);
    let latest = values.last().copied().unwrap_or(Decimal::ZERO);
    let latest = if latest.is_sign_negative() {
        Decimal::ZERO
    } else {
        latest
    };
    AggregationResult {
        aggregation: latest,
        current_usage_units: Some(latest),
        count: events.len() as i64,
        per_event_values: values,
        ..Default::default()
    }
}

/// `UniqueCountService#compute_aggregation` / `Postgres::UniqueCountQuery` —
/// net count of active unique `field_name` values, honoring the `operation_type`
/// (`add` / `remove`) semantics, `ceil(5)`. `count` is set to the aggregation
/// (as in Lago).
///
/// Faithful to Lago's `operation_value_sql`: per property value, events are
/// ordered by timestamp and each contributes an adjusted value using the
/// previous operation for that property (defaulting to `remove` for the first,
/// so a leading `remove` is a no-op):
///   * `add`  after non-`add`    → `+1`  (redundant `add` after `add` → `0`)
///   * `remove` after non-`remove` → `-1` (redundant `remove` → `0`)
/// The per-property sums are then summed. Add-only streams reduce exactly to the
/// distinct-value count; a matched `add`+`remove` nets to `0`.
fn unique_count_aggregation(
    metric: &BillableMetric,
    events: &[Event],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> AggregationResult {
    let field = metric.field_name.clone().unwrap_or_default();
    // Preserve first-seen order of properties; events arrive timestamp-ordered.
    let mut order: Vec<String> = Vec::new();
    let mut nets: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut last_op: BTreeMap<String, String> = BTreeMap::new();
    for e in events {
        let Some(v) = e.properties.get(&field) else {
            continue;
        };
        let key = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let op = match e.properties.get("operation_type") {
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => "add".to_string(),
        };
        // LAG(operation_type, 1, 'remove') per property.
        let prev = last_op
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "remove".to_string());
        let adjusted = if op == prev {
            Decimal::ZERO
        } else if op == "add" {
            Decimal::ONE
        } else {
            Decimal::NEGATIVE_ONE
        };
        if !nets.contains_key(&key) {
            order.push(key.clone());
        }
        *nets.entry(key.clone()).or_insert(Decimal::ZERO) += adjusted;
        last_op.insert(key, op);
    }
    let net: Decimal = nets.values().copied().sum();
    let aggregation = ceil_dp(net, 5);
    let n = aggregation.to_i64().unwrap_or(0);
    let running_total = if free_units_per_events == 0 && free_units_per_total_aggregation.is_zero()
    {
        Vec::new()
    } else {
        (1..=n).map(Decimal::from).collect()
    };
    AggregationResult {
        aggregation,
        current_usage_units: Some(aggregation),
        count: n,
        running_total,
        per_event_values: (0..n.max(0)).map(|_| Decimal::ONE).collect(),
        ..Default::default()
    }
}

/// `WeightedSumService` / `WeightedSumQuery` — time-weighted sum of a running
/// total. Each `(timestamp, difference)` event contributes
/// `running_total * seconds_to_next_event / period_seconds`. The timeline is
/// bounded by `[from, initial_value]` at the start and `[to, 0]` at the end.
///
/// `charges_duration_days` is the billing period length in days (Lago's
/// `charges_duration.days.to_i` = `days * 86400` seconds).
///
/// Boundary fidelity (money-critical): Lago's `WeightedSumQuery` binds
/// `to_datetime: to_datetime.ceil` — the period end is rounded **up** to the
/// whole second, so a `to` of `23:59:59.999` becomes the next midnight and the
/// final span of the last active event covers the full period. It also measures
/// durations with `EXTRACT(EPOCH …)`, which keeps sub-second fractions. We match
/// both here: `to` is ceil'd to the whole second, and span lengths are measured
/// to microsecond precision (not truncated to whole seconds).
pub fn weighted_sum(
    events: &[(NaiveDateTime, Decimal)],
    from: NaiveDateTime,
    to: NaiveDateTime,
    charges_duration_days: i64,
    initial_value: Decimal,
) -> AggregationResult {
    let period_seconds = Decimal::from(charges_duration_days * 86_400);
    let to = ceil_to_second(to);

    // Build the timeline: initial, events (ordered), end-of-period sentinel.
    let mut timeline: Vec<(NaiveDateTime, Decimal)> = Vec::with_capacity(events.len() + 2);
    timeline.push((from, initial_value));
    let mut ordered: Vec<(NaiveDateTime, Decimal)> = events.to_vec();
    ordered.sort_by_key(|(ts, _)| *ts);
    timeline.extend(ordered.iter().copied());
    timeline.push((to, Decimal::ZERO));

    let mut running = Decimal::ZERO;
    let mut aggregation = Decimal::ZERO;
    let mut variation_with_initial = Decimal::ZERO;
    for i in 0..timeline.len() {
        let (ts, diff) = timeline[i];
        running += diff;
        variation_with_initial += diff;
        let next_ts = if i + 1 < timeline.len() {
            timeline[i + 1].0
        } else {
            to
        };
        let seconds = duration_seconds(next_ts - ts);
        if !seconds.is_zero() && !period_seconds.is_zero() {
            aggregation += running * seconds / period_seconds;
        }
    }

    let aggregation = ceil_dp(aggregation, 20);
    let variation = variation_with_initial - initial_value;
    // rows_count - 2 (drop the initial + end sentinels), clamped at 0.
    let count = ((timeline.len() as i64) - 2).max(0);

    AggregationResult {
        aggregation,
        count,
        variation: Some(variation),
        total_aggregated_units: Some(variation),
        ..Default::default()
    }
}

/// `BillableMetrics::ProratedAggregations::SumService` — prorate each event's
/// value by the fraction of the period it was active, for the prorated
/// graduated charge model. The per-event proration coefficient is
/// `(DATE(to) - DATE(event) + 1) / period_days` (Lago `duration_ratio_sql`).
///
/// Produces `event_full_values` (raw per-event values), `event_prorated_values`
/// (prorated), `aggregation = ceil5(sum prorated)` and
/// `full_units_number = sum(full)`.
pub fn prorated_sum(
    events: &[(NaiveDate, Decimal)],
    to_date: NaiveDate,
    period_days: i64,
) -> AggregationResult {
    let period = Decimal::from(period_days);
    let mut full_values = Vec::with_capacity(events.len());
    let mut prorated_values = Vec::with_capacity(events.len());
    for (date, value) in events {
        let days = Decimal::from((to_date - *date).num_days() + 1);
        let coeff = if period.is_zero() {
            Decimal::ZERO
        } else {
            days / period
        };
        full_values.push(*value);
        prorated_values.push(*value * coeff);
    }
    let prorated_sum: Decimal = prorated_values.iter().copied().sum();
    let full_sum: Decimal = full_values.iter().copied().sum();

    AggregationResult {
        aggregation: ceil_dp(prorated_sum, 5),
        current_usage_units: Some(full_sum),
        full_units_number: Some(full_sum),
        count: events.len() as i64,
        event_full_values: full_values,
        event_prorated_values: prorated_values,
        ..Default::default()
    }
}

/// `BillableMetrics::ProratedAggregations::UniqueCountService` /
/// `Events::Stores::Postgres::UniqueCountQuery#prorated_query` — the prorated
/// net-active unique count: each *add* of a `field_name` value contributes the
/// fraction of the period it stayed active, honoring `operation_type`
/// (`add`/`remove`) with per-day dedup.
///
/// Faithful to Lago's SQL, in three stages over the timestamp-ordered events:
///
/// 1. **same-day ignore** (`ignore_remove_events_sql`): a `remove` is ignored
///    when a later event *the same day* for the same property is an `add` (so an
///    add-after-remove-same-day pair nets to nothing).
/// 2. **operation dedup** (`operation_value_sql`, a `LAG` over the surviving
///    events per property): an operation that repeats the previous one for that
///    property contributes `0` and is dropped; the first ever op is compared
///    against a default of `remove`, so a leading `remove` is a no-op.
/// 3. **proration** (`period_ratio_sql`): each surviving `add` contributes
///    `(DATE(next_boundary) − DATE(this)) / charges_duration`, where
///    `next_boundary` is the next surviving event for that property (or the
///    period end, `+1` day) and both dates are clamped to `from`. `remove`
///    events contribute `0`. The period sum is `ceil(5)`'d.
///
/// `from`/`to` are the period boundaries; `to` is Lago's `to_datetime`
/// (end-of-day). Dates are taken in UTC (Rago's documented timezone
/// simplification, matching [`crate::dates`]).
pub fn prorated_unique_count(
    events: &[rago_core::models::Event],
    field_name: &str,
    from: NaiveDateTime,
    to: NaiveDateTime,
    charges_duration_days: i64,
) -> AggregationResult {
    #[derive(Clone)]
    struct Row {
        ts: NaiveDateTime,
        property: String,
        op: String,
    }

    // events_data: (timestamp, property, operation_type), ordered by timestamp.
    let mut rows: Vec<Row> = events
        .iter()
        .filter_map(|e| {
            let v = e.properties.get(field_name)?;
            let property = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let op = match e.properties.get("operation_type") {
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => "add".to_string(),
            };
            let ts = e.timestamp?;
            Some(Row { ts, property, op })
        })
        .collect();
    rows.sort_by(|a, b| a.ts.cmp(&b.ts));

    // Stage 1: same-day ignore for `remove` events.
    let is_ignored: Vec<bool> = rows
        .iter()
        .enumerate()
        .map(|(i, e)| {
            if e.op == "add" {
                return false;
            }
            rows.iter().enumerate().any(|(j, next)| {
                j != i
                    && next.property == e.property
                    && next.ts.date() == e.ts.date()
                    && next.op != e.op
                    && next.ts > e.ts
            })
        })
        .collect();
    let surviving: Vec<Row> = rows
        .iter()
        .zip(is_ignored.iter())
        .filter(|(_, ig)| !**ig)
        .map(|(r, _)| r.clone())
        .collect();

    // Stage 2: operation dedup via LAG(op, default 'remove') per property.
    let mut last_op: BTreeMap<String, String> = BTreeMap::new();
    let mut kept: Vec<Row> = Vec::new();
    for r in &surviving {
        let prev = last_op
            .get(&r.property)
            .cloned()
            .unwrap_or_else(|| "remove".to_string());
        let adjusted_is_zero = r.op == prev;
        last_op.insert(r.property.clone(), r.op.clone());
        if !adjusted_is_zero {
            kept.push(r.clone());
        }
    }
    // GROUP BY property, operation_type, timestamp — dedup identical triples.
    kept.dedup_by(|a, b| a.ts == b.ts && a.property == b.property && a.op == b.op);

    // Stage 3: per-`add` proration via LEAD per property.
    let duration = Decimal::from(charges_duration_days.max(1));
    let from_date = from.date();
    let mut sum = Decimal::ZERO;
    for (i, r) in kept.iter().enumerate() {
        if r.op != "add" {
            continue;
        }
        // LEAD(timestamp, 1, to_datetime) OVER (PARTITION BY property ORDER BY timestamp)
        let lead = kept[i + 1..]
            .iter()
            .find(|n| n.property == r.property)
            .map(|n| n.ts)
            .unwrap_or(to);
        // next_ref = (LEAD < from) ? from : LEAD + 1 day  (then DATE)
        let next_ref_date = if lead < from {
            from_date
        } else {
            (lead + Duration::days(1)).date()
        };
        // this_ref = (ts < from) ? from : ts  (then DATE)
        let this_ref_date = if r.ts < from { from_date } else { r.ts.date() };
        let days = (next_ref_date - this_ref_date).num_days();
        sum += Decimal::from(days) / duration;
    }

    let aggregation = ceil_dp(sum, 5);
    let n = aggregation.to_i64().unwrap_or(0);
    AggregationResult {
        aggregation,
        current_usage_units: Some(aggregation),
        count: n.max(0),
        ..Default::default()
    }
}

/// `ApplyRoundingService` — apply the metric's `rounding_function` at its
/// `rounding_precision`, to `aggregation`/`full_units_number`/`current_usage_units`.
fn apply_rounding(metric: &BillableMetric, result: &mut AggregationResult) {
    let Some(func) = metric.rounding_function.as_deref() else {
        return;
    };
    let precision = metric.rounding_precision.unwrap_or(0).max(0) as u32;
    let round = |v: Decimal| match func {
        "ceil" => ceil_dp(v, precision),
        "floor" => floor_dp(v, precision),
        "round" => v.round_dp_with_strategy(precision, RoundingStrategy::MidpointAwayFromZero),
        _ => v,
    };
    result.aggregation = round(result.aggregation);
    if let Some(f) = result.full_units_number {
        result.full_units_number = Some(round(f));
    }
    if let Some(c) = result.current_usage_units {
        result.current_usage_units = Some(round(c));
    }
}

/// `BigDecimal#ceil(precision)` — round toward +∞ at `dp` decimals.
pub fn ceil_dp(v: Decimal, dp: u32) -> Decimal {
    let r = v.round_dp_with_strategy(dp, RoundingStrategy::ToZero);
    if r < v {
        r + Decimal::new(1, dp)
    } else {
        r
    }
}

/// `BigDecimal#floor(precision)` — round toward -∞ at `dp` decimals.
pub fn floor_dp(v: Decimal, dp: u32) -> Decimal {
    let r = v.round_dp_with_strategy(dp, RoundingStrategy::ToZero);
    if r > v {
        r - Decimal::new(1, dp)
    } else {
        r
    }
}

/// Round a timestamp **up** to the whole second (Ruby `Time#ceil`): if it has a
/// sub-second component, advance to the next whole second, else leave it. Used
/// by [`weighted_sum`] to match Lago's `to_datetime.ceil`.
fn ceil_to_second(ts: NaiveDateTime) -> NaiveDateTime {
    let subsec = ts.and_utc().timestamp_subsec_nanos();
    if subsec == 0 {
        ts
    } else {
        (ts - Duration::nanoseconds(subsec as i64)) + Duration::seconds(1)
    }
}

/// Duration as fractional seconds at microsecond precision (Postgres
/// `EXTRACT(EPOCH …)`), not the whole-second truncation of `num_seconds()`.
fn duration_seconds(d: Duration) -> Decimal {
    match d.num_microseconds() {
        Some(us) => Decimal::new(us, 6),
        None => Decimal::from(d.num_seconds()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use rago_core::models::Event;
    use rust_decimal_macros::dec;
    use serde_json::json;
    use uuid::Uuid;

    fn dt(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, ss)
            .unwrap()
    }
    fn eod(y: i32, m: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap()
    }
    fn ev(ts: NaiveDateTime, props: serde_json::Value) -> Event {
        let now = Utc::now();
        Event {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            customer_id: None,
            subscription_id: None,
            transaction_id: Uuid::new_v4().to_string(),
            code: "c".into(),
            properties: props,
            external_customer_id: None,
            external_subscription_id: None,
            timestamp: Some(ts),
            precise_total_amount_cents: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            persisted: true,
        }
    }

    // Period 2022-07-09 .. 2022-08-08 (charges_duration 31), from unique_count
    // prorated_aggregations spec.
    fn from() -> NaiveDateTime {
        dt(2022, 7, 9, 0, 0, 0)
    }
    fn to() -> NaiveDateTime {
        eod(2022, 8, 8)
    }
    fn puc(events: &[Event]) -> Decimal {
        prorated_unique_count(events, "unique_id", from(), to(), 31).aggregation
    }

    // ---- Lago spec: prorated_aggregations/unique_count_service_spec.rb ----

    #[test]
    fn prorated_unique_persisted_full_period() {
        let e = vec![ev(dt(2022, 6, 9, 0, 0, 0), json!({"unique_id": "a"}))];
        assert_eq!(puc(&e), dec!(1));
    }

    #[test]
    fn prorated_unique_persisted_plus_added_in_period() {
        let e = vec![
            ev(dt(2022, 6, 9, 0, 0, 0), json!({"unique_id": "a"})),
            ev(dt(2022, 7, 19, 0, 0, 0), json!({"unique_id": "b"})), // from + 10 days
        ];
        // (1 + 21/31).ceil(5)
        let expected = ceil_dp(dec!(1) + dec!(21) / dec!(31), 5);
        assert_eq!(puc(&e), expected);
    }

    #[test]
    fn prorated_unique_added_mid_period() {
        // Single add at from + 15 days (07-24) → 16/31.
        let e = vec![ev(dt(2022, 7, 24, 0, 0, 0), json!({"unique_id": "a"}))];
        assert_eq!(puc(&e), ceil_dp(dec!(16) / dec!(31), 5));
    }

    #[test]
    fn prorated_unique_added_first_day_full() {
        let e = vec![ev(dt(2022, 7, 9, 0, 0, 0), json!({"unique_id": "a"}))];
        assert_eq!(puc(&e), dec!(1));
    }

    #[test]
    fn prorated_unique_removed_mid_period() {
        // add persisted 06-09, remove at to-15 (07-24) → active 16 days.
        let e = vec![
            ev(dt(2022, 6, 9, 0, 0, 0), json!({"unique_id": "a"})),
            ev(
                dt(2022, 7, 24, 0, 0, 0),
                json!({"unique_id": "a", "operation_type": "remove"}),
            ),
        ];
        assert_eq!(puc(&e), ceil_dp(dec!(16) / dec!(31), 5));
    }

    #[test]
    fn prorated_unique_removed_last_day_full() {
        let e = vec![
            ev(dt(2022, 6, 9, 0, 0, 0), json!({"unique_id": "a"})),
            ev(
                eod(2022, 8, 8),
                json!({"unique_id": "a", "operation_type": "remove"}),
            ),
        ];
        assert_eq!(puc(&e), dec!(1));
    }

    #[test]
    fn prorated_unique_added_and_removed_in_period() {
        // add at from+1day (07-10), remove at to-1day (08-07) → 29 days.
        let e = vec![
            ev(dt(2022, 7, 10, 0, 0, 0), json!({"unique_id": "a"})),
            ev(
                dt(2022, 8, 7, 23, 59, 59),
                json!({"unique_id": "a", "operation_type": "remove"}),
            ),
        ];
        assert_eq!(puc(&e), ceil_dp(dec!(29) / dec!(31), 5));
    }

    #[test]
    fn prorated_unique_added_removed_same_day_multiple() {
        // add@01:00, remove@02:00, add@03:00, remove@04:00 (same property, same day)
        // → net 1-day duration = 1/31.
        let base = dt(2022, 7, 9, 1, 0, 0);
        let e = vec![
            ev(base, json!({"unique_id": "a", "operation_type": "add"})),
            ev(
                base + Duration::hours(1),
                json!({"unique_id": "a", "operation_type": "remove"}),
            ),
            ev(
                base + Duration::hours(2),
                json!({"unique_id": "a", "operation_type": "add"}),
            ),
            ev(
                base + Duration::hours(3),
                json!({"unique_id": "a", "operation_type": "remove"}),
            ),
        ];
        assert_eq!(puc(&e), ceil_dp(dec!(1) / dec!(31), 5));
    }

    // ---- grouped aggregation + charge filters ----

    #[test]
    fn grouped_sum_splits_by_property() {
        let m = BillableMetric {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            name: "m".into(),
            code: "c".into(),
            description: None,
            aggregation_type: AggregationType::SumAgg,
            field_name: Some("amount".into()),
            recurring: false,
            expression: None,
            rounding_function: None,
            rounding_precision: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        };
        let e = vec![
            ev(
                dt(2023, 5, 1, 0, 0, 0),
                json!({"amount": 10, "region": "eu"}),
            ),
            ev(
                dt(2023, 5, 2, 0, 0, 0),
                json!({"amount": 5, "region": "eu"}),
            ),
            ev(
                dt(2023, 5, 3, 0, 0, 0),
                json!({"amount": 7, "region": "us"}),
            ),
            ev(dt(2023, 5, 4, 0, 0, 0), json!({"amount": 3})), // no region → NULL group
        ];
        let r = aggregate_grouped(&m, &e, &["region".to_string()], 0, Decimal::ZERO).unwrap();
        assert_eq!(r.aggregation, dec!(25)); // total
        assert_eq!(r.aggregations.len(), 3);
        let eu = r
            .aggregations
            .iter()
            .find(|a| a.grouped_by.get("region") == Some(&Some("eu".into())))
            .unwrap();
        assert_eq!(eu.aggregation, dec!(15));
        let us = r
            .aggregations
            .iter()
            .find(|a| a.grouped_by.get("region") == Some(&Some("us".into())))
            .unwrap();
        assert_eq!(us.aggregation, dec!(7));
        let null = r
            .aggregations
            .iter()
            .find(|a| a.grouped_by.get("region") == Some(&None))
            .unwrap();
        assert_eq!(null.aggregation, dec!(3));
    }

    #[test]
    fn charge_filter_matching_and_ignored() {
        let e = ev(
            dt(2023, 5, 1, 0, 0, 0),
            json!({"region": "eu", "cloud": "aws"}),
        );
        let matching = vec![PropertyFilter {
            key: "region".into(),
            values: vec!["eu".into()],
        }];
        assert!(event_matches_filters(&e, &matching, &[]));
        // Non-matching value.
        let matching2 = vec![PropertyFilter {
            key: "region".into(),
            values: vec!["us".into()],
        }];
        assert!(!event_matches_filters(&e, &matching2, &[]));
        // Ignored filter excludes an event that matches the competing filter.
        let ignored = vec![PropertyFilter {
            key: "cloud".into(),
            values: vec!["aws".into()],
        }];
        assert!(!event_matches_filters(&e, &matching, &ignored));
        // Missing property never matches.
        let missing = vec![PropertyFilter {
            key: "zone".into(),
            values: vec!["a".into()],
        }];
        assert!(!event_matches_filters(&e, &missing, &[]));
    }
}

/// `SumService#running_total`.
fn running_total(
    values: &[Decimal],
    free_units_per_events: i64,
    free_units_per_total_aggregation: Decimal,
) -> Vec<Decimal> {
    if free_units_per_events == 0 && free_units_per_total_aggregation.is_zero() {
        return Vec::new();
    }
    if free_units_per_events != 0 {
        let limit = free_units_per_events as usize;
        let mut total = Decimal::ZERO;
        return values
            .iter()
            .take(limit)
            .map(|v| {
                total += *v;
                total
            })
            .collect();
    }
    let mut total = Decimal::ZERO;
    let mut acc = Vec::new();
    for v in values {
        if free_units_per_total_aggregation < total {
            break;
        }
        total += *v;
        acc.push(total);
    }
    acc
}
