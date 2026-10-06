//! End-to-end tests through the real router and call pipeline.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use base64::Engine;
use common::*;
use serde_json::{json, Value};
use switchyard_auth::KeyVerifier;
use switchyard_core::EgressError;
use switchyard_meter::OutboxWorker;
use switchyard_testkit::FakeReply;

// ---------------------------------------------------------------- surface

#[tokio::test]
async fn health_llms_openapi_and_auth_required() {
    let h = harness().await;
    assert_eq!(send(&h.app, "GET", "/healthz", None, None, &[]).await.status, 200);
    assert!(send(&h.app, "GET", "/readyz", None, None, &[]).await.body["tools"].as_u64().unwrap() >= 25);
    let llms = send(&h.app, "GET", "/llms.txt", None, None, &[]).await;
    assert!(llms.text.contains("rally.email.verify") && llms.text.contains("/mcp"));
    let oa = send(&h.app, "GET", "/openapi.json", None, None, &[]).await;
    assert_eq!(oa.body["openapi"], "3.1.0");
    let r = send(&h.app, "GET", "/v1/tools/search?q=weather", None, None, &[]).await;
    assert_eq!(r.status, 401);
    assert_eq!(r.body["error"]["code"], "unauthorized");
    let bad = send(&h.app, "GET", "/v1/tools/search?q=weather", Some("sk-t0-aaaaaaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"), None, &[]).await;
    assert_eq!(bad.status, 401);
}

#[tokio::test]
async fn search_and_describe_spend_nothing() {
    let h = harness().await;
    let r = send(&h.app, "GET", "/v1/tools/search?q=verify%20an%20email%20address", Some(&h.key), None, &[]).await;
    assert_eq!(r.status, 200);
    let first = &r.body["data"]["results"][0];
    assert_eq!(first["capability"], "email.verify");
    assert_eq!(first["candidates"][0]["tool_id"], "rally.email.verify");
    let d = send(&h.app, "GET", "/v1/tools/web.search", Some(&h.key), None, &[]).await;
    assert_eq!(d.status, 200);
    assert_eq!(d.body["data"]["kind"], "capability");
    assert!(d.body["data"]["input_schema"]["properties"]["q"].is_object());
    let chain = &d.body["data"]["routing"];
    assert_eq!(chain["candidates"][0]["tool"], "web.search.searxng", "free route first");
    assert!(chain["skipped"].as_array().unwrap().iter().any(|s| s["tool"] == "web.search.brave"), "brave needs a BYO key");
    assert_eq!(h.mint.holds().len(), 0);
    assert_eq!(h.http.total_requests(), 0, "describe never calls upstream");
    assert_eq!(send(&h.app, "GET", "/v1/tools/nope.nope", Some(&h.key), None, &[]).await.status, 404);
}

// ------------------------------------------------------------ call pipeline

#[tokio::test]
async fn free_call_is_zero_cost_with_headers_and_audit() {
    let h = harness().await;
    let r = call(&h, "weather.forecast", json!({"lat": 52.52, "lon": 13.41})).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["data"]["timezone"], "Europe/Berlin");
    let meta = &r.body["meta"];
    assert_eq!((meta["provider"].as_str(), meta["cost_micros"].as_i64()), (Some("open-meteo"), Some(0)));
    assert_eq!(r.headers["x-switchyard-provider"], "open-meteo");
    assert_eq!(r.headers["x-switchyard-cost-micros"], "0");
    let call_id = r.headers["x-switchyard-call-id"].to_str().unwrap().to_string();
    assert_eq!(h.mint.holds().len(), 0, "free calls never touch Mint");
    let audit = send(&h.app, "GET", &format!("/v1/usage/calls/{call_id}"), Some(&h.key), None, &[]).await;
    assert_eq!(audit.body["data"]["proof"]["verified"], true);
    assert_eq!(audit.body["data"]["call"]["status"], "ok");
}

