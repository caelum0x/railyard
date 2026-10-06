//! Connecting a team's own provider account over OAuth 2 (code + PKCE), and
//! renewing the token before a call when it is about to expire.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use common::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use switchyard_core::OAuthSpec;
use switchyard_server::config::OAuthClient;
use switchyard_store::MemStore;
use switchyard_testkit::FakeReply;

async fn oauth_harness() -> H {
    let catalog = catalog_with_providers(|p| {
        if p.id == "brave" {
            p.oauth = Some(OAuthSpec { authorize_url: "https://auth.brave.test/authorize".into(), token_url: "https://auth.brave.test/token".into(), scopes: vec!["search".into()] });
        }
    });
    let mem = Arc::new(MemStore::new());
    harness_configured(mem.clone(), Some(mem), catalog, |s| {
        s.oauth_clients.insert("brave".into(), OAuthClient { client_id: "cid".into(), client_secret: "csecret".into() });
    })
    .await
}

fn form(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

#[tokio::test]
async fn connect_exchanges_the_code_with_pkce_and_refreshes_before_a_call() {
    let h = oauth_harness().await;
    let challenge = Arc::new(Mutex::new(String::new()));
    let grants = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let challenge = challenge.clone();
        let grants = grants.clone();
        h.http.route_fn("auth.brave.test", "/token", std::time::Duration::ZERO, move |req| {
            let f = form(req.body.as_deref().unwrap_or_default());
            grants.lock().unwrap().push(f["grant_type"].clone());
            if f.get("client_secret").map(String::as_str) != Some("csecret") {
                return FakeReply::Json(401, json!({ "error": "invalid_client" }));
            }
            match f["grant_type"].as_str() {
                "authorization_code" => {
                    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(f["code_verifier"].as_bytes()));
                    if computed != *challenge.lock().unwrap() || f["code"] != "the-code" {
                        return FakeReply::Json(400, json!({ "error": "invalid_grant" }));
                    }
                    // Expires within the refresh margin, so the first call renews it.
                    FakeReply::Json(200, json!({ "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 10, "token_type": "bearer" }))
                }
                "refresh_token" if f["refresh_token"] == "rt-1" => FakeReply::Json(200, json!({ "access_token": "at-2", "expires_in": 3600 })),
                _ => FakeReply::Json(400, json!({ "error": "invalid_grant" })),
            }
        });
    }
    let c = send(&h.app, "POST", "/v1/credentials/oauth/brave/connect", Some(&h.key), Some(json!({ "name": "team brave" })), &[]).await;
    assert_eq!(c.status, 200, "{}", c.text);
    let url = url::Url::parse(c.body["data"]["authorize_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!((url.host_str(), q["client_id"].as_str(), q["code_challenge_method"].as_str()), (Some("auth.brave.test"), "cid", "S256"));
    assert_eq!(q["redirect_uri"], format!("{BASE}/v1/credentials/oauth/brave/callback"));
    *challenge.lock().unwrap() = q["code_challenge"].clone();

    // A tampered state is refused; the real one connects.
    let bad = send(&h.app, "GET", &format!("/v1/credentials/oauth/brave/callback?code=the-code&state={}x", q["state"]), None, None, &[]).await;
    assert_eq!(bad.status, 400);
    let state = url::form_urlencoded::byte_serialize(q["state"].as_bytes()).collect::<String>();
    let cb = send(&h.app, "GET", &format!("/v1/credentials/oauth/brave/callback?code=the-code&state={state}"), None, None, &[]).await;
    assert_eq!(cb.status, 200, "{}", cb.text);
    assert!(cb.text.contains("Connected"));
    let creds = send(&h.app, "GET", "/v1/credentials", Some(&h.key), None, &[]).await;
    let providers: Vec<String> = creds.body["data"].as_array().unwrap().iter().map(|c| c["provider"].as_str().unwrap().to_string()).collect();
    assert!(providers.contains(&"brave".to_string()) && providers.contains(&"brave#refresh".to_string()), "{providers:?}");
    assert!(!creds.text.contains("at-1") && !creds.text.contains("rt-1"), "secrets are write-only");

    // The first call renews the nearly-expired token and uses the new one.
    let r = send(&h.app, "POST", "/v1/tools/web.search.brave/call", Some(&h.key), Some(json!({ "input": { "q": "rust" } })), &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let brave = h.http.requests().into_iter().rev().find(|r| r.url.contains("api.search.brave.com")).unwrap();
    assert!(brave.sensitive_headers.iter().any(|(k, v)| k == "X-Subscription-Token" && v == "at-2"), "{:?}", brave.sensitive_headers);
    assert_eq!(*grants.lock().unwrap(), vec!["authorization_code", "refresh_token"]);
}

#[tokio::test]
async fn connect_needs_a_provider_with_oauth_a_configured_client_and_admin() {
    let h = oauth_harness().await;
    assert_eq!(send(&h.app, "POST", "/v1/credentials/oauth/searxng/connect", Some(&h.key), None, &[]).await.status, 404);
    let plain = harness().await;
    assert_eq!(send(&plain.app, "POST", "/v1/credentials/oauth/brave/connect", Some(&plain.key), None, &[]).await.status, 404, "no oauth block in the shipped catalog");
    let reader = mint_key(&h.keys, h.store.as_ref(), &h.org, &h.project.id, &["tools:read", "tools:call"], None).await;
    assert_eq!(send(&h.app, "POST", "/v1/credentials/oauth/brave/connect", Some(&reader), None, &[]).await.status, 403);
    let denied = send(&h.app, "GET", "/v1/credentials/oauth/brave/callback?error=access_denied", None, None, &[]).await;
    assert_eq!(denied.status, 400);
}
