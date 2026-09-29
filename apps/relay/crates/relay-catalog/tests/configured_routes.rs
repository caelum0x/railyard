//! Contract tests for the deployed Relay config (RY-RL2): `config/tensorzero.toml`
//! and `config/catalog.json` must agree, and the named Claude routes keep the
//! shape Rally and Menivor rely on.

#![expect(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use relay_catalog::{CatalogService, JsonCatalogSource};
use toml::Value;

const TOML: &str = include_str!("../../../config/tensorzero.toml");
const CATALOG: &str = include_str!("../../../config/catalog.json");

fn config() -> toml::Table {
    TOML.parse::<toml::Table>().expect("tensorzero.toml parses")
}

async fn deployed_catalog() -> CatalogService {
    CatalogService::initialize(Arc::new(JsonCatalogSource::new(CATALOG)))
        .await
        .expect("config/catalog.json is a valid catalog")
}

fn legs(config: &toml::Table, route: &str) -> Vec<(String, String)> {
    let model = config["models"][route].as_table().expect("route exists");
    let providers = model["providers"].as_table().unwrap();
    model["routing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| {
            let provider = &providers[key.as_str().unwrap()];
            (
                provider["type"].as_str().unwrap().to_string(),
                provider["model_name"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn names(config: &toml::Table, section: &str) -> Vec<String> {
    config
        .get(section)
        .and_then(Value::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn every_configured_route_is_priced_so_metered_startup_passes() {
    let config = config();
    let routes = names(&config, "models")
        .into_iter()
        .chain(names(&config, "embedding_models"))
        .map(|name| format!("tensorzero::model_name::{name}"))
        .chain(
            names(&config, "functions")
                .into_iter()
                .map(|name| format!("tensorzero::function_name::{name}")),
        );
    deployed_catalog()
        .await
        .validate_configured_routes(routes)
        .await
        .expect("the gateway's startup check (validate_configured_catalog_routes) passes");
}

#[test]
fn named_claude_routes_are_anthropic_first_then_openrouter() {
    let config = config();
    assert_eq!(
        legs(&config, "claude-haiku"),
        [
            ("anthropic".into(), "claude-haiku-4-5".into()),
            ("openrouter".into(), "anthropic/claude-haiku-4.5".into())
        ]
    );
    assert_eq!(
        legs(&config, "claude-sonnet-5"),
        [
            ("anthropic".into(), "claude-sonnet-5".into()),
            ("openrouter".into(), "anthropic/claude-sonnet-5".into())
        ]
    );
    // The existing route is unchanged: callers of `claude-sonnet` stay on 4.5.
    assert_eq!(
        legs(&config, "claude-sonnet"),
        [
            ("anthropic".into(), "claude-sonnet-4-5".into()),
            ("openrouter".into(), "anthropic/claude-sonnet-4.5".into())
        ]
    );
    for route in ["claude-haiku", "claude-sonnet-5"] {
        for (provider, _) in legs(&config, route) {
            let location = format!("dynamic::relay_{provider}_api_key");
            let table = config["models"][route]["providers"][provider.as_str()]
                .as_table()
                .unwrap();
            assert_eq!(table["api_key_location"].as_str(), Some(location.as_str()));
        }
    }
}

/// A consumer that sends `tensorzero::model_name::claude-haiku` instead of a
/// shorthand needs no other change: the name resolves and quotes, and billing
/// prices the route name, so the OpenRouter leg (Anthropic key revoked) is
/// billed at the same catalog price as the Anthropic leg.
#[tokio::test]
async fn named_routes_bill_the_catalog_list_price_whichever_leg_serves() {
    let catalog = deployed_catalog().await;
    for (route, list_id, input, output) in [
        (
            "claude-haiku",
            "anthropic/claude-haiku-4.5",
            1_000_000,
            5_000_000,
        ),
        (
            "claude-sonnet-5",
            "anthropic/claude-sonnet-5",
            2_000_000,
            10_000_000,
        ),
        (
            "claude-sonnet",
            "anthropic/claude-sonnet-4.5",
            3_000_000,
            15_000_000,
        ),
    ] {
        let selector = format!("tensorzero::model_name::{route}");
        let quote = catalog
            .quote(&selector, 1_000_000, 1_000_000)
            .await
            .unwrap();
        assert_eq!(quote.pricing.input_micro_usd_per_million, input, "{route}");
        assert_eq!(
            quote.pricing.output_micro_usd_per_million, output,
            "{route}"
        );
        // The route's retail price equals the model's list price.
        let list = catalog.resolve(list_id).await.unwrap();
        assert_eq!(list.pricing.input_micro_usd_per_million, input, "{list_id}");
        assert_eq!(
            list.pricing.output_micro_usd_per_million, output,
            "{list_id}"
        );
        // And the built-in billing table agrees with the list price.
        let builtin = relay_billing::BUILTIN_PRICES
            .iter()
            .find(|(id, _, _)| *id == list_id)
            .expect("list price in BUILTIN_PRICES");
        assert_eq!((builtin.1, builtin.2), (input, output), "{list_id}");
        // Billing looks the price up by the requested route name, not the leg.
        let fallback = relay_billing::ModelPrice::new(99_000_000, 99_000_000);
        let table = catalog.billing_price_table(fallback).await;
        let billed = table.price_for(&relay_billing::ModelId::from_tensorzero_selector(&selector));
        assert_eq!(
            billed,
            relay_billing::ModelPrice::new(input, output),
            "{route}"
        );
    }
}

/// H10 (Wave D hand-off): a compatibility alias that runs on another model is
/// priced at that model's cost times 1.25, never below cost. The cost is the
/// list price of the route's FIRST leg (the leg that serves while its key is
/// valid), taken from the built-in billing table.
#[tokio::test]
async fn groq_compat_aliases_sell_at_cost_times_1_25_never_below_cost() {
    let config = config();
    let catalog = deployed_catalog().await;
    for route in ["llama-3.1-8b-instant", "llama-3.3-70b-versatile"] {
        let (provider, model) = legs(&config, route)[0].clone();
        let leg_id = format!("{provider}/{model}");
        let cost = relay_billing::BUILTIN_PRICES
            .iter()
            .find(|(id, _, _)| *id == leg_id)
            .expect("first-leg cost in BUILTIN_PRICES");
        let selector = format!("tensorzero::model_name::{route}");
        let quote = catalog.quote(&selector, 1_000_000, 1_000_000).await.unwrap();
        assert!(
            quote.pricing.input_micro_usd_per_million >= cost.1
                && quote.pricing.output_micro_usd_per_million >= cost.2,
            "{route} sells below the cost of {leg_id}"
        );
    }
    // The 8B alias runs GPT-OSS 20B on Groq ($0.075 / $0.30): cost x 1.25.
    let quote = catalog
        .quote("tensorzero::model_name::llama-3.1-8b-instant", 1_000_000, 1_000_000)
        .await
        .unwrap();
    assert_eq!(quote.pricing.input_micro_usd_per_million, 75_000 * 5 / 4);
    assert_eq!(quote.pricing.output_micro_usd_per_million, 300_000 * 5 / 4);
}
