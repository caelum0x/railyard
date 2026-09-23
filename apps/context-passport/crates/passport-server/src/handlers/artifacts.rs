//! Trained-artifact handlers: register, list, fetch, complete/fail, revoke.
//!
//! Artifacts are derived from a LOCKED version, so they can be registered and
//! revoked without branching a new version.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use passport_core::{PassportError, TrainedArtifact};

use crate::auth::authorize_passport;
use crate::dto::{RegisterArtifactRequest, RevokeArtifactRequest, UpdateArtifactRequest};
use crate::error::ApiError;
use crate::service::{RegisterArtifact, UpdateArtifact};
use crate::state::{AppState, Data};

/// `POST /v1/passports/:id/artifacts`
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RegisterArtifactRequest>,
) -> Result<(StatusCode, Json<Data<TrainedArtifact>>), ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    let artifact = state
        .service
        .register_artifact(
            &id,
            RegisterArtifact {
                provider: request.provider,
                kind: request.kind,
                version: request.version,
                artifact_ref: request.artifact_ref,
                provider_model: request.provider_model,
                status: request.status,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(Data { data: artifact })))
}

/// `GET /v1/passports/:id/artifacts` — newest first.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<Vec<TrainedArtifact>>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    Ok(Json(Data {
        data: state.service.artifacts(&id).await?,
    }))
}

/// `GET /v1/passports/:id/artifacts/:artifact_id`
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, artifact_id)): Path<(String, String)>,
) -> Result<Json<Data<TrainedArtifact>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    Ok(Json(Data {
        data: state.service.artifact(&id, &artifact_id).await?,
    }))
}

/// `PATCH /v1/passports/:id/artifacts/:artifact_id` — pending -> ready/failed,
/// or rotate a ready artifact's ref.
pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, artifact_id)): Path<(String, String)>,
    Json(request): Json<UpdateArtifactRequest>,
) -> Result<Json<Data<TrainedArtifact>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    let artifact = state
        .service
        .update_artifact(
            &id,
            &artifact_id,
            UpdateArtifact {
                status: request.status,
                artifact_ref: request.artifact_ref,
                provider_model: request.provider_model,
                failure_reason: request.failure_reason,
            },
        )
        .await?;
    Ok(Json(Data { data: artifact }))
}

/// `DELETE /v1/passports/:id/artifacts/:artifact_id` — revoke (the record is
/// kept for provenance). The JSON body `{ "reason": "..." }` is optional.
pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, artifact_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Data<TrainedArtifact>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    let request: RevokeArtifactRequest = if body.iter().all(u8::is_ascii_whitespace) {
        RevokeArtifactRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError(PassportError::Invalid(format!("invalid JSON body: {e}"))))?
    };
    Ok(Json(Data {
        data: state
            .service
            .revoke_artifact(&id, &artifact_id, request.reason)
            .await?,
    }))
}
