//! Version handlers: fetch a specific version, branch a new draft.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use passport_core::{PassportVersion, PassportView};

use crate::auth::authorize;
use crate::error::ApiError;
use crate::state::{AppState, Data};

pub async fn get_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version)): Path<(String, i64)>,
) -> Result<Json<Data<PassportVersion>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.get_version(&id, version).await?,
    }))
}

pub async fn new_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Data<PassportView>>), ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(Data {
            data: state.service.new_version(&id).await?,
        }),
    ))
}