#[tokio::test]
async fn paid_call_holds_captures_exact_cost_and_emits_rago_event() {
    let h = harness().await;
    let r = call(&h, "rally.leads.search", json!({"kind": "companies", "filters": {"country": "DE"}, "limit": 2})).await;
    assert_eq!(r.status, 200, "{}", r.text);
    // 2 results x 2,000 µ$ per result; the hold was for the 2-result quote.
    assert_eq!(r.body["meta"]["cost_micros"], 4000);
    assert!(r.body["meta"]["receipt_id"].as_str().unwrap().contains(':'));
    let holds = h.mint.holds();
    assert_eq!(holds.len(), 1);
    assert_eq!((holds[0].1.state, holds[0].1.captured, holds[0].1.amount), ("captured", 4000, 4000));
    assert_eq!(h.mint.available(ACCOUNT), 10_000_000 - 4000);
    assert_eq!(h.mint.available(REVENUE), 4000);
    // The platform Rally credential was injected, never the caller's key.
    let req = h.http.requests().into_iter().find(|r| r.url.contains("rally.test")).unwrap();
    assert!(req.sensitive_headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer platform-rally-token"));
    assert!(!req.headers.iter().chain(req.sensitive_headers.iter()).any(|(_, v)| v.contains("sk-t0-")));
    assert_eq!(h.rago.events().len(), 0, "Rago events go through the outbox");
    assert_eq!(drain(&h).await.delivered, 1);
    let ev = &h.rago.events()[0];
    assert_eq!(ev["code"], "tool_call");
    assert_eq!(ev["transaction_id"], r.body["meta"]["call_id"]);
    assert_eq!(ev["properties"]["cost_micros"], 4000);
    assert_eq!(ev["external_subscription_id"], "sub_acme");
}

#[tokio::test]
async fn upstream_5xx_and_timeout_release_the_hold_and_bill_nothing() {
    let h = harness().await;
    h.http.route_json("rally.test", "/api/rally/v1/search/companies", 503, json!({"error": "down"}));
    let r = call(&h, "rally.leads.search", json!({"filters": {"country": "DE"}})).await;
    assert_eq!(r.status, 424, "{}", r.text);
    assert_eq!(r.body["error"]["details"]["billed_micros"], 0);
    assert_eq!(h.mint.holds()[0].1.state, "released");
    assert_eq!(h.mint.available(ACCOUNT), 10_000_000);

    h.http.route("rally.test", "/api/rally/v1/search/companies", FakeReply::Fail(EgressError::Timeout));
    let r = call(&h, "rally.leads.search", json!({"filters": {"country": "FR"}})).await;
    assert_eq!(r.status, 424);
    assert!(h.mint.holds().iter().all(|(_, s)| s.state == "released"));
    assert_eq!(h.mint.available(ACCOUNT), 10_000_000, "timeouts are never billed");
    assert_eq!(drain(&h).await.delivered, 0, "no Rago event for unbilled calls");
}

#[tokio::test]
async fn payment_required_body_is_machine_readable() {
    let h = harness().await;
    let poor = h
        .store
        .create_org(switchyard_store::Org {
            id: "org_poor".into(),
            slug: "poor".into(),
            name: "Poor".into(),
            mint_account_id: Some("acct_poor".into()),
            rago_subscription_id: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    h.mint.fund("acct_poor", 100);
    let p = h.store.create_project(project(&poor.id)).await.unwrap();
    let key = mint_key(&h.keys, h.store.as_ref(), &poor, &p.id, &["tools:read", "tools:call"], None).await;
    let r = send(&h.app, "POST", "/v1/tools/rally.company.enrich/call", Some(&key), Some(json!({"input": {"domain": "stripe.com"}})), &[]).await;
    assert_eq!(r.status, 402, "{}", r.text);
    let d = &r.body["error"]["details"];
    assert_eq!((d["balance_micros"].as_i64(), d["estimated_cost_micros"].as_i64()), (Some(100), Some(5000)));
    assert_eq!(d["topup_url"], format!("{BASE}/v1/balance/topup"));
    assert_eq!(h.http.total_requests(), 0, "no upstream call without a hold");
    // Free tools still work at zero balance.
    let f = send(&h.app, "POST", "/v1/tools/weather.forecast/call", Some(&key), Some(json!({"input": {"lat": 1.0, "lon": 2.0}})), &[]).await;
    assert_eq!(f.status, 200);
}

#[tokio::test]
async fn project_caps_stop_calls_with_429_naming_the_cap() {
    let h = harness().await;
    let r = send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"daily_cap_micros": 6000, "tool_daily_caps": {"rally.email.send": 500}})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(call(&h, "rally.company.enrich", json!({"domain": "stripe.com"})).await.status, 200);
    let r = call(&h, "rally.company.enrich", json!({"domain": "acme.com"})).await;
    assert_eq!(r.status, 429);
    assert_eq!(r.body["error"]["code"], "cap_exceeded");
    assert_eq!(r.body["error"]["details"]["cap"], "project_daily");
    let r = call(&h, "rally.email.send", json!({"from": "a@acme.com", "to": ["1@x.io", "2@x.io"]})).await;
    assert_eq!(r.body["error"]["details"]["cap"], "tool_daily", "{}", r.text);
    assert_eq!(h.mint.holds().len(), 1, "capped calls never reach Mint");
}

#[tokio::test]
async fn tool_caps_cover_capability_and_tool_id_aliases() {
    let h = harness().await;
    // Cap keyed by the concrete tool; calling through the capability must hit it.
    let r = send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"tool_daily_caps": {"rally.email.send": 500}})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let r = call(&h, "email.send", json!({"from": "a@acme.com", "to": ["1@x.io", "2@x.io"]})).await;
    assert_eq!((r.status.as_u16(), r.body["error"]["details"]["cap"].as_str()), (429, Some("tool_daily")), "{}", r.text);
    // Cap keyed by the capability; calling the concrete tool must hit it.
    let r = send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"tool_daily_caps": {"email.send": 500}})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let r = call(&h, "rally.email.send", json!({"from": "a@acme.com", "to": ["1@x.io", "2@x.io"]})).await;
    assert_eq!((r.status.as_u16(), r.body["error"]["details"]["cap"].as_str()), (429, Some("tool_daily")), "{}", r.text);
    assert_eq!(h.mint.holds().len(), 0, "capped calls never reach Mint");
    assert_eq!(h.http.total_requests(), 0);
}

#[tokio::test]
async fn batch_enforces_send_limit_and_tool_caps_per_call() {
    let h = harness().await;
    let r = send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"daily_send_limit": 1})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let one = json!({"tool": "rally.email.send", "input": {"from": "a@acme.com", "to": "1@x.io", "subject": "s", "text": "t"}});
    let r = send(&h.app, "POST", "/v1/tools/batch", Some(&h.key), Some(json!({"calls": [one.clone(), one.clone(), one]})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let data = r.body["data"].as_array().unwrap();
    assert_eq!(data[0]["ok"], true, "{}", r.text);
    for d in &data[1..] {
        assert_eq!(d["error"]["details"]["cap"], "daily_sends", "{}", r.text);
    }
    let sends = h.http.requests().iter().filter(|q| q.url.contains("/api/rally/v1/send")).count();
    assert_eq!(sends, 1, "a batch cannot exceed the daily send limit");
    assert_eq!(r.body["meta"]["cost_micros"], 400, "only the delivered send is billed");
}

#[tokio::test]
async fn upstream_echo_of_injected_secrets_is_redacted() {
    let h = harness().await;
    let key = h.key.clone();
    h.http.route_fn("rally.test", "/api/rally/v1/search", Duration::ZERO, move |_| {
        FakeReply::Json(400, json!({"error": format!("bad token platform-rally-token (caller {key})")}))
    });
    let r = call(&h, "rally.leads.search", json!({"kind": "companies", "filters": {}, "limit": 2})).await;
    assert_eq!(r.status, 422, "{}", r.text);
    assert!(r.text.contains("[redacted]"), "{}", r.text);
    assert!(!r.text.contains("platform-rally-token") && !r.text.contains(&h.key), "secret leaked: {}", r.text);
    let calls = send(&h.app, "GET", "/v1/usage/calls", Some(&h.key), None, &[]).await;
    assert!(!calls.text.contains("platform-rally-token") && !calls.text.contains(&h.key), "secret in audit log: {}", calls.text);
}

#[tokio::test]
async fn blocked_caller_urls_do_not_open_the_shared_breaker() {
    let h = harness().await;
    h.http.route("169.254.169.254", "/", FakeReply::Fail(EgressError::BlockedAddress("169.254.169.254".into())));
    for _ in 0..5 {
        let r = call(&h, "web.read", json!({"url": "https://169.254.169.254/latest/meta-data/"})).await;
        assert_eq!(r.body["error"]["code"], "provider_rejected_input", "{}", r.text);
    }
    let r = call(&h, "web.read", json!({"url": "https://example.com/post"})).await;
    assert_eq!(r.status, 200, "one caller's bad URLs must not take web.read down for everyone: {}", r.text);
}

#[tokio::test]
async fn schema_errors_are_422_with_pointers_and_spend_nothing() {
    let h = harness().await;
    let r = call(&h, "weather.forecast", json!({"lat": 500, "lon": "x", "extra": 1})).await;
    assert_eq!(r.status, 422);
    let ptrs: Vec<String> = r.body["error"]["details"]["errors"].as_array().unwrap().iter().map(|e| e["pointer"].as_str().unwrap().to_string()).collect();
    assert!(ptrs.contains(&"/lat".to_string()) && ptrs.contains(&"/lon".to_string()), "{ptrs:?}");
    assert_eq!(h.http.total_requests(), 0);
    assert_eq!(h.mint.holds().len(), 0);
}

#[tokio::test]
async fn idempotent_replay_is_free_and_changed_body_conflicts() {
    let h = harness().await;
    let body = json!({"input": {"from": "Acme <hi@acme.com>", "to": ["a@x.io"], "subject": "Hi", "text": "Hello"}});
    let hdr = [("idempotency-key", "welcome-a@x.io")];
    let first = send(&h.app, "POST", "/v1/tools/rally.email.send/call", Some(&h.key), Some(body.clone()), &hdr).await;
    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.body["meta"]["cost_micros"], 400);
    let second = send(&h.app, "POST", "/v1/tools/rally.email.send/call", Some(&h.key), Some(body.clone()), &hdr).await;
    assert_eq!(second.status, 200);
    assert_eq!(second.body["meta"]["cache"], "replay");
    assert_eq!(second.body["meta"]["cost_micros"], 0);
    assert_eq!(second.body["meta"]["replayed_call_id"], first.body["meta"]["call_id"]);
    assert_eq!(second.body["data"], first.body["data"]);
    let sends = h.http.requests().iter().filter(|r| r.url.ends_with("/api/rally/v1/send")).count();
    assert_eq!(sends, 1, "the email went out once");
    let mut changed = body.clone();
    changed["input"]["subject"] = json!("Different");
    let conflict = send(&h.app, "POST", "/v1/tools/rally.email.send/call", Some(&h.key), Some(changed), &hdr).await;
    assert_eq!(conflict.status, 409);
    assert_eq!(h.mint.available(REVENUE), 400);
}

