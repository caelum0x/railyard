//! Axum product API. Handlers stay thin: they authenticate, deserialize, and
//! delegate to `PassportService`. Every route except health checks requires a
//! constant-time `X-API-Key`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tower_http::trace::TraceLayer;

use crate::domain::{
    AssetsPatch, CreatePassport, GenerationRecord, Passport, PassportFilter, PassportVersion,
    PassportView,
};
use crate::error::PassportError;
use crate::resolve::{ConditioningPayload, Modality};
use crate::service::PassportService;

#[derive(Clone)]
pub struct AppState {
    pub service: PassportService,
    pub api_key: Arc<str>,
}

#[derive(Serialize)]
struct Data<T> {
    data: T,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/passports", post(create_passport).get(list_passports))
        .route("/v1/passports/:id", get(get_passport).patch(update_draft))
        .route("/v1/passports/:id/lock", post(lock_passport))
        .route("/v1/passports/:id/versions", post(new_version))
        .route("/v1/passports/:id/versions/:version", get(get_version))
        .route("/v1/passports/:id/resolve", post(resolve_passport))
        .route(
            "/v1/passports/:id/generations",
            post(record_generation).get(list_generations),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn healthz() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn readyz(State(state): State<AppState>) -> Result<StatusCode, PassportError> {
    state.service.ready().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_passport(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreatePassport>,
) -> Result<(StatusCode, Json<Data<PassportView>>), PassportError> {
    authorize(&state, &headers)?;
    let result = state.service.create(request).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(Data { data: result.view })))
}

async fn list_passports(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filter): Query<PassportFilter>,
) -> Result<Json<Data<Vec<Passport>>>, PassportError> {
    authorize(&state, &headers)?;
    let passports = state.service.list(&filter).await?;
    Ok(Json(Data { data: passports }))
}

async fn get_passport(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<PassportView>>, PassportError> {
    authorize(&state, &headers)?;
    let view = state.service.get(&id).await?;
    Ok(Json(Data { data: view }))
}

async fn get_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version)): Path<(String, i64)>,
) -> Result<Json<Data<PassportVersion>>, PassportError> {
    authorize(&state, &headers)?;
    let version = state.service.get_version(&id, version).await?;
    Ok(Json(Data { data: version }))
}

async fn update_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<AssetsPatch>,
) -> Result<Json<Data<PassportView>>, PassportError> {
    authorize(&state, &headers)?;
    let view = state.service.update_draft(&id, &patch).await?;
    Ok(Json(Data { data: view }))
}

async fn lock_passport(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<PassportView>>, PassportError> {
    authorize(&state, &headers)?;
    let view = state.service.lock(&id).await?;
    Ok(Json(Data { data: view }))
}

async fn new_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Data<PassportView>>), PassportError> {
    authorize(&state, &headers)?;
    let view = state.service.new_version(&id).await?;
    Ok((StatusCode::CREATED, Json(Data { data: view })))
}

#[derive(Debug, Deserialize)]
struct ResolveRequest {
    provider: String,
    modality: Modality,
    #[serde(default)]
    version: Option<i64>,
}

async fn resolve_passport(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ResolveRequest>,
) -> Result<Json<Data<ConditioningPayload>>, PassportError> {
    authorize(&state, &headers)?;
    let payload = state
        .service
        .resolve(&id, &request.provider, request.modality, request.version)
        .await?;
    Ok(Json(Data { data: payload }))
}

#[derive(Debug, Deserialize)]
struct GenerationRequest {
    version: i64,
    provider: String,
    modality: Modality,
    output_ref: String,
}

async fn record_generation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<GenerationRequest>,
) -> Result<(StatusCode, Json<Data<GenerationRecord>>), PassportError> {
    authorize(&state, &headers)?;
    let record = state
        .service
        .record_generation(
            &id,
            request.version,
            &request.provider,
            request.modality,
            &request.output_ref,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(Data { data: record })))
}

async fn list_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<Vec<GenerationRecord>>>, PassportError> {
    authorize(&state, &headers)?;
    let records = state.service.generations(&id).await?;
    Ok(Json(Data { data: records }))
}

/// Constant-time `X-API-Key` check. Fails closed on a missing or mismatched key.
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), PassportError> {
    let presented = headers
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let expected = state.api_key.as_bytes();
    // `ct_eq` returns 0 for unequal lengths, then compares bytes with no
    // early-exit timing leak on the shared prefix.
    let matches: bool = expected.ct_eq(presented.as_bytes()).into();
    if matches && !expected.is_empty() {
        Ok(())
    } else {
        Err(PassportError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;

    fn app() -> Router {
        let state = AppState {
            service: PassportService::new(Arc::new(MemoryStore::default())),
            api_key: Arc::from("secret"),
        };
        build_router(state)
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn auth_rejects_missing_and_wrong_keys() {
        let app = app();
        let create = json!({
            "idempotency_key": "k1",
            "kind": "character",
            "owner": "menivor"
        });

        let no_key = app
            .clone()
            .oneshot(
                Request::post("/v1/passports")
                    .header("content-type", "application/json")
                    .body(Body::from(create.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED);

        let wrong_key = app
            .clone()
            .oneshot(
                Request::post("/v1/passports")
                    .header("content-type", "application/json")
                    .header("x-api-key", "nope")
                    .body(Body::from(create.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_key.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn full_lifecycle_over_http() {
        let app = app();
        let create = json!({
            "idempotency_key": "k1",
            "kind": "character",
            "owner": "menivor",
            "assets": {
                "kind": "character",
                "face_refs": ["face_a"],
                "voice_id": "eleven-42",
                "lora_ref": "lora_y",
                "style": "cinematic"
            }
        });
        let created = app
            .clone()
            .oneshot(
                Request::post("/v1/passports")
                    .header("content-type", "application/json")
                    .header("x-api-key", "secret")
                    .body(Body::from(create.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created_json = body_json(created).await;
        let id = created_json["data"]["id"].as_str().unwrap().to_string();

        // Lock it.
        let locked = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/passports/{id}/lock"))
                    .header("x-api-key", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(locked.status(), StatusCode::OK);
        let locked_json = body_json(locked).await;
        assert_eq!(locked_json["data"]["head"]["status"], "locked");

        // Resolve to a video conditioning payload.
        let resolve = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/passports/{id}/resolve"))
                    .header("content-type", "application/json")
                    .header("x-api-key", "secret")
                    .body(Body::from(
                        json!({"provider": "heygen", "modality": "video"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resolve.status(), StatusCode::OK);
        let resolve_json = body_json(resolve).await;
        assert_eq!(resolve_json["data"]["voice_id"], "eleven-42");
        assert_eq!(resolve_json["data"]["reference_images"][0], "face_a");

        // A locked head cannot be edited.
        let edit = app
            .clone()
            .oneshot(
                Request::patch(format!("/v1/passports/{id}"))
                    .header("content-type", "application/json")
                    .header("x-api-key", "secret")
                    .body(Body::from(
                        json!({"kind": "character", "style": "noir"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(edit.status(), StatusCode::CONFLICT);
    }
}
