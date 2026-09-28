//! The shipped catalog loads, is internally consistent and is searchable by task.

use std::path::PathBuf;

use serde_json::json;
use switchyard_catalog::{Catalog, SearchFilters};
use switchyard_core::SideEffect;

fn catalog() -> Catalog {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog");
    Catalog::load_dir(dir).expect("shipped catalog must load")
}

#[test]
fn ships_the_mvp_catalog() {
    let c = catalog();
    let tools: Vec<_> = c.tools().collect();
    assert!(tools.len() >= 25, "MVP catalog has at least 25 tools, got {}", tools.len());
    assert!(c.capabilities().count() >= 23);
    let own = tools.iter().filter(|t| ["relay", "scribase", "rally", "menivor"].contains(&t.provider.as_str())).count();
    assert!(own >= 10, "own products: {own}");
    // No platform paid keys in the MVP.
    assert!(c.providers().all(|p| p.kind != switchyard_core::ProviderKind::Platform));
    assert!(c.bundle("lead-to-video").is_some());
}

#[test]
fn every_tool_has_an_input_schema_and_fallback_capabilities_have_two_providers() {
    let c = catalog();
    for t in c.tools() {
        assert!(c.input_schema(&t.id).is_some(), "{}", t.id);
        assert!(c.output_schema(&t.id).is_some(), "{}", t.id);
    }
    for cap in ["web.search", "research.papers.search", "geo.geocode"] {
        assert!(c.tools_for_capability(cap).len() >= 2, "{cap} needs a fallback provider");
    }
}

#[test]
fn search_by_task_finds_the_right_capability() {
    let c = catalog();
    let f = SearchFilters { limit: 3, ..Default::default() };
    let cases = [
        ("verify an email address", "email.verify"),
        ("weather forecast for berlin", "weather.forecast"),
        ("find academic papers about transformers", "research.papers.search"),
        ("who owns this domain whois", "domain.whois"),
        ("geocode an address", "geo.geocode"),
        ("render a short tiktok video", "video.render"),
        ("latest version of an npm package", "dev.package.info"),
    ];
    for (q, want) in cases {
        let hits = c.search(q, &f);
        assert_eq!(hits.first().map(|h| h.capability.as_str()), Some(want), "query {q:?} -> {:?}", hits.iter().map(|h| &h.capability).collect::<Vec<_>>());
    }
}

#[test]
fn search_filters_side_effect_and_price() {
    let c = catalog();
    let sends = c.search("send email", &SearchFilters { side_effect: Some(SideEffect::Send), limit: 10, ..Default::default() });
    assert!(sends.iter().all(|h| h.candidates.iter().all(|x| x.side_effect == "send")));
    assert!(sends.iter().any(|h| h.capability == "email.send"));
    let free = c.search("company leads", &SearchFilters { max_price_micros: Some(0), limit: 10, ..Default::default() });
    assert!(free.iter().all(|h| h.capability != "leads.search"), "paid-only capability filtered out");
}

#[test]
fn validation_rejects_bad_input_before_spend() {
    let c = catalog();
    assert!(c.validate_input("weather.forecast", &json!({"lat": 52.5, "lon": 13.4})).is_ok());
    let errs = c.validate_input("weather.forecast", &json!({"lat": 200})).unwrap_err();
    assert!(errs.iter().any(|(p, _)| p == "/lat"));
    assert!(c.validate_input("company.filings", &json!({})).is_err(), "needs ticker or cik");
    assert!(c.validate_input("rally.email.send", &json!({"from": "a@b.io", "to": ["x@y.io"]})).is_ok());
}

#[test]
fn photon_refuses_country_filter() {
    let c = catalog();
    let photon = c.tool("geo.geocode.photon").unwrap();
    assert!(photon.accepts_fields(&json!({"q": "Paris", "country": "fr"})).is_err());
    assert!(photon.accepts_fields(&json!({"q": "Paris"})).is_ok());
}
