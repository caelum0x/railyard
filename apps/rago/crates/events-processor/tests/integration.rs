//! Integration tests exercising the real ported logic end-to-end (no network).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};

use rago::cache::Cache;
use rago::expression;
use rago::models::billable_metrics::AGGREGATION_TYPE_WEIGHTED_SUM;
use rago::models::charge_cache::ChargeCache;
use rago::models::stores::Cacher;
use rago::models::{matching_filter, AggregationType, Event, FlatFilter, Subscription};
use rago::utils::result::Result;
use rago::utils::time::NullTime;

// --- lago-expression (expression-core) evaluation ---

#[test]
fn expression_evaluates_arithmetic() {
    let event = r#"{"code":"c","timestamp":1,"properties":{}}"#;
    assert_eq!(expression::evaluate("2 * 3", event).as_deref(), Some("6"));
}

#[test]
fn expression_reads_properties() {
    let event = r#"{"code":"c","timestamp":1,"properties":{"a":2,"b":3}}"#;
    assert_eq!(
        expression::evaluate("event.properties.a + event.properties.b", event).as_deref(),
        Some("5")
    );
}

#[test]
fn expression_returns_none_on_parse_error() {
    let event = r#"{"code":"c","timestamp":1,"properties":{}}"#;
    assert_eq!(expression::evaluate("@@ not valid @@", event), None);
}

// --- AggregationType.String() ---

#[test]
fn aggregation_type_string_mapping() {
    assert_eq!(AggregationType(0).to_string(), "count");
    assert_eq!(AGGREGATION_TYPE_WEIGHTED_SUM.to_string(), "weighted_sum");
    assert_eq!(AggregationType(7).to_string(), "custom");
}

// --- Event -> EnrichedEvent conversion ---

#[test]
fn event_to_enriched_event_parses_timestamp() {
    let raw = r#"{
        "organization_id":"org1",
        "external_subscription_id":"ext1",
        "transaction_id":"txn1",
        "code":"api_calls",
        "properties":{"value":"10"},
        "precise_total_amount_cents":"0",
        "timestamp":1719000000.5
    }"#;
    let event: Event = serde_json::from_str(raw).unwrap();
    let event = Arc::new(event);
    let res = event.to_enriched_event();
    assert!(res.is_success());
    let enriched = res.into_value();
    assert_eq!(enriched.organization_id, "org1");
    assert_eq!(enriched.timestamp, 1719000000.5);
    // "%f" formatting -> 6 decimal places.
    assert_eq!(enriched.timestamp_str, "1719000000.500000");
}

// --- MatchingFilter: falls back to the default bucket when nothing matches ---

#[test]
fn matching_filter_defaults_when_no_match() {
    let raw = r#"{"code":"c","timestamp":1,"properties":{}}"#;
    let event: Event = serde_json::from_str(raw).unwrap();
    let enriched = Arc::new(event).to_enriched_event().into_value();

    let mut filters_map = std::collections::HashMap::new();
    filters_map.insert("region".to_string(), vec!["eu".to_string()]);

    let filter = FlatFilter {
        organization_id: "org".into(),
        billable_metric_code: "bm".into(),
        plan_id: "plan".into(),
        charge_id: "charge1".into(),
        charge_updated_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        charge_filter_id: Some("cf1".into()),
        charge_filter_updated_at: Some(Utc::now()),
        filters: Some(filters_map),
        pricing_group_keys: vec![],
        pay_in_advance: false,
        accepts_target_wallet: false,
    };

    // Event has no "region" property, so no filter matches -> default bucket.
    let matched = matching_filter(&[filter], &enriched);
    assert_eq!(matched.charge_id, "charge1");
    assert!(matched.charge_filter_id.is_none()); // default filter has no filter id
}

// --- redb-backed cache store round-trip + prefix search ---

#[test]
fn cache_store_roundtrip_and_search() {
    let cache = Cache::new(String::new()).unwrap();

    let sub = Subscription {
        id: "s1".into(),
        organization_id: Some("org".into()),
        external_id: "ext".into(),
        plan_id: "plan".into(),
        started_at: NullTime::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        ..Default::default()
    };

    assert!(cache.set_subscription(&sub).is_success());

    let got: Result<Subscription> = cache.get_subscription("org", "ext", "s1");
    assert!(got.is_success());
    assert_eq!(got.value().plan_id, "plan");

    // Search by timestamp after started_at should find it.
    let search = cache.search_subscriptions(
        "org",
        "ext",
        Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
    );
    assert!(search.is_success());
    assert_eq!(search.value().id, "s1");

    // A missing key is a non-capturable, non-retryable "record not found".
    let missing: Result<Subscription> = cache.get_subscription("org", "ext", "nope");
    assert!(missing.is_failure());
    assert!(!missing.is_capturable());
    assert!(!missing.is_retryable());
}

// --- ChargeCache key format (load-bearing: must match the usage reader) ---

struct RecordingCacher {
    keys: Mutex<Vec<String>>,
}

#[async_trait]
impl Cacher for RecordingCacher {
    async fn expire_key(&self, key: &str) -> Result<bool> {
        self.keys.lock().unwrap().push(key.to_string());
        Result::success(true)
    }
}

#[tokio::test]
async fn charge_cache_builds_expected_key() {
    let recorder = Arc::new(RecordingCacher {
        keys: Mutex::new(Vec::new()),
    });
    let charge_cache = ChargeCache::new(recorder.clone());

    let ff = FlatFilter {
        organization_id: "org".into(),
        billable_metric_code: "bm".into(),
        plan_id: "plan".into(),
        charge_id: "charge1".into(),
        charge_updated_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        charge_filter_id: Some("cf1".into()),
        charge_filter_updated_at: Some(Utc.with_ymd_and_hms(2026, 2, 3, 4, 5, 6).unwrap()),
        filters: None,
        pricing_group_keys: vec![],
        pay_in_advance: false,
        accepts_target_wallet: false,
    };

    let res = charge_cache.expire(&ff, "sub1").await;
    assert!(res.is_success());

    let keys = recorder.keys.lock().unwrap();
    assert_eq!(
        keys[0],
        "charge-usage/1/charge1/sub1/2026-01-02T03:04:05Z/cf1/2026-02-03T04:05:06Z"
    );
}
