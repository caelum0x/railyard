//! Version handlers: list the history, fetch a specific version, branch a new draft.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use passport_core::{PassportVersion, PassportView};

use crate::auth::authorize_passport;
use crate::error::ApiError;
use crate::state::{AppState, Data};

/// `GET /v1/passports/:id/versions` — every version, oldest first.
pub async fn list_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<Vec<PassportVersion>>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    Ok(Json(Data {
        data: state.service.versions(&id).await?,
    }))
}

pub async fn get_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version)): Path<(String, i64)>,
) -> Result<Json<Data<PassportVersion>>, ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    Ok(Json(Data {
        data: state.service.get_version(&id, version).await?,
    }))
}

pub async fn new_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Data<PassportView>>), ApiError> {
    authorize_passport(&state, &headers, &id).await?;
    Ok((
        StatusCode::CREATED,
        Json(Data {
            data: state.service.new_version(&id).await?,
        }),
    ))
}