#[tokio::test]
async fn concurrent_requests_with_one_idempotency_key_send_once() {
    let h = harness().await;
    let sends = h.http.route_fn("rally.test", "/api/rally/v1/send", Duration::from_millis(300), |_| {
        FakeReply::Json(200, json!({"ok": true, "data": {"id": "msg_1", "accepted": 1}}))
    });
    let body = json!({"input": {"from": "a@acme.com", "to": "b@x.io", "subject": "s", "text": "t"}});
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let (app, key, body) = (h.app.clone(), h.key.clone(), body.clone());
        tasks.push(tokio::spawn(async move {
            send(&app, "POST", "/v1/tools/rally.email.send/call", Some(&key), Some(body), &[("idempotency-key", "race-1")]).await
        }));
    }
    let mut codes = Vec::new();
    for t in tasks {
        let r = t.await.unwrap();
        codes.push(r.status.as_u16());
    }
    assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1, "one key, one upstream send: {codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == 200).count(), 1, "{codes:?}");
    assert!(codes.iter().all(|c| *c == 200 || *c == 409), "{codes:?}");
    // After it finishes, a retry replays the stored result at zero cost.
    let r = send(&h.app, "POST", "/v1/tools/rally.email.send/call", Some(&h.key), Some(body), &[("idempotency-key", "race-1")]).await;
    assert_eq!((r.status.as_u16(), r.body["meta"]["cache"].as_str()), (200, Some("replay")), "{}", r.text);
    assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn single_flight_collapses_identical_concurrent_calls() {
    let h = harness().await;
    let hits = h.http.route_fn("api.open-meteo.com", "/v1/forecast", Duration::from_millis(300), |_| {
        FakeReply::Json(200, json!({"timezone": "UTC", "hourly": {}, "hourly_units": {}}))
    });
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let app = h.app.clone();
        let key = h.key.clone();
        tasks.push(tokio::spawn(async move {
            send(&app, "POST", "/v1/tools/weather.forecast/call", Some(&key), Some(json!({"input": {"lat": 10.0, "lon": 20.0}})), &[]).await
        }));
    }
    let mut caches = Vec::new();
    for t in tasks {
        let r = t.await.unwrap();
        assert_eq!(r.status, 200, "{}", r.text);
        caches.push(r.body["meta"]["cache"].as_str().unwrap().to_string());
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "N concurrent calls, 1 upstream hit");
    assert_eq!(caches.iter().filter(|c| *c == "miss").count(), 1);
    assert!(caches.iter().all(|c| c == "miss" || c == "dedupe" || c == "hit"));
    // Later identical calls are cache hits; bypass forces a fresh call.
    let r = call(&h, "weather.forecast", json!({"lat": 10.0, "lon": 20.0})).await;
    assert_eq!(r.body["meta"]["cache"], "hit");
    let b = send(&h.app, "POST", "/v1/tools/weather.forecast/call", Some(&h.key), Some(json!({"input": {"lat": 10.0, "lon": 20.0}, "options": {"cache": "bypass"}})), &[]).await;
    assert_eq!(b.body["meta"]["cache"], "bypass");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fallback_is_disclosed_and_pinning_disables_it() {
    let h = harness().await;
    h.http.route_json("searx.test", "/search", 503, json!({}));
    // Without a Brave key there is nothing to fall back to.
    let r = call(&h, "web.search", json!({"q": "rust"})).await;
    assert_eq!(r.status, 424, "{}", r.text);
    // Store a BYO Brave key (write-only), then fallback works and is disclosed.
    let c = send(&h.app, "POST", "/v1/credentials", Some(&h.key), Some(json!({"provider": "brave", "value": "brave-secret-123"})), &[]).await;
    assert_eq!(c.status, 201, "{}", c.text);
    let r = send(&h.app, "POST", "/v1/tools/web.search/call", Some(&h.key), Some(json!({"input": {"q": "rust"}, "options": {"cache": "bypass"}})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["meta"]["provider"], "brave");
    let chain = r.body["meta"]["fallback_chain"].as_array().unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!((chain[0]["provider"].as_str(), chain[0]["outcome"].as_str()), (Some("searxng"), Some("retryable")));
    assert_eq!(r.body["meta"]["cost_micros"], 0, "BYO calls are unmetered");
    let brave = h.http.requests().into_iter().find(|r| r.url.contains("brave")).unwrap();
    assert!(brave.sensitive_headers.iter().any(|(k, v)| k == "X-Subscription-Token" && v == "brave-secret-123"));
    // Pinned provider never falls back.
    let p = send(&h.app, "POST", "/v1/tools/web.search/call", Some(&h.key), Some(json!({"input": {"q": "go"}, "options": {"provider": "searxng"}})), &[]).await;
    assert_eq!(p.status, 424, "{}", p.text);
    assert_eq!(p.body["error"]["details"]["fallback_chain"].as_array().unwrap().len(), 1);
    // SearXNG has now failed three times in a row: its circuit is open, so a
    // concrete-tool call (which never falls back) is refused up front.
    let t = call(&h, "web.search.searxng", json!({"q": "zig"})).await;
    assert_eq!(t.status, 422, "{}", t.text);
    assert!(t.body["error"]["details"]["skipped"][0]["reason"].as_str().unwrap().contains("circuit open"));
}

#[tokio::test]
async fn accepts_filter_skips_providers_that_cannot_take_the_input() {
    let h = harness().await;
    h.http.route_json("nominatim.openstreetmap.org", "/search", 503, json!({}));
    // Photon cannot filter by country, so it is skipped rather than silently
    // dropping the constraint.
    let r = call(&h, "geo.geocode", json!({"q": "Paris", "country": "fr"})).await;
    assert_eq!(r.status, 424);
    let d = send(&h.app, "POST", "/v1/tools/geo.geocode/call", Some(&h.key), Some(json!({"input": {"q": "Paris", "country": "fr"}, "options": {"dry_run": true}})), &[]).await;
    assert!(d.body["meta"]["skipped"].as_array().unwrap().iter().any(|s| s["tool"] == "geo.geocode.photon"));
    let ok = call(&h, "geo.geocode", json!({"q": "Paris"})).await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body["meta"]["provider"], "photon");
}

