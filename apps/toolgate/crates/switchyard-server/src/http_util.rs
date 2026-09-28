//! Response helpers: the one error envelope and JSON success bodies.

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use switchyard_core::ApiError;

pub struct AppError(pub ApiError);

impl From<ApiError> for AppError {
    fn from(e: ApiError) -> Self {
        AppError(e)
    }
}

impl From<switchyard_store::StoreError> for AppError {
    fn from(e: switchyard_store::StoreError) -> Self {
        AppError(match e {
            switchyard_store::StoreError::NotFound(w) => ApiError::not_found(format!("{w} not found")),
            switchyard_store::StoreError::Conflict(w) => ApiError::new(409, "conflict", w),
            switchyard_store::StoreError::Db(w) => {
                tracing::error!(error = %w, "store error");
                ApiError::internal("storage error")
            }
        })
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut resp = (status, Json(self.0.body())).into_response();
        if self.0.status == 401 {
            resp.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer realm=\"switchyard\""));
        }
        resp
    }
}

pub type ApiResult = Result<Response, AppError>;

pub fn ok(data: Value) -> Response {
    Json(json!({ "ok": true, "data": data })).into_response()
}

pub fn with_headers(mut resp: Response, headers: &[(&'static str, String)]) -> Response {
    let h: &mut HeaderMap = resp.headers_mut();
    for (k, v) in headers {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(*k, v);
        }
    }
    resp
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}
