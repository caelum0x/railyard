//! Server integration tests: the REAL router / handlers / auth / service with an
//! in-memory store and fake Relay/Mint seams injected (no network, no DB). The
//! Postgres store path is compiled here and exercised live at deploy.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use passport_resolve::Modality;
use passport_server::service::PassportService;
use passport_server::{build_router, AppState};
use passport_store::MemoryStore;
use passport_testkit::{FakeMint, FakeRelay};

const API_KEY: &str = "test-secret-key";

struct Harness {
    app: Router,
    relay: Arc<FakeRelay>,
    mint: Arc<FakeMint>,
    service: PassportService,
}

fn harness() -> Harness {
    let store = Arc::new(MemoryStore::default());
    let relay = Arc::new(FakeRelay::default());
    let mint = Arc::new(FakeMint::default());
    let service = PassportService::new(store.clone(), store, relay.clone(), mint.clone());
    let state = AppState {
        service: service.clone(),
        api_key: Arc::from(API_KEY),
    };
    Harness {
        app: build_router(state),
        relay,
        mint,
        service,
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        builder = builder.header("x-api-key", k);
    }
    let request = if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn create_locked_character(app: &Router) -> String {
    let (status, created) = send(
        app,
        "POST",
        "/v1/passports",
        Some(API_KEY),
        Some(json!({
            "idempotency_key": "hero-1",
            "kind": "character",
            "owner": "menivor",
            "assets": {
                "kind": "character",
                "face_refs": ["face_a", "face_b"],
                "voice_id": "eleven-42",
                "avatar_ref": "avatar_x",
                "lora_ref": "lora_y",
                "style": "cinematic",
                "wardrobe": "denim jacket",
                "personality": "warm, witty"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["data"]["id"].as_str().unwrap().to_string();
    let (status, locked) = send(
        app,
        "POST",
        &format!("/v1/passports/{id}/lock"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(locked["data"]["head"]["status"], "locked");
    // Locking content-addresses the version.
    assert!(locked["data"]["head"]["content_digest"]
        .as_str()
        .unwrap()
        .starts_with("ni:///sha-256;"));
    id
}

#[tokio::test]
async fn health_is_public() {
    let (status, _) = send(&harness().app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn auth_rejects_missing_and_wrong_keys() {
    let app = harness().app;
    let body = json!({"idempotency_key": "k", "kind": "persona", "owner": "x"});
    let (missing, _) = send(&app, "POST", "/v1/passports", None, Some(body.clone())).await;
    assert_eq!(missing, StatusCode::UNAUTHORIZED);
    let (wrong, _) = send(&app, "POST", "/v1/passports", Some("nope"), Some(body)).await;
    assert_eq!(wrong, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_is_idempotent() {
    let app = harness().app;
    let body = json!({"idempotency_key": "same", "kind": "brand", "owner": "acme"});
    let (s1, a) = send(
        &app,
        "POST",
        "/v1/passports",
        Some(API_KEY),
        Some(body.clone()),
    )
    .await;
    let (s2, b) = send(&app, "POST", "/v1/passports", Some(API_KEY), Some(body)).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(a["data"]["id"], b["data"]["id"]);
}

#[tokio::test]
async fn resolve_returns_correct_conditioning_per_modality() {
    let app = harness().app;
    let id = create_locked_character(&app).await;

    let (s, image) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "runway", "modality": "image"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        image["data"]["reference_images"],
        json!(["face_a", "face_b"])
    );
    assert_eq!(image["data"]["lora_ref"], "lora_y");
    assert!(image["data"]["voice_id"].is_null());

    let (s, voice) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "elevenlabs", "modality": "voice"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(voice["data"]["voice_id"], "eleven-42");

    let (s, video) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "heygen", "modality": "video"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(video["data"]["avatar_ref"], "avatar_x");
    assert_eq!(video["data"]["voice_id"], "eleven-42");
}

#[tokio::test]
async fn resolve_fails_closed_when_voice_clone_missing() {
    let app = harness().app;
    // Brand has no voice_id → voice is Family-B, needs an artifact.
    let (status, created) = send(
        &app,
        "POST",
        "/v1/passports",
        Some(API_KEY),
        Some(
            json!({"idempotency_key": "b1", "kind": "brand", "owner": "acme",
                    "assets": {"kind": "brand", "voice_tone": "bold"}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["data"]["id"].as_str().unwrap().to_string();
    send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/lock"),
        Some(API_KEY),
        None,
    )
    .await;

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "elevenlabs", "modality": "voice"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "needs_artifact");
}

#[tokio::test]
async fn locked_version_is_immutable_but_new_version_is_editable() {
    let app = harness().app;
    let id = create_locked_character(&app).await;

    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/v1/passports/{id}"),
        Some(API_KEY),
        Some(json!({"kind": "character", "style": "noir"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, branched) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/versions"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(branched["data"]["head"]["version"], 2);
    assert_eq!(branched["data"]["head"]["status"], "draft");

    let (status, edited) = send(
        &app,
        "PATCH",
        &format!("/v1/passports/{id}"),
        Some(API_KEY),
        Some(json!({"kind": "character", "style": "noir"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["data"]["head"]["assets"]["style"], "noir");

    let (status, v1) = send(
        &app,
        "GET",
        &format!("/v1/passports/{id}/versions/1"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1["data"]["assets"]["style"], "cinematic");
    assert_eq!(v1["data"]["status"], "locked");
}

#[tokio::test]
async fn generate_as_passport_wires_relay_provenance_and_mint() {
    let h = harness();
    let id = create_locked_character(&h.app).await;

    let (status, record) = send(
        &h.app,
        "POST",
        &format!("/v1/passports/{id}/generate"),
        Some(API_KEY),
        Some(json!({"provider": "heygen", "modality": "video"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(record["data"]["passport_id"], id);
    assert!(record["data"]["output_ref"]
        .as_str()
        .unwrap()
        .starts_with("fake://heygen"));
    // Provenance froze the passport content digest and a Mint receipt.
    assert!(record["data"]["passport_content_digest"]
        .as_str()
        .unwrap()
        .starts_with("ni:///sha-256;"));
    assert!(record["data"]["mint_receipt_ref"].as_str().is_some());

    assert_eq!(h.relay.call_count(), 1);
    assert_eq!(h.mint.call_count(), 1);

    let (status, all) = send(
        &h.app,
        "GET",
        &format!("/v1/passports/{id}/generations"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn generate_on_draft_fails_closed_without_calling_relay() {
    let h = harness();
    let (_, created) = send(
        &h.app,
        "POST",
        "/v1/passports",
        Some(API_KEY),
        Some(
            json!({"idempotency_key": "d1", "kind": "character", "owner": "m",
                    "assets": {"kind": "character", "face_refs": ["f"]}}),
        ),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    // Never locked → resolve/generate must refuse and never hit Relay.
    let err = h
        .service
        .generate(&id, "runway", Modality::Image, None)
        .await
        .unwrap_err();
    assert_eq!(err, passport_core::PassportError::VersionNotLocked);
    assert_eq!(h.relay.call_count(), 0);
    assert_eq!(h.mint.call_count(), 0);
}