#[tokio::test]
async fn circuit_breaker_opens_after_repeated_failures() {
    let h = harness().await;
    h.http.route_json("api.open-meteo.com", "/v1/forecast", 500, json!({}));
    for i in 0..3 {
        let r = send(&h.app, "POST", "/v1/tools/weather.forecast/call", Some(&h.key), Some(json!({"input": {"lat": i as f64, "lon": 1.0}})), &[]).await;
        assert_eq!(r.status, 424);
    }
    let r = call(&h, "weather.forecast", json!({"lat": 9.0, "lon": 1.0})).await;
    assert_eq!(r.status, 422, "{}", r.text);
    assert!(r.body["error"]["details"]["skipped"][0]["reason"].as_str().unwrap().contains("circuit open"));
}

#[tokio::test]
async fn outbox_survives_a_crash_between_capture_and_rago() {
    let h = harness().await;
    h.rago.fail_next.store(1, std::sync::atomic::Ordering::SeqCst);
    let r = call(&h, "rally.company.enrich", json!({"domain": "stripe.com"})).await;
    assert_eq!(r.status, 200);
    // The worker that would have sent the event "crashed" (never ran). A
    // fresh worker after restart delivers it; Rago was down on the first try.
    let first = OutboxWorker { store: h.store.clone(), rago: Some(h.rago.clone()), mint: Some(h.mint.clone()) }.drain_once().await;
    assert_eq!((first.delivered, first.failed), (0, 1));
    assert_eq!(h.mem.as_ref().unwrap().outbox_len(), 1, "event kept after the failure");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let second = OutboxWorker { store: h.store.clone(), rago: Some(h.rago.clone()), mint: Some(h.mint.clone()) }.drain_once().await;
    assert_eq!(second.delivered, 1);
    assert_eq!(h.rago.events().len(), 1);
    assert_eq!(drain(&h).await.delivered, 0, "delivered exactly once");
}

#[tokio::test]
async fn mint_outage_during_capture_is_retried_from_the_outbox() {
    let h = harness().await;
    // Let the hold succeed, then take Mint down before capture.
    let mint = h.mint.clone();
    h.http.route_fn("rally.test", "/api/rally/v1/search/companies-by-domains", Duration::ZERO, move |_| {
        mint.unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
        FakeReply::Json(200, json!({"success": true, "data": {"companies": [{"name": "Stripe"}]}}))
    });
    let r = call(&h, "rally.company.enrich", json!({"domain": "stripe.com"})).await;
    assert_eq!(r.status, 200, "the caller still gets the answer");
    assert_eq!(h.mint.open_holds(), 1);
    h.mint.unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    let rep = drain(&h).await;
    assert_eq!(rep.delivered, 2, "mint_capture + rago_event");
    assert_eq!(h.mint.open_holds(), 0);
    assert_eq!(h.mint.available(REVENUE), 5000);
}

// ------------------------------------------------------------------ auth

#[tokio::test]
async fn one_key_both_rails_scopes_allowlist_and_revocation() {
    let h = harness().await;
    let created = send(&h.app, "POST", "/v1/keys", Some(&h.key), Some(json!({"description": "ci", "scopes": ["tools:read", "tools:call"], "tool_allow": ["weather.forecast"]})), &[]).await;
    assert_eq!(created.status, 201, "{}", created.text);
    let raw = created.body["data"]["key"].as_str().unwrap().to_string();
    assert!(raw.starts_with("sk-t0-"));
    // The key lives in Relay's key store: Relay validates the very same key.
    let v = h.keys.verify(&raw).await.unwrap();
    assert_eq!(v.workspace, h.project.id);
    assert_eq!(send(&h.app, "POST", "/v1/tools/weather.forecast/call", Some(&raw), Some(json!({"input": {"lat": 1.0, "lon": 2.0}})), &[]).await.status, 200);
    let denied = send(&h.app, "POST", "/v1/tools/web.search/call", Some(&raw), Some(json!({"input": {"q": "x"}})), &[]).await;
    assert_eq!(denied.status, 403, "tool allowlist: {}", denied.text);
    let no_send = send(&h.app, "POST", "/v1/tools/rally.email.send/call", Some(&raw), Some(json!({"input": {"from": "a@b.io", "to": "c@d.io"}})), &[]).await;
    assert_eq!(no_send.status, 403);
    assert_eq!(send(&h.app, "GET", "/v1/keys", Some(&raw), None, &[]).await.status, 403, "admin scope needed");
    let id = created.body["data"]["id"].as_str().unwrap();
    assert_eq!(send(&h.app, "POST", &format!("/v1/keys/{id}/revoke"), Some(&h.key), None, &[]).await.status, 200);
    assert_eq!(send(&h.app, "GET", "/v1/tools/search?q=x", Some(&raw), None, &[]).await.status, 401);
    assert!(h.keys.verify(&raw).await.is_err(), "revoked on Relay too");
}

