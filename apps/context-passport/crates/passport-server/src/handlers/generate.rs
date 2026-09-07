//! Resolve + generate-AS-passport + provenance listing.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use passport_provenance::GenerationRecord;
use passport_resolve::ConditioningPayload;

use crate::auth::authorize;
use crate::dto::{AttestRequest, GenerateRequest, ResolveRequest};
use crate::error::ApiError;
use crate::state::{AppState, Data};

/// Resolve a locked passport into provider conditioning (no generation).
pub async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ResolveRequest>,
) -> Result<Json<Data<ConditioningPayload>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    let payload = state
        .service
        .resolve(&id, &request.provider, request.modality, request.version)
        .await?;
    Ok(Json(Data { data: payload }))
}

/// Generate AS this passport through Relay, recording provenance.
pub async fn generate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<GenerateRequest>,
) -> Result<(StatusCode, Json<Data<GenerationRecord>>), ApiError> {
    authorize(&state.api_key, &headers)?;
    let record = state
        .service
        .generate(&id, &request.provider, request.modality, request.version)
        .await?;
    Ok((StatusCode::CREATED, Json(Data { data: record })))
}

/// Append provenance for a first-party generation performed outside this
/// service, after re-validating both the lock and signed settlement evidence.
pub async fn attest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<AttestRequest>,
) -> Result<(StatusCode, Json<Data<GenerationRecord>>), ApiError> {
    authorize(&state.api_key, &headers)?;
    let record = state
        .service
        .attest(
            &id,
            &request.provider,
            request.modality,
            request.version,
            request.output_ref,
            request.asset_hash,
            request.payment_evidence,
            request.settled_at_ms,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(Data { data: record })))
}

pub async fn list_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<Vec<GenerationRecord>>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.generations(&id).await?,
    }))
}
