use relay_contract::{DataEnvelope, Microdollars};
use relay_cost_feed::{CostEventPage, CostEventState};

#[test]
fn v1_cost_event_page_shape_is_stable() -> Result<(), Box<dyn std::error::Error>> {
    let json = include_str!("fixtures/cost_event_page.json");
    let expected: serde_json::Value = serde_json::from_str(json)?;
    let typed: DataEnvelope<CostEventPage> = serde_json::from_str(json)?;

    assert_eq!(typed.data.items.len(), 1);
    assert_eq!(
        typed.data.items[0].event.charged_micros,
        Microdollars::new(7_500)
    );
    assert_eq!(serde_json::to_value(typed)?, expected);
    Ok(())
}

#[test]
fn v1_cost_event_list_query_shape_is_stable() -> Result<(), Box<dyn std::error::Error>> {
    let query: relay_cost_feed::CostEventListQuery = serde_json::from_value(serde_json::json!({
        "state": "dead_lettered",
        "limit": 25
    }))?;
    assert_eq!(query.state, Some(CostEventState::DeadLettered));
    assert_eq!(query.limit, 25);
    assert_eq!(query.before, None);
    Ok(())
}