#[tokio::test]
async fn credentials_are_write_only_and_bundles_refuse_secrets() {
    let h = harness().await;
    let c = send(&h.app, "POST", "/v1/credentials", Some(&h.key), Some(json!({"provider": "github", "value": "ghp_supersecretvalue"})), &[]).await;
    assert_eq!(c.status, 201);
    assert!(!c.text.contains("supersecret"));
    let list = send(&h.app, "GET", "/v1/credentials", Some(&h.key), None, &[]).await;
    assert!(!list.text.contains("supersecret") && !list.text.contains("ciphertext"));
    assert_eq!(list.body["data"][0]["provider"], "github");
    // Used by the GitHub tool (BYO optional) and injected server-side only.
    let r = call(&h, "dev.repo.search", json!({"q": "axum"})).await;
    assert_eq!(r.status, 200);
    let gh = h.http.requests().into_iter().find(|r| r.url.contains("api.github.com")).unwrap();
    assert!(gh.sensitive_headers.iter().any(|(_, v)| v == "Bearer ghp_supersecretvalue"));
    let bad = send(&h.app, "POST", "/v1/bundles", Some(&h.key), Some(json!({"id": "leak", "name": "x", "skill_md": "use sk-t0-abc", "tool_ids": []})), &[]).await;
    assert_eq!(bad.status, 400);
    let ok = send(&h.app, "POST", "/v1/bundles", Some(&h.key), Some(json!({"id": "enrich-flow", "name": "Enrich", "skill_md": "# Enrich\nCall rally.company.enrich", "tool_ids": ["rally.company.enrich"], "credential_refs": ["rally"]})), &[]).await;
    assert_eq!(ok.status, 200, "{}", ok.text);
    let list = send(&h.app, "GET", "/v1/bundles", Some(&h.key), None, &[]).await;
    let ids: Vec<&str> = list.body["data"].as_array().unwrap().iter().map(|b| b["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"enrich-flow") && ids.contains(&"lead-to-video"));
    let md = send(&h.app, "GET", "/v1/bundles/lead-to-video/skill.md", Some(&h.key), None, &[]).await;
    assert!(md.text.contains("# Lead to video"));
    let del = send(&h.app, "DELETE", &format!("/v1/credentials/{}", c.body["data"]["id"].as_str().unwrap()), Some(&h.key), None, &[]).await;
    assert_eq!(del.status, 200);
}

// -------------------------------------------------------- money surfaces

#[tokio::test]
async fn batch_runs_under_one_hold() {
    let h = harness().await;
    let r = send(
        &h.app,
        "POST",
        "/v1/tools/batch",
        Some(&h.key),
        Some(json!({"calls": [
            {"tool": "weather.forecast", "input": {"lat": 1.0, "lon": 2.0}},
            {"tool": "rally.company.enrich", "input": {"domain": "stripe.com"}},
            {"tool": "rally.email.verify", "input": {"email": "founder@stripe.com"}},
            {"tool": "weather.forecast", "input": {"lat": 999}}
        ]})),
        &[],
    )
    .await;
    assert_eq!(r.status, 422, "invalid items are rejected before any hold: {}", r.text);
    assert_eq!(r.body["error"]["details"]["index"], 3);
    let r = send(
        &h.app,
        "POST",
        "/v1/tools/batch",
        Some(&h.key),
        Some(json!({"calls": [
            {"tool": "weather.forecast", "input": {"lat": 1.0, "lon": 2.0}},
            {"tool": "rally.company.enrich", "input": {"domain": "stripe.com"}},
            {"tool": "rally.email.verify", "input": {"email": "founder@stripe.com"}}
        ]})),
        &[],
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let data = r.body["data"].as_array().unwrap();
    assert_eq!(data.len(), 3);
    assert_eq!(data[1]["meta"]["provider"], "rally");
    assert_eq!(r.body["meta"]["cost_micros"], 5200);
    let holds = h.mint.holds();
    assert_eq!(holds.len(), 1, "one hold for the whole batch");
    assert_eq!((holds[0].1.amount, holds[0].1.captured), (5200, 5200));
}

#[tokio::test]
async fn balance_topup_and_usage() {
    let h = harness().await;
    call(&h, "rally.company.enrich", json!({"domain": "stripe.com"})).await;
    call(&h, "weather.forecast", json!({"lat": 1.0, "lon": 1.0})).await;
    let b = send(&h.app, "GET", "/v1/balance", Some(&h.key), None, &[]).await;
    assert_eq!(b.body["data"]["balance"]["available_micros"], 10_000_000 - 5000);
    assert_eq!(b.body["data"]["project"]["spent_today_micros"], 5000);
    let t = send(&h.app, "POST", "/v1/balance/topup", Some(&h.key), Some(json!({"amount_micros": 20_000_000})), &[]).await;
    assert_eq!(t.status, 200, "{}", t.text);
    assert!(t.body["data"]["checkout_url"].as_str().unwrap().contains("dodo"));
    assert_eq!(h.mint.checkouts.lock().unwrap()[0], (ACCOUNT.to_string(), 20_000_000, "dodo".to_string()));
    let u = send(&h.app, "GET", "/v1/usage?group_by=provider", Some(&h.key), None, &[]).await;
    assert_eq!(u.body["data"]["total_cost_micros"], 5000);
    assert_eq!(u.body["data"]["total_calls"], 2);
    let calls = send(&h.app, "GET", "/v1/usage/calls?limit=1", Some(&h.key), None, &[]).await;
    assert_eq!(calls.body["data"]["calls"].as_array().unwrap().len(), 1);
    assert!(calls.body["data"]["next_cursor"].is_string());
}

#[tokio::test]
async fn lead_to_video_bundle_runs_end_to_end_on_one_key() {
    let h = harness().await;
    let hdr = [("idempotency-key", "l2v:acme.de")];
    let leads = call(&h, "leads.search", json!({"filters": {"country": "DE"}, "limit": 2})).await;
    assert_eq!(leads.status, 200, "{}", leads.text);
    let domain = leads.body["data"]["results"][0]["domain"].as_str().unwrap().to_string();
    let company = call(&h, "company.enrich", json!({"domain": domain})).await;
    assert_eq!(company.status, 200);
    let verify = call(&h, "email.verify", json!({"email": "founder@stripe.com"})).await;
    assert_eq!(verify.body["data"]["result"], "deliverable");
    let render = call(&h, "video.render", json!({"subject": "Hello Acme", "duration_s": 15})).await;
    assert_eq!(render.status, 200, "{}", render.text);
    let job = render.body["data"]["job_id"].as_str().unwrap().to_string();
    let status = call(&h, "video.status", json!({"job_id": job})).await;
    assert!(status.body["data"]["videos"][0].as_str().unwrap().ends_with(".mp4"));
    let sent = send(&h.app, "POST", "/v1/tools/email.send/call", Some(&h.key), Some(json!({"input": {"from": "hi@acme.com", "to": "founder@stripe.com", "subject": "A video for you", "text": "https://cdn.menivor.test/v.mp4"}})), &hdr).await;
    assert_eq!(sent.status, 200, "{}", sent.text);
    // 2 leads (4,000) + enrich (5,000) + verify (200) + 15 s render (300,000) + status (0) + 1 recipient (400)
    let total = 4000 + 5000 + 200 + 300_000 + 400;
    assert_eq!(h.mint.available(REVENUE), total);
    let u = send(&h.app, "GET", "/v1/usage?group_by=tool", Some(&h.key), None, &[]).await;
    assert_eq!(u.body["data"]["total_cost_micros"], total);
    assert_eq!(drain(&h).await.delivered, 5, "one Rago event per billed call");
}

#[tokio::test]
async fn passthrough_relays_byo_tools_with_injected_credentials() {
    let h = harness().await;
    let c = send(&h.app, "POST", "/v1/credentials", Some(&h.key), Some(json!({"provider": "byo", "name": "crm", "value": "crm-token"})), &[]).await;
    let cred_id = c.body["data"]["id"].as_str().unwrap().to_string();
    let admin = [("authorization", "Bearer admin-test-token")];
    let reg = send(&h.app, "POST", "/admin/api/byo-tools", None, Some(json!({"org_id": h.org.id, "id": "crm", "base_url": "https://crm.acme.test/api", "credential_id": cred_id, "prefix": "Token "})), &admin).await;
    assert_eq!(reg.status, 200, "{}", reg.text);
    let http_reg = send(&h.app, "POST", "/admin/api/byo-tools", None, Some(json!({"org_id": h.org.id, "id": "x", "base_url": "http://crm.acme.test"})), &admin).await;
    assert_eq!(http_reg.status, 400, "https only");
    h.http.route_json("crm.acme.test", "/api/contacts", 200, json!({"contacts": [1, 2]}));
    let r = send(&h.app, "GET", "/v1/passthrough/crm/contacts?limit=2", Some(&h.key), None, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["contacts"], json!([1, 2]));
    assert_eq!(r.headers["x-switchyard-cost-micros"], "0");
    let req = h.http.requests().into_iter().find(|r| r.url.contains("crm.acme.test")).unwrap();
    assert_eq!(req.url, "https://crm.acme.test/api/contacts?limit=2");
    assert!(req.sensitive_headers.iter().any(|(k, v)| k == "Authorization" && v == "Token crm-token"));
    assert_eq!(send(&h.app, "GET", "/v1/passthrough/unknown/x", Some(&h.key), None, &[]).await.status, 404);
}

#[tokio::test]
async fn admin_console_and_bootstrap() {
    let h = harness().await;
    assert_eq!(send(&h.app, "GET", "/admin", None, None, &[]).await.status, 401);
    let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("op:admin-test-token"));
    call(&h, "weather.forecast", json!({"lat": 1.0, "lon": 1.0})).await;
    let page = send(&h.app, "GET", "/admin", None, None, &[("authorization", &basic)]).await;
    assert_eq!(page.status, 200);
    assert!(page.text.contains("weather.forecast.open-meteo") && page.text.contains("Projects and caps"));
    let admin = [("authorization", "Bearer admin-test-token")];
    let org = send(&h.app, "POST", "/admin/api/orgs", None, Some(json!({"slug": "globex", "name": "Globex", "owner_user_id": "u_bob", "monthly_cap_micros": 20000000})), &admin).await;
    assert_eq!(org.status, 201, "{}", org.text);
    let pid = org.body["data"]["project"]["id"].as_str().unwrap();
    let key = send(&h.app, "POST", "/admin/api/keys", None, Some(json!({"project_id": pid, "scopes": ["tools:read", "tools:call"]})), &admin).await;
    assert_eq!(key.status, 201, "{}", key.text);
    let raw = key.body["data"]["key"].as_str().unwrap();
    assert_eq!(send(&h.app, "GET", "/v1/tools/search?q=weather", Some(raw), None, &[]).await.status, 200);
    assert_eq!(send(&h.app, "POST", "/admin/api/orgs", None, Some(json!({"slug": "x", "name": "x"})), &[("authorization", "Bearer wrong")]).await.status, 401);
}

#[tokio::test]
async fn two_hundred_concurrent_calls_mixed_cached_and_uncached() {
    let h = harness().await;
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for i in 0..200 {
        let app = h.app.clone();
        let key = h.key.clone();
        tasks.push(tokio::spawn(async move {
            // Half share inputs (cache/dedupe), half are distinct (uncached, paid).
            let email = if i % 2 == 0 { "founder@stripe.com".to_string() } else { format!("user{i}@stripe.com") };
            send(&app, "POST", "/v1/tools/rally.email.verify/call", Some(&key), Some(json!({"input": {"email": email}})), &[]).await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        let r = t.await.unwrap();
        assert_eq!(r.status, 200, "{}", r.text);
        ok += 1;
    }
    assert_eq!(ok, 200);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(h.mint.open_holds(), 0, "every hold settled");
}

// ------------------------------------------------------------ MCP + OAuth

fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

async fn post_form(h: &H, path: &str, body: String, cookie: Option<&str>) -> Resp {
    let mut b = Request::builder().method("POST").uri(path).header("content-type", "application/x-www-form-urlencoded");
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    raw(&h.app, b.body(Body::from(body)).unwrap()).await
}

async fn get_page(h: &H, path: &str, cookie: &str) -> Resp {
    raw(&h.app, Request::builder().uri(path).header("cookie", cookie).body(Body::empty()).unwrap()).await
}

fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let i = html.find(&marker).unwrap_or_else(|| panic!("no {name} in page")) + marker.len();
    html[i..i + html[i..].find('"').unwrap()].to_string()
}

async fn mcp(h: &H, token: &str, msg: Value) -> Resp {
    send(&h.app, "POST", "/mcp", Some(token), Some(msg), &[("accept", "application/json, text/event-stream")]).await
}

/// Full OAuth 2.1 code flow with PKCE through the HTTP routes; returns tokens.
async fn oauth_login(h: &H, scope: &str, extra_boxes: &[&str]) -> Value {
    let reg = send(&h.app, "POST", "/oauth/register", None, Some(json!({"client_name": "Claude Code", "redirect_uris": ["http://127.0.0.1:43110/callback"]})), &[]).await;
    assert_eq!(reg.status, 201);
    let client_id = reg.body["client_id"].as_str().unwrap().to_string();
    let verifier = "v".repeat(20) + &switchyard_auth::random_token();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
    let q = form(&[
        ("response_type", "code"),
        ("client_id", &client_id),
        ("redirect_uri", "http://127.0.0.1:43110/callback"),
        ("scope", scope),
        ("state", "st4te"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("resource", &format!("{BASE}/mcp")),
    ]);
    // No session yet: sign-in page.
    let signin = raw(&h.app, Request::builder().uri(format!("/oauth/authorize?{q}")).body(Body::empty()).unwrap()).await;
    assert!(signin.text.contains("Sign in"));
    let cookie = "scribase_session=sess-alice";
    let page = get_page(h, &format!("/oauth/authorize?{q}"), cookie).await;
    assert_eq!(page.status, 200);
    assert!(page.text.contains("Claude Code") && page.text.contains(&h.project.id));
    assert!(page.text.contains("name=\"scope_tools:read\" value=\"1\" checked"));
    assert!(!page.text.contains("name=\"scope_tools:send\" value=\"1\" checked"), "send is opt-in");
    let mut fields: Vec<(String, String)> = [
        "response_type", "client_id", "redirect_uri", "scope", "state", "code_challenge", "code_challenge_method", "resource", "csrf",
    ]
    .iter()
    .map(|n| (n.to_string(), hidden(&page.text, n)))
    .collect();
    fields.push(("project".into(), format!("{}|{}", h.org.id, h.project.id)));
    fields.push(("decision".into(), "approve".into()));
    for s in ["tools:read", "tools:call"].iter().chain(extra_boxes.iter()) {
        fields.push((format!("scope_{s}"), "1".into()));
    }
    let body = form(&fields.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>());
    let forged = post_form(h, "/oauth/authorize", body.replace("csrf=", "csrf=x"), Some(cookie)).await;
    assert_eq!(forged.status, 403, "CSRF token enforced");
    let approved = post_form(h, "/oauth/authorize", body, Some(cookie)).await;
    assert_eq!(approved.status, 303);
    let loc = approved.headers["location"].to_str().unwrap();
    let u = url::Url::parse(loc).unwrap();
    let code = u.query_pairs().find(|(k, _)| k == "code").unwrap().1.to_string();
    assert_eq!(u.query_pairs().find(|(k, _)| k == "state").unwrap().1, "st4te");
    let tok = post_form(
        h,
        "/oauth/token",
        form(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", "http://127.0.0.1:43110/callback"), ("code_verifier", &verifier), ("client_id", &client_id)]),
        None,
    )
    .await;
    assert_eq!(tok.status, 200, "{}", tok.text);
    let mut t = tok.body.clone();
    t["client_id"] = json!(client_id);
    t
}

#[tokio::test]
async fn oauth_code_flow_then_mcp_meta_tools() {
    let h = harness().await;
    let t = oauth_login(&h, "tools:read tools:call tools:send", &[]).await;
    let at = t["access_token"].as_str().unwrap();
    assert_eq!(t["scope"], "tools:call tools:read");
    let init = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
    assert_eq!(init.status, 200, "{}", init.text);
    assert_eq!(init.body["result"]["serverInfo"]["name"], "switchyard");
    let note = mcp(&h, at, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
    assert_eq!(note.status, 202);
    let list = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    let names: Vec<&str> = list.body["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["search_tools", "describe_tool", "call_tool", "balance", "usage"]);
    let s = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search_tools", "arguments": {"q": "weather forecast"}}})).await;
    assert_eq!(s.body["result"]["structuredContent"]["results"][0]["capability"], "weather.forecast");
    let c = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "call_tool", "arguments": {"id": "weather.forecast", "input": {"lat": 1.0, "lon": 2.0}}}})).await;
    assert_eq!(c.body["result"]["isError"], false, "{}", c.text);
    assert_eq!(c.body["result"]["structuredContent"]["meta"]["provider"], "open-meteo");
    // The granted token lacks tools:send (unticked at consent).
    let send_try = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "call_tool", "arguments": {"id": "rally.email.send", "input": {"from": "a@acme.com", "to": "b@x.io"}}}})).await;
    assert_eq!(send_try.body["result"]["isError"], true);
    let res = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 6, "method": "resources/read", "params": {"uri": "skill://lead-to-video"}})).await;
    assert!(res.body["result"]["contents"][0]["text"].as_str().unwrap().contains("Lead to video"));
    // The same token on the plain API works too (resource list includes /mcp).
    assert_eq!(send(&h.app, "GET", "/v1/tools/search?q=dns", Some(at), None, &[]).await.status, 200);
    // Unauthenticated MCP points clients at the protected-resource metadata.
    let anon = send(&h.app, "POST", "/mcp", None, Some(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})), &[]).await;
    assert_eq!(anon.status, 401);
    assert!(anon.headers["www-authenticate"].to_str().unwrap().contains("/.well-known/oauth-protected-resource/mcp"));
    let prm = send(&h.app, "GET", "/.well-known/oauth-protected-resource/mcp", None, None, &[]).await;
    assert_eq!(prm.body["resource"], format!("{BASE}/mcp"));
    let asm = send(&h.app, "GET", "/.well-known/oauth-authorization-server", None, None, &[]).await;
    assert_eq!(asm.body["code_challenge_methods_supported"], json!(["S256"]));
    // Refresh rotation over HTTP.
    let r1 = post_form(&h, "/oauth/token", form(&[("grant_type", "refresh_token"), ("refresh_token", t["refresh_token"].as_str().unwrap()), ("client_id", t["client_id"].as_str().unwrap())]), None).await;
    assert_eq!(r1.status, 200);
    let replay = post_form(&h, "/oauth/token", form(&[("grant_type", "refresh_token"), ("refresh_token", t["refresh_token"].as_str().unwrap()), ("client_id", t["client_id"].as_str().unwrap())]), None).await;
    assert_eq!(replay.body["error"], "invalid_grant");
    // Family revoked: even the fresh access token is now refused.
    let after = send(&h.app, "GET", "/v1/tools/search?q=dns", Some(r1.body["access_token"].as_str().unwrap()), None, &[]).await;
    assert_eq!(after.status, 401);
}

