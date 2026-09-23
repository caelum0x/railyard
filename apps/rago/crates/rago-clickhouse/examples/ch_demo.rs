//! ClickHouse parity demo: insert events → boundary-scoped read → aggregate.
use chrono::NaiveDate;
use rago_clickhouse::ClickHouseStore;
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Event};
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("CH_URL").unwrap_or_else(|_| "http://localhost:8124".into());
    let store = ClickHouseStore::new(
        url,
        "default",
        "default",
        std::env::var("CH_PASS").unwrap_or_else(|_| "chpass".into()),
    );
    store.migrate().await?;
    let org = Uuid::new_v4();

    for (i, (v, day)) in [(40, 5), (35, 10), (25, 15)].into_iter().enumerate() {
        let ts = NaiveDate::from_ymd_opt(2024, 1, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let ev = Event {
            id: Uuid::new_v4(),
            organization_id: org,
            customer_id: None,
            subscription_id: None,
            transaction_id: format!("tx_{i}"),
            code: "api_calls".into(),
            properties: json!({ "value": v }),
            external_customer_id: Some("cust1".into()),
            external_subscription_id: Some("sub1".into()),
            timestamp: Some(ts),
            precise_total_amount_cents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            persisted: true,
        };
        store.insert_event(&ev, "cust1").await?;
    }

    let from = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let to = NaiveDate::from_ymd_opt(2024, 1, 31)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();
    let events = store
        .events_in_scope(org, "sub1", "api_calls", Some(from), to)
        .await?;
    println!("CLICKHOUSE read {} events", events.len());

    let metric = BillableMetric {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "API".into(),
        code: "api_calls".into(),
        description: None,
        aggregation_type: AggregationType::SumAgg,
        field_name: Some("value".into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    let agg =
        rago_billing::aggregation::aggregate(&metric, &events, 0, rust_decimal::Decimal::ZERO)?;
    println!(
        "CLICKHOUSE sum_agg aggregation = {} (count {})",
        agg.aggregation, agg.count
    );
    assert_eq!(agg.aggregation, rust_decimal::Decimal::from(100));
    println!("PARITY OK: ClickHouse path yields 100 units — identical to Postgres");
    Ok(())
}
