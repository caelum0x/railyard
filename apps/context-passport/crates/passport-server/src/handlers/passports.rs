//! Passport lifecycle handlers: create, list, get, patch (draft edit), lock.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use passport_core::{AssetsPatch, CreatePassport, Passport, PassportFilter, PassportView};

use crate::auth::authorize;
use crate::error::ApiError;
use crate::state::{AppState, Data};

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreatePassport>,
) -> Result<(StatusCode, Json<Data<PassportView>>), ApiError> {
    authorize(&state.api_key, &headers)?;
    let result = state.service.create(request).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(Data { data: result.view })))
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filter): Query<PassportFilter>,
) -> Result<Json<Data<Vec<Passport>>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.list(&filter).await?,
    }))
}

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<PassportView>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.get(&id).await?,
    }))
}

pub async fn update_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<AssetsPatch>,
) -> Result<Json<Data<PassportView>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.update_draft(&id, &patch).await?,
    }))
}

pub async fn lock(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Data<PassportView>>, ApiError> {
    authorize(&state.api_key, &headers)?;
    Ok(Json(Data {
        data: state.service.lock(&id).await?,
    }))
}