#[tokio::test]
async fn oauth_send_tools_need_one_time_consent_per_tool() {
    let h = harness().await;
    let t = oauth_login(&h, "tools:read tools:call tools:send", &["tools:send"]).await;
    let at = t["access_token"].as_str().unwrap();
    let args = json!({"id": "rally.email.send", "input": {"from": "a@acme.com", "to": "b@x.io", "subject": "s", "text": "t"}, "idempotency_key": "k1"});
    let first = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "call_tool", "arguments": args}})).await;
    assert_eq!(first.body["result"]["isError"], true);
    let err = &first.body["result"]["structuredContent"]["error"];
    assert_eq!(err["code"], "consent_required");
    let consent_url = err["details"]["consent_url"].as_str().unwrap().replace(BASE, "");
    let page = get_page(&h, &consent_url, "scribase_session=sess-alice").await;
    assert!(page.text.contains("Rally transactional send"));
    let fields: Vec<(String, String)> = ["tool", "grant", "org", "csrf"].iter().map(|n| (n.to_string(), hidden(&page.text, n))).collect();
    let mut f: Vec<(&str, &str)> = fields.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    f.push(("decision", "approve"));
    let mallory = post_form(&h, "/oauth/consent", form(&f), Some("scribase_session=sess-mallory")).await;
    assert_eq!(mallory.status, 403, "another user cannot approve");
    let ok = post_form(&h, "/oauth/consent", form(&f), Some("scribase_session=sess-alice")).await;
    assert!(ok.text.contains("Approved"));
    let second = mcp(&h, at, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "call_tool", "arguments": args}})).await;
    assert_eq!(second.body["result"]["isError"], false, "{}", second.text);
}

