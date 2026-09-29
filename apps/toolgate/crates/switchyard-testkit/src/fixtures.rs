//! Provider-shaped fixtures: one per catalog tool. The response bodies follow
//! each provider's documented response format (trimmed); the nightly live
//! smoke test (`SWITCHYARD_LIVE_SMOKE=1`) checks the same tools against the
//! real providers.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use switchyard_catalog::Catalog;

use crate::http::{FakeHttp, FakeReply};

pub fn catalog_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

pub fn catalog() -> Catalog {
    Catalog::load_dir(catalog_dir()).expect("shipped catalog loads")
}

/// Server config pointing our own products and SearXNG at fake hosts.
pub fn test_config() -> BTreeMap<String, String> {
    [
        ("relay_url", "https://relay.test"),
        ("scribase_url", "https://scribase.test"),
        ("scribase_project_domain", "scribase.test"),
        ("rally_url", "https://rally.test"),
        ("menivor_url", "https://menivor.test"),
        ("searxng_url", "https://searx.test"),
        ("contact_email", "ops@railyard.test"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

pub struct Fixture {
    pub tool_id: &'static str,
    pub input: Value,
    pub host: &'static str,
    pub path: &'static str,
    pub reply: FakeReply,
    /// Extra routes some tools need (bootstrap files, DNS).
    pub extra: Vec<(&'static str, &'static str, FakeReply)>,
}

fn j(status: u16, v: Value) -> FakeReply {
    FakeReply::Json(status, v)
}

pub fn all() -> Vec<Fixture> {
    let dns_mx = j(200, json!({"Status": 0, "Answer": [{"name": "stripe.com", "type": 15, "TTL": 300, "data": "1 aspmx.l.google.com."}]}));
    vec![
        Fixture { tool_id: "web.search.searxng", input: json!({"q": "rust axum", "limit": 2}), host: "searx.test", path: "/search",
            reply: j(200, json!({"results": [{"title": "Axum", "url": "https://docs.rs/axum", "content": "web framework"}, {"title": "Tokio", "url": "https://tokio.rs", "content": "runtime"}, {"title": "Hyper", "url": "https://hyper.rs", "content": "http"}]})), extra: vec![] },
        Fixture { tool_id: "web.search.brave", input: json!({"q": "rust axum"}), host: "api.search.brave.com", path: "/res/v1/web/search",
            reply: j(200, json!({"web": {"results": [{"title": "Axum", "url": "https://docs.rs/axum", "description": "web framework"}]}})), extra: vec![] },
        Fixture { tool_id: "web.read.readable", input: json!({"url": "https://example.com/post"}), host: "example.com", path: "/post",
            reply: FakeReply::Text(200, "<html><head><title>Post</title></head><body><article><h1>Hello</h1><p>World</p></article></body></html>".into(), "text/html".into()), extra: vec![] },
        Fixture { tool_id: "geo.geocode.nominatim", input: json!({"q": "Brandenburger Tor", "limit": 1}), host: "nominatim.openstreetmap.org", path: "/search",
            reply: j(200, json!([{"display_name": "Brandenburger Tor, Berlin", "lat": "52.5162", "lon": "13.3777", "type": "attraction"}])), extra: vec![] },
        Fixture { tool_id: "geo.geocode.photon", input: json!({"q": "Brandenburger Tor"}), host: "photon.komoot.io", path: "/api",
            reply: j(200, json!({"features": [{"geometry": {"coordinates": [13.3777, 52.5162]}, "properties": {"name": "Brandenburger Tor", "osm_value": "attraction"}}]})), extra: vec![] },
        Fixture { tool_id: "weather.forecast.open-meteo", input: json!({"lat": 52.52, "lon": 13.41}), host: "api.open-meteo.com", path: "/v1/forecast",
            reply: j(200, json!({"timezone": "Europe/Berlin", "hourly": {"time": ["2026-09-29T00:00"], "temperature_2m": [11.2], "precipitation": [0.0]}, "hourly_units": {"temperature_2m": "°C"}})), extra: vec![] },
        Fixture { tool_id: "knowledge.wikipedia.summary", input: json!({"title": "Ada Lovelace"}), host: "en.wikipedia.org", path: "/api/rest_v1/page/summary/Ada_Lovelace",
            reply: j(200, json!({"title": "Ada Lovelace", "extract": "English mathematician.", "content_urls": {"desktop": {"page": "https://en.wikipedia.org/wiki/Ada_Lovelace"}}})), extra: vec![] },
        Fixture { tool_id: "research.papers.openalex", input: json!({"q": "transformers", "limit": 1}), host: "api.openalex.org", path: "/works",
            reply: j(200, json!({"results": [{"display_name": "Attention Is All You Need", "authorships": [{"author": {"display_name": "Ashish Vaswani"}}], "publication_year": 2017, "doi": "https://doi.org/10.48550/arxiv.1706.03762", "id": "https://openalex.org/W1"}]})), extra: vec![] },
        Fixture { tool_id: "research.papers.arxiv", input: json!({"q": "transformers", "limit": 1}), host: "export.arxiv.org", path: "/api/query",
            reply: FakeReply::Text(200, r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>http://arxiv.org/abs/1706.03762</id><published>2017-06-12T00:00:00Z</published><title>Attention Is All You Need</title><author><name>Ashish Vaswani</name></author></entry></feed>"#.into(), "application/atom+xml".into()), extra: vec![] },
        Fixture { tool_id: "dev.github.repos.search", input: json!({"q": "axum", "limit": 1}), host: "api.github.com", path: "/search/repositories",
            reply: j(200, json!({"total_count": 1, "items": [{"full_name": "tokio-rs/axum", "html_url": "https://github.com/tokio-rs/axum", "description": "web framework", "stargazers_count": 20000, "language": "Rust"}]})), extra: vec![] },
        Fixture { tool_id: "dev.package.info.registries", input: json!({"ecosystem": "crates", "name": "serde"}), host: "crates.io", path: "/api/v1/crates/serde",
            reply: j(200, json!({"crate": {"max_stable_version": "1.0.228", "description": "serialization", "homepage": "https://serde.rs", "repository": "https://github.com/serde-rs/serde"}, "versions": [{"license": "MIT OR Apache-2.0"}]})), extra: vec![] },
        Fixture { tool_id: "company.sec.filings", input: json!({"ticker": "AAPL", "limit": 1}), host: "data.sec.gov", path: "/submissions/CIK0000320193.json",
            reply: j(200, json!({"name": "Apple Inc.", "filings": {"recent": {"form": ["10-K"], "filingDate": ["2025-11-01"], "accessionNumber": ["0000320193-25-000106"], "primaryDocument": ["aapl.htm"]}}})),
            extra: vec![("www.sec.gov", "/files/company_tickers.json", j(200, json!({"0": {"cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc."}})))] },
        Fixture { tool_id: "company.lei.lookup", input: json!({"name": "Apple Inc"}), host: "api.gleif.org", path: "/api/v1/lei-records",
            reply: j(200, json!({"data": [{"attributes": {"lei": "HWUPKR0MPOU8FGXBT394", "entity": {"legalName": {"name": "Apple Inc."}, "jurisdiction": "US-CA", "status": "ACTIVE", "legalAddress": {"country": "US", "city": "Cupertino"}}}}]})), extra: vec![] },
        Fixture { tool_id: "domain.dns.lookup", input: json!({"name": "stripe.com", "type": "MX"}), host: "cloudflare-dns.com", path: "/dns-query", reply: dns_mx.clone(), extra: vec![] },
        Fixture { tool_id: "domain.rdap.whois", input: json!({"domain": "stripe.com"}), host: "rdap.verisign.test", path: "/com/v1/domain/stripe.com",
            reply: j(200, json!({"events": [{"eventAction": "registration", "eventDate": "1995-09-12"}], "status": ["active"], "nameservers": [{"ldhName": "NS1.STRIPE.COM"}]})),
            extra: vec![("data.iana.org", "/rdap/dns.json", j(200, json!({"services": [[["com"], ["https://rdap.verisign.test/com/v1/"]]]})))] },
        Fixture { tool_id: "social.hn.search", input: json!({"q": "rust", "limit": 1}), host: "hn.algolia.com", path: "/api/v1/search",
            reply: j(200, json!({"hits": [{"title": "Rust 2.0", "url": "https://blog.rust-lang.org", "points": 900, "num_comments": 400, "author": "steveklabnik", "created_at": "2026-09-01T00:00:00Z"}]})), extra: vec![] },
        Fixture { tool_id: "relay.chat.complete", input: json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]}), host: "relay.test", path: "/openai/v1/chat/completions",
            reply: j(200, json!({"model": "m", "choices": [{"message": {"role": "assistant", "content": "hello"}}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}})), extra: vec![] },
        Fixture { tool_id: "scribase.db.query", input: json!({"organization": "acme", "project": "web", "query": "select 1"}), host: "scribase.test", path: "/v1/organizations/acme/projects/web/environments/production/database/query",
            reply: j(200, json!({"columns": ["?column?"], "rows": [[1]]})), extra: vec![] },
        Fixture { tool_id: "scribase.db.branch.create", input: json!({"organization": "acme", "project": "web", "environment_id": "agent-task-1"}), host: "scribase.test", path: "/v1/organizations/acme/projects/web/branches",
            reply: j(200, json!({"environment_id": "agent-task-1", "status": "ready"})), extra: vec![] },
        Fixture { tool_id: "scribase.auth.magic_link.send", input: json!({"project_url": "https://abc.scribase.test", "email": "user@acme.com"}), host: "abc.scribase.test", path: "/auth/v1/magiclink",
            reply: j(200, json!({})), extra: vec![] },
        Fixture { tool_id: "rally.leads.search", input: json!({"kind": "companies", "filters": {"country": "DE"}, "limit": 2}), host: "rally.test", path: "/api/rally/v1/search/companies",
            reply: j(200, json!({"success": true, "data": {"companies": [{"name": "Acme GmbH", "domain": "acme.de"}, {"name": "Beta AG", "domain": "beta.de"}, {"name": "Gamma", "domain": "g.de"}], "total": 3}})), extra: vec![] },
        Fixture { tool_id: "rally.company.enrich", input: json!({"domain": "stripe.com"}), host: "rally.test", path: "/api/rally/v1/search/companies-by-domains",
            reply: j(200, json!({"success": true, "data": {"companies": [{"name": "Stripe", "domain": "stripe.com", "industry": "Payments"}]}})), extra: vec![] },
        Fixture { tool_id: "rally.email.verify", input: json!({"email": "founder@stripe.com"}), host: "cloudflare-dns.com", path: "/dns-query", reply: dns_mx, extra: vec![] },
        Fixture { tool_id: "rally.email.send", input: json!({"from": "Acme <hi@acme.com>", "to": ["a@x.io", "b@x.io"], "subject": "Hi", "text": "Hello"}), host: "rally.test", path: "/api/rally/v1/send",
            reply: j(200, json!({"success": true, "data": {"id": "msg_1", "status": "queued"}})), extra: vec![] },
        Fixture { tool_id: "menivor.video.render", input: json!({"subject": "Acme onboarding", "duration_s": 15}), host: "menivor.test", path: "/api/v1/videos",
            reply: j(200, json!({"status": 200, "message": "success", "data": {"task_id": "task_9"}})), extra: vec![] },
        Fixture { tool_id: "menivor.video.status", input: json!({"job_id": "task_9"}), host: "menivor.test", path: "/api/v1/tasks/task_9",
            reply: j(200, json!({"status": 200, "data": {"state": 1, "progress": 100, "videos": ["https://cdn.menivor.test/v.mp4"]}})), extra: vec![] },
        Fixture { tool_id: "menivor.tts.speak", input: json!({"text": "Hello there"}), host: "menivor.test", path: "/api/v1/audio",
            reply: j(200, json!({"status": 200, "data": {"task_id": "task_a"}})), extra: vec![] },
    ]
}

/// Install every fixture route on a fake upstream.
pub fn install_all(http: &Arc<FakeHttp>) {
    for f in all() {
        http.route(f.host, f.path, f.reply.clone());
        for (h, p, r) in f.extra {
            http.route(h, p, r);
        }
    }
}
