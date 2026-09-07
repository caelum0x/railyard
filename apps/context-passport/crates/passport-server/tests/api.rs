//! Server integration tests: the REAL router / handlers / auth / service with an
//! in-memory store and fake receipted Relay seam injected (no network, no DB). The
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
use passport_testkit::{FakeRelay, ReplayRelay};

const API_KEY: &str = "test-secret-key";

struct Harness {
    app: Router,
    relay: Arc<FakeRelay>,
    service: PassportService,
}

fn harness() -> Harness {
    let store = Arc::new(MemoryStore::default());
    let relay = Arc::new(FakeRelay::default());
    let service = PassportService::new(store.clone(), store, relay.clone());
    let state = AppState {
        service: service.clone(),
        api_key: Arc::from(API_KEY),
    };
    Harness {
        app: build_router(state),
        relay,
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
    // Provenance froze the passport digest and Relay's signed/correlated Mint evidence.
    assert!(record["data"]["passport_content_digest"]
        .as_str()
        .unwrap()
        .starts_with("ni:///sha-256;"));
    assert!(record["data"]["mint_receipt_ref"].as_str().is_some());
    assert_eq!(
        record["data"]["payment_evidence"]["payer_account"],
        "menivor"
    );
    assert_eq!(
        record["data"]["payment_evidence"]["receipt"]["operation"],
        "hold.capture"
    );

    assert_eq!(h.relay.call_count(), 1);

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
async fn external_generation_attestation_is_locked_receipted_and_idempotent() {
    let h = harness();
    let id = create_locked_character(&h.app).await;
    let body = json!({
        "provider": "runway",
        "modality": "image",
        "version": 1,
        "output_ref": "r2://menivor/assets/hero.png",
        "asset_hash": "sha256:asset",
        "settled_at_ms": 1_787_854_400_000_i64,
        "payment_evidence": {
            "request_id": "11111111-1111-4111-8111-111111111111",
            "payer_account": "menivor",
            "receipt": {
                "sequence": 42,
                "operation": "hold.capture",
                "operation_hash": "op-hash",
                "amount_micros": 990000,
                "receipt_hash": "receipt-hash",
                "signature": "signed-receipt"
            }
        }
    });
    let uri = format!("/v1/passports/{id}/attest");
    let (status, first) = send(&h.app, "POST", &uri, Some(API_KEY), Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first["data"]["passport_id"], id);
    assert_eq!(first["data"]["version"], 1);
    assert_eq!(
        first["data"]["id"],
        "gen_ext_11111111-1111-4111-8111-111111111111"
    );
    assert!(first["data"]["passport_content_digest"]
        .as_str()
        .unwrap()
        .starts_with("ni:///sha-256;"));

    let (status, replay) = send(&h.app, "POST", &uri, Some(API_KEY), Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay, first);

    let (status, listed) = send(
        &h.app,
        "GET",
        &format!("/v1/passports/{id}/generations"),
        Some(API_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);

    let mut conflict = body;
    conflict["output_ref"] = json!("r2://menivor/assets/different.png");
    let (status, error) = send(&h.app, "POST", &uri, Some(API_KEY), Some(conflict)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "idempotency_conflict");
}

#[tokio::test]
async fn external_attestation_rejects_wrong_payer() {
    let h = harness();
    let id = create_locked_character(&h.app).await;
    let (status, error) = send(
        &h.app,
        "POST",
        &format!("/v1/passports/{id}/attest"),
        Some(API_KEY),
        Some(json!({
            "provider": "runway",
            "modality": "image",
            "output_ref": "r2://asset",
            "settled_at_ms": 1_787_854_400_000_i64,
            "payment_evidence": {
                "request_id": "22222222-2222-4222-8222-222222222222",
                "payer_account": "someone-else",
                "receipt": {
                    "sequence": 1,
                    "operation": "hold.capture",
                    "operation_hash": "op",
                    "amount_micros": 1,
                    "receipt_hash": "hash",
                    "signature": "sig"
                }
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "invalid_passport");
}

#[tokio::test]
async fn resolve_targets_explicit_locked_version_and_rejects_draft() {
    let app = harness().app;
    let id = create_locked_character(&app).await; // v1 is locked

    // Branch a new draft head (v2) from the locked v1.
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

    // Explicitly targeting the locked v1 resolves.
    let (status, v1) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "runway", "modality": "image", "version": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1["data"]["version"], 1);
    assert_eq!(v1["data"]["reference_images"], json!(["face_a", "face_b"]));

    // Explicitly targeting the draft v2 fails closed.
    let (status, v2) = send(
        &app,
        "POST",
        &format!("/v1/passports/{id}/resolve"),
        Some(API_KEY),
        Some(json!({"provider": "runway", "modality": "image", "version": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v2["error"]["code"], "version_not_locked");
}

#[tokio::test]
async fn generate_is_idempotent_on_relay_request_id() {
    // Relay models its own request idempotency (same request_id on retry); a
    // retried POST /generate must therefore record exactly one provenance row.
    let store = Arc::new(MemoryStore::default());
    let relay = Arc::new(ReplayRelay::default());
    let service = PassportService::new(store.clone(), store, relay.clone());
    let state = AppState {
        service,
        api_key: Arc::from(API_KEY),
    };
    let app = build_router(state);
    let id = create_locked_character(&app).await;

    let uri = format!("/v1/passports/{id}/generate");
    let body = json!({"provider": "heygen", "modality": "video"});
    let (s1, first) = send(&app, "POST", &uri, Some(API_KEY), Some(body.clone())).await;
    let (s2, second) = send(&app, "POST", &uri, Some(API_KEY), Some(body)).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(s2, StatusCode::CREATED);
    assert_eq!(first["data"]["id"], "gen_req_stable");
    // Byte-identical replay: the retry returns the already-recorded provenance.
    assert_eq!(first, second);

    // Relay was hit on both calls, but only one provenance record exists.
    assert_eq!(relay.call_count(), 2);
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
}