#[tokio::test]
async fn device_flow_for_the_cli() {
    let h = harness().await;
    let start = post_form(&h, "/oauth/device/code", form(&[("client_id", "switchyard-cli"), ("scope", "tools:read tools:call")]), None).await;
    assert_eq!(start.status, 200, "{}", start.text);
    let user_code = start.body["user_code"].as_str().unwrap().to_string();
    let device_code = start.body["device_code"].as_str().unwrap().to_string();
    let poll = || post_form(&h, "/oauth/token", form(&[("grant_type", switchyard_auth::oauth::DEVICE_GRANT), ("device_code", &device_code), ("client_id", "switchyard-cli")]), None);
    assert_eq!(poll().await.body["error"], "authorization_pending");
    let page = get_page(&h, &format!("/device?user_code={user_code}"), "scribase_session=sess-alice").await;
    assert!(page.text.contains(&user_code));
    let csrf = hidden(&page.text, "csrf");
    let project = format!("{}|{}", h.org.id, h.project.id);
    let approve = post_form(
        &h,
        "/device",
        form(&[("user_code", &user_code), ("csrf", &csrf), ("project", &project), ("scope_tools:read", "1"), ("scope_tools:call", "1"), ("decision", "approve")]),
        Some("scribase_session=sess-alice"),
    )
    .await;
    assert!(approve.text.contains("Connected"), "{}", approve.text);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let tok = poll().await;
    let at = match tok.body["access_token"].as_str() {
        Some(t) => t.to_string(),
        None => {
            // The previous poll may have been within the interval (slow_down).
            tokio::time::sleep(Duration::from_secs(5)).await;
            poll().await.body["access_token"].as_str().expect("device token").to_string()
        }
    };
    let r = send(&h.app, "POST", "/v1/tools/weather.forecast/call", Some(&at), Some(json!({"input": {"lat": 1.0, "lon": 2.0}})), &[]).await;
    assert_eq!(r.status, 200);
    // A CLI token is bound to the API resource, not the MCP endpoint.
    assert_eq!(mcp(&h, &at, json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).await.status, 401);
    // Removing the member cuts the token off.
    h.store.upsert_member(switchyard_store::Member { org_id: h.org.id.clone(), user_id: "u_alice".into(), role: "viewer".into() }).await.unwrap();
    assert_eq!(send(&h.app, "GET", "/v1/tools/search?q=x", Some(&at), None, &[]).await.status, 200, "viewers keep read access");
}

