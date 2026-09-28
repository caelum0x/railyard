//! Contract tests: every catalog tool, driven through its real adapter against
//! a provider-shaped fixture, must produce output that validates against the
//! capability's output schema. Plus a live smoke run (opt-in).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{json, Value};
use switchyard_catalog::schema::Schema;
use switchyard_core::{CallCtx, Credential, CredentialKind, ErrorClass, HttpTransport};
use switchyard_testkit::{fixtures, FakeHttp, FakeReply};

fn ctx(http: Arc<dyn HttpTransport>, config: BTreeMap<String, String>, cred: bool) -> CallCtx {
    CallCtx {
        call_id: "call_test".into(),
        project_id: "prj_test".into(),
        org_id: "org_test".into(),
        http,
        credential: cred.then(|| Credential {
            id: "cred".into(),
            provider: "x".into(),
            kind: CredentialKind::ApiKey,
            secret: SecretString::from("secret-token".to_string()),
            meta: Value::Null,
            platform: false,
        }),
        caller_key: Some(SecretString::from("sk-t0-aaaaaaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string())),
        config: Arc::new(config),
        timeout: Duration::from_secs(5),
        idempotency_key: Some("idem-1".into()),
    }
}

#[tokio::test]
async fn every_catalog_tool_has_a_fixture_and_honours_its_output_schema() {
    let catalog = fixtures::catalog();
    let adapters = switchyard_adapters::build(&catalog).unwrap();
    let fx = fixtures::all();
    for t in catalog.tools() {
        assert!(fx.iter().any(|f| f.tool_id == t.id), "missing contract fixture for {}", t.id);
        assert!(adapters.contains_key(&t.id), "no adapter for {}", t.id);
    }
    for f in fx {
        let http = FakeHttp::new();
        fixtures::install_all(&http);
        let a = &adapters[f.tool_id];
        catalog.validate_input(f.tool_id, &f.input).unwrap_or_else(|e| panic!("{} fixture input invalid: {e:?}", f.tool_id));
        let out = a.call(&ctx(http.clone(), fixtures::test_config(), true), f.tool_id, f.input.clone()).await.unwrap_or_else(|e| panic!("{}: {e}", f.tool_id));
        let schema = Schema::compile(&catalog.output_schema(f.tool_id).unwrap()).unwrap();
        schema.validate(&out.data).unwrap_or_else(|e| panic!("{} output {} violates schema: {e:?}", f.tool_id, out.data));
        assert!(http.total_requests() >= 1, "{} made no upstream request", f.tool_id);
        // Every request carries our contact User-Agent.
        for r in http.requests() {
            assert!(r.headers.iter().any(|(k, v)| k == "user-agent" && v.contains("ops@railyard.test")), "{} missing UA", f.tool_id);
        }
    }
}

#[tokio::test]
async fn mapping_details() {
    let catalog = fixtures::catalog();
    let adapters = switchyard_adapters::build(&catalog).unwrap();
    let http = FakeHttp::new();
    fixtures::install_all(&http);
    let c = ctx(http.clone(), fixtures::test_config(), true);

    let s = adapters["web.search.searxng"].call(&c, "web.search.searxng", json!({"q": "rust", "limit": 2})).await.unwrap();
    assert_eq!(s.data["results"].as_array().unwrap().len(), 2, "limit truncates");
    assert_eq!(s.units, Some(2));
    assert_eq!(s.data["results"][0]["snippet"], "web framework");

    let b = adapters["web.search.brave"].call(&c, "web.search.brave", json!({"q": "rust"})).await.unwrap();
    assert_eq!(b.data["results"][0]["url"], "https://docs.rs/axum");
    let brave_req = http.requests().into_iter().find(|r| r.url.contains("brave")).unwrap();
    assert!(brave_req.sensitive_headers.iter().any(|(k, v)| k == "X-Subscription-Token" && v == "secret-token"), "BYO key injected as a credential header");
    assert!(brave_req.url.contains("count=10"), "{}", brave_req.url);

    let leads = adapters["rally.leads.search"].call(&c, "rally.leads.search", json!({"filters": {"country": "DE"}, "limit": 2})).await.unwrap();
    assert_eq!(leads.units, Some(2), "per-result pricing counts returned rows");

    let send = adapters["rally.email.send"].call(&c, "rally.email.send", json!({"from": "a@acme.com", "to": ["x@y.io", "z@y.io"], "reply_to": "r@acme.com"})).await.unwrap();
    assert_eq!(send.units, Some(2));
    let sreq = http.requests().into_iter().find(|r| r.url.ends_with("/api/rally/v1/send")).unwrap();
    assert!(sreq.headers.iter().any(|(k, v)| k == "idempotency-key" && v == "switchyard:prj_test:idem-1"));
    let body: Value = serde_json::from_slice(sreq.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["replyTo"], "r@acme.com");

    let wiki = adapters["knowledge.wikipedia.summary"].call(&c, "knowledge.wikipedia.summary", json!({"title": "Ada Lovelace"})).await.unwrap();
    assert_eq!(wiki.data["summary"], "English mathematician.");

    let v = adapters["rally.email.verify"].call(&c, "rally.email.verify", json!({"email": "info@stripe.com"})).await.unwrap();
    assert_eq!(v.data["result"], "risky", "role account");
    let v = adapters["rally.email.verify"].call(&c, "rally.email.verify", json!({"email": "not-an-email"})).await.unwrap();
    assert_eq!(v.data["result"], "undeliverable");
    let render = adapters["menivor.video.render"].call(&c, "menivor.video.render", json!({"subject": "x", "duration_s": 15})).await.unwrap();
    assert_eq!((render.data["job_id"].as_str(), render.units), (Some("task_9"), Some(15)));
}

#[tokio::test]
async fn enrich_miss_is_free() {
    let catalog = fixtures::catalog();
    let adapters = switchyard_adapters::build(&catalog).unwrap();
    let http = FakeHttp::new();
    http.route_json("rally.test", "/api/rally/v1/search/companies-by-domains", 200, json!({"success": true, "data": {"companies": []}}));
    let out = adapters["rally.company.enrich"].call(&ctx(http, fixtures::test_config(), true), "rally.company.enrich", json!({"domain": "nope.io"})).await.unwrap();
    assert_eq!(out.data["company"], Value::Null);
    assert_eq!(out.reported_cost_micros, Some(0));
}

#[tokio::test]
async fn error_classification() {
    let catalog = fixtures::catalog();
    let adapters = switchyard_adapters::build(&catalog).unwrap();
    let a = &adapters["weather.forecast.open-meteo"];
    let input = json!({"lat": 1.0, "lon": 2.0});
    for (status, class) in [(500, ErrorClass::Retryable), (503, ErrorClass::Retryable), (429, ErrorClass::Retryable), (400, ErrorClass::InputRejected), (404, ErrorClass::InputRejected)] {
        let http = FakeHttp::new();
        http.route_json("api.open-meteo.com", "/", status, json!({"error": true}));
        let e = a.call(&ctx(http, fixtures::test_config(), false), "weather.forecast.open-meteo", input.clone()).await.unwrap_err();
        assert_eq!(e.class, class, "status {status}");
    }
    let http = FakeHttp::new();
    http.route("api.open-meteo.com", "/", FakeReply::Fail(switchyard_core::EgressError::Timeout));
    assert_eq!(a.call(&ctx(http, fixtures::test_config(), false), "weather.forecast.open-meteo", input.clone()).await.unwrap_err().class, ErrorClass::Retryable);
    let http = FakeHttp::new();
    http.route("api.open-meteo.com", "/", FakeReply::Fail(switchyard_core::EgressError::BlockedAddress("10.0.0.1".into())));
    assert_eq!(a.call(&ctx(http, fixtures::test_config(), false), "weather.forecast.open-meteo", input).await.unwrap_err().class, ErrorClass::Fatal);
    // Own products without config fail fatally, not silently.
    let http = FakeHttp::new();
    let e = adapters["rally.leads.search"].call(&ctx(http, BTreeMap::new(), true), "rally.leads.search", json!({"filters": {}})).await.unwrap_err();
    assert_eq!(e.class, ErrorClass::Fatal);
}

/// Opt-in live smoke against the real public providers through the real
/// SSRF-safe egress: `SWITCHYARD_LIVE_SMOKE=1 cargo test -p switchyard-adapters live -- --nocapture`.
#[tokio::test]
async fn live_smoke_public_tools() {
    if std::env::var("SWITCHYARD_LIVE_SMOKE").ok().as_deref() != Some("1") {
        eprintln!("skipping live smoke (set SWITCHYARD_LIVE_SMOKE=1)");
        return;
    }
    let catalog = fixtures::catalog();
    let adapters = switchyard_adapters::build(&catalog).unwrap();
    let mut hosts: std::collections::BTreeSet<String> = catalog.egress_hosts();
    hosts.insert("example.com".into());
    let egress = switchyard_egress::SafeClient::new(switchyard_egress::EgressConfig {
        allow_hosts: hosts,
        user_agent: "Switchyard/0.1 live-smoke (+ops@railyard.dev)".into(),
        ..Default::default()
    })
    .unwrap();
    let http: Arc<dyn HttpTransport> = Arc::new(egress);
    let mut cfg = BTreeMap::new();
    cfg.insert("contact_email".to_string(), "ops@railyard.dev".to_string());
    let cases = [
        ("weather.forecast.open-meteo", json!({"lat": 52.52, "lon": 13.41, "days": 1})),
        ("knowledge.wikipedia.summary", json!({"title": "Ada Lovelace"})),
        ("research.papers.openalex", json!({"q": "attention is all you need", "limit": 2})),
        ("research.papers.arxiv", json!({"q": "transformer", "limit": 2})),
        ("geo.geocode.nominatim", json!({"q": "Brandenburger Tor, Berlin", "limit": 1})),
        ("geo.geocode.photon", json!({"q": "Brandenburger Tor", "limit": 1})),
        ("dev.github.repos.search", json!({"q": "axum", "limit": 2})),
        ("dev.package.info.registries", json!({"ecosystem": "crates", "name": "serde"})),
        ("dev.package.info.registries", json!({"ecosystem": "npm", "name": "react"})),
        ("dev.package.info.registries", json!({"ecosystem": "pypi", "name": "requests"})),
        ("company.sec.filings", json!({"ticker": "AAPL", "form": "10-K", "limit": 1})),
        ("company.lei.lookup", json!({"name": "Apple Inc.", "limit": 1})),
        ("domain.dns.lookup", json!({"name": "stripe.com", "type": "MX"})),
        ("domain.rdap.whois", json!({"domain": "stripe.com"})),
        ("social.hn.search", json!({"q": "rust", "limit": 2})),
        ("web.read.readable", json!({"url": "https://example.com/"})),
        ("rally.email.verify", json!({"email": "founder@stripe.com"})),
    ];
    let mut failures = Vec::new();
    for (tool, input) in cases {
        let c = CallCtx { timeout: Duration::from_secs(20), ..ctx(http.clone(), cfg.clone(), false) };
        match adapters[tool].call(&c, tool, input).await {
            Ok(out) => {
                let schema = Schema::compile(&catalog.output_schema(tool).unwrap()).unwrap();
                match schema.validate(&out.data) {
                    Ok(()) => eprintln!("LIVE OK   {tool}: {}", out.data.to_string().chars().take(140).collect::<String>()),
                    Err(e) => failures.push(format!("{tool}: schema {e:?} for {}", out.data)),
                }
            }
            Err(e) => failures.push(format!("{tool}: {e}")),
        }
    }
    assert!(failures.is_empty(), "live failures:\n{}", failures.join("\n"));
}
