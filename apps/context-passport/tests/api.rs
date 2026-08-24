//! Server integration tests: the REAL router / handlers / auth / service logic
//! with an in-memory store injected (no database). The Postgres store path is
//! compiled here and exercised live at deploy time.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use context_passport::server::{build_router, AppState};
use context_passport::service::PassportService;
use context_passport::store::MemoryStore;
use serde_json::{json, Value};
use tower::ServiceExt;

const API_KEY: &str = "test-secret-key";

fn app() -> Router {
    let state = AppState {
        service: PassportService::new(Arc::new(MemoryStore::default())),
        api_key: Arc::from(API_KEY),
    };
    build_router(state)
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
            "idempotency_key": "brand-hero-1",
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

    let (status, _) = send(
        app,
        "POST",
        &format!("/v1/passports/{id}/lock"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    id
}

#[tokio::test]
async fn health_is_public() {
    let (status, _) = send(&app(), "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn create_is_idempotent_over_http() {
    let app = app();
    let body = json!({
        "idempotency_key": "same-key",
        "kind": "brand",
        "owner": "acme"
    });
    let (first_status, first) =
        send(&app, "POST", "/v1/passports", Some(API_KEY), Some(body.clone())).await;
    let (second_status, second) =
        send(&app, "POST", "/v1/passports", Some(API_KEY), Some(body)).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first["data"]["id"], second["data"]["id"]);
}

#[tokio::test]
async fn resolve_returns_correct_conditioning_per_modality() {
    let app = app();
    let id = create_locked_character(&app).await;

    let (status, image) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "runway", "modality": "image"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(image["data"]["reference_images"], json!(["face_a", "face_b"]));
    assert_eq!(image["data"]["lora_ref"], "lora_y");
    assert!(image["data"]["voice_id"].is_null());

    let (status, voice) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "elevenlabs", "modality": "voice"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(voice["data"]["voice_id"], "eleven-42");

    let (status, video) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "heygen", "modality": "video"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(video["data"]["avatar_ref"], "avatar_x");
    assert_eq!(video["data"]["voice_id"], "eleven-42");
}

#[tokio::test]
async fn locked_version_is_immutable_but_new_version_is_editable() {
    let app = app();
    let id = create_locked_character(&app).await;

    // Editing the locked head is a conflict.
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/v1/passports/{id}"),
        Some(API_KEY),
        Some(json!({"kind": "character", "style": "noir"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Branch a new draft version from the locked one.
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

    // The new draft accepts edits.
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

    // Version 1 is unchanged (still cinematic).
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
async fn provenance_is_recorded_against_a_locked_version() {
    let app = app();
    let id = create_locked_character(&app).await;

    let (status, record) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/generations"),
        Some(API_KEY),
        Some(json!({
            "version": 1,
            "provider": "heygen",
            "modality": "video",
            "output_ref": "r2://out/clip.mp4"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(record["data"]["passport_id"], id);

    let (status, all) = send(
        &app,
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
async fn auth_rejects_bad_keys() {
    let app = app();
    let body = json!({"idempotency_key": "k", "kind": "persona", "owner": "x"});
    let (missing, _) = send(&app, "POST", "/v1/passports", None, Some(body.clone())).await;
    assert_eq!(missing, StatusCode::UNAUTHORIZED);
    let (wrong, _) = send(&app, "POST", "/v1/passports", Some("nope"), Some(body)).await;
    assert_eq!(wrong, StatusCode::UNAUTHORIZED);
}