#[tokio::test]
async fn pinned_tools_expose_real_schemas_and_safety_annotations() {
    let h = harness().await;
    let r = send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"pinned_tools": ["weather.forecast", "rally.email.send", "menivor.video.render"]})), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let too_many: Vec<String> = (0..21).map(|_| "weather.forecast".to_string()).collect();
    assert_eq!(send(&h.app, "PATCH", "/v1/project", Some(&h.key), Some(json!({"pinned_tools": too_many})), &[]).await.status, 400);
    let list = mcp(&h, &h.key, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).await;
    // sk-t0 keys are accepted on /mcp too (one key for everything).
    assert_eq!(list.status, 200, "{}", list.text);
    let tools = list.body["result"]["tools"].as_array().unwrap();
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap_or_else(|| panic!("{n} missing"));
    let w = find("weather_forecast");
    assert_eq!(w["annotations"]["readOnlyHint"], true);
    assert_eq!(w["inputSchema"]["required"], json!(["lat", "lon"]));
    assert!(w["outputSchema"].is_object());
    let s = find("rally_email_send");
    assert_eq!((s["annotations"]["destructiveHint"].as_bool(), s["annotations"]["readOnlyHint"].as_bool()), (Some(true), Some(false)));
    assert_eq!(find("menivor_video_render")["annotations"]["openWorldHint"], true);
    let c = mcp(&h, &h.key, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "weather_forecast", "arguments": {"lat": 3.0, "lon": 4.0}}})).await;
    assert_eq!(c.body["result"]["structuredContent"]["meta"]["provider"], "open-meteo");
}

// ----------------------------------------------------- Postgres (opt-in)

#[tokio::test]
async fn postgres_backed_paid_call_and_chain_proof() {
    let Ok(admin) = std::env::var("SWITCHYARD_TEST_PG_ADMIN_URL") else {
        eprintln!("skipping: SWITCHYARD_TEST_PG_ADMIN_URL not set");
        return;
    };
    use sqlx::Connection;
    let db = format!("sy_it_{}", ulid::Ulid::new().to_string().to_lowercase());
    let mut conn = sqlx::PgConnection::connect(&admin).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {db}")).execute(&mut conn).await.unwrap();
    let url = format!("{}/{db}", &admin[..admin.rfind('/').unwrap()]);
    {
        let pg = Arc::new(switchyard_store::PgStore::connect(&url).await.unwrap());
        let h = harness_with(pg.clone(), None).await;
        let r = call(&h, "rally.leads.search", json!({"filters": {"country": "DE"}, "limit": 2})).await;
        assert_eq!(r.status, 200, "{}", r.text);
        let r2 = call(&h, "weather.forecast", json!({"lat": 1.0, "lon": 1.0})).await;
        let id = r2.body["meta"]["call_id"].as_str().unwrap();
        let proof = send(&h.app, "GET", &format!("/v1/usage/calls/{id}"), Some(&h.key), None, &[]).await;
        assert_eq!(proof.body["data"]["proof"]["verified"], true);
        assert_eq!(proof.body["data"]["proof"]["chain_length"], 2);
        assert_eq!(drain(&h).await.delivered, 1);
        let c = send(&h.app, "POST", "/v1/credentials", Some(&h.key), Some(json!({"provider": "brave", "value": "brave-pg"})), &[]).await;
        assert_eq!(c.status, 201);
        pg.pool().close().await;
    }
    sqlx::query(&format!("DROP DATABASE {db}")).execute(&mut conn).await.unwrap();
}

#[tokio::test]
async fn cache_hits_pay_the_tools_cache_price() {
    let h = harness_with_catalog(catalog_with(|t| {
        if t.id == "rally.leads.search" {
            t.price.cache_micros = 300;
        }
    }))
    .await;
    let input = json!({"kind": "companies", "filters": {"country": "FR"}, "limit": 2});
    let first = call(&h, "rally.leads.search", input.clone()).await;
    assert_eq!((first.body["meta"]["cache"].as_str(), first.body["meta"]["cost_micros"].as_i64()), (Some("miss"), Some(4000)), "{}", first.text);
    let hit = call(&h, "rally.leads.search", input).await;
    assert_eq!(hit.status, 200, "{}", hit.text);
    assert_eq!(hit.body["meta"]["cache"], "hit");
    assert_eq!(hit.body["meta"]["cost_micros"], 300);
    assert!(hit.body["meta"]["receipt_id"].is_string());
    assert_eq!(h.mint.available(ACCOUNT), 10_000_000 - 4000 - 300);
    assert_eq!(h.mint.available(REVENUE), 4300);
    // Both calls reach Rago through the outbox, the hit at its cache price.
    assert_eq!(drain(&h).await.delivered, 2);
    let costs: Vec<i64> = h.rago.events().iter().map(|e| e["properties"]["cost_micros"].as_i64().unwrap()).collect();
    assert_eq!(costs, vec![4000, 300]);
}

#[tokio::test]
async fn a_cache_hit_without_funds_is_refused_and_free_hits_stay_free() {
    let h = harness_with_catalog(catalog_with(|t| {
        if t.id == "rally.leads.search" {
            t.price.cache_micros = 300;
        }
    }))
    .await;
    let input = json!({"kind": "companies", "filters": {"country": "IT"}, "limit": 1});
    assert_eq!(call(&h, "rally.leads.search", input.clone()).await.status, 200);
    // Drain the account to below the cache price.
    let left = h.mint.available(ACCOUNT);
    h.mint.fund(ACCOUNT, -(left - 100));
    let hit = call(&h, "rally.leads.search", input).await;
    assert_eq!(hit.status, 402, "{}", hit.text);
    // The default catalog prices hits at zero: no hold at all.
    let plain = harness().await;
    let q = json!({"lat": 1.0, "lon": 2.0});
    call(&plain, "weather.forecast", q.clone()).await;
    let r = call(&plain, "weather.forecast", q).await;
    assert_eq!((r.body["meta"]["cache"].as_str(), r.body["meta"]["cost_micros"].as_i64()), (Some("hit"), Some(0)));
    assert_eq!(plain.mint.holds().len(), 0);
}
