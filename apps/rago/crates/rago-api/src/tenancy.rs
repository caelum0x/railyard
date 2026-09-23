//! Tenant-isolation middleware for `/api/v1` and `/api/v2`.
//!
//! Many handlers look records up by primary key (`/invoices/:id`,
//! `/webhook_endpoints/:id`, `invoice_id` in a credit-note body, ...). This
//! layer runs before every handler: it collects every UUID the request
//! references — path segments, query string and JSON body — resolves the
//! caller's organization from the bearer API key, and answers `404` when any of
//! those UUIDs belongs to a different organization. A caller can therefore never
//! read, mutate, or even confirm the existence of another tenant's records.
//!
//! Requests without a valid key pass through untouched: the handler's own
//! [`crate::auth::Authed`] extractor rejects them with `401`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use rago_db::tenancy::{uuids_in_text, MAX_CHECKED_IDS};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Largest body the guard buffers for scanning (matches axum's JSON default
/// order of magnitude; larger bodies are rejected with 413 by the guard).
const MAX_SCANNED_BODY: usize = 4 * 1024 * 1024;

/// Event-ingestion routes carry client transaction ids (often UUIDs) that are
/// not record references, so their bodies are not scanned.
fn skip_body_scan(path: &str) -> bool {
    path.ends_with("/events") || path.ends_with("/events/batch") || path.ends_with("/events/estimate_fees")
        || path.ends_with("/fees/estimate") || path.ends_with("/reels/usage")
}

/// A resource name for the 404 code, derived from the first path segment after
/// the API version (`/api/v1/invoices/...` → `invoice`).
fn resource_name(path: &str) -> String {
    let seg = path
        .split('/')
        .filter(|s| !s.is_empty())
        .nth(2)
        .unwrap_or("resource");
    seg.strip_suffix('s').unwrap_or(seg).to_string()
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_whitespace().nth(1))
        .map(str::to_string)
}

/// The middleware (install with `from_fn_with_state`).
pub async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut ids: Vec<Uuid> = uuids_in_text(&path);
    for id in uuids_in_text(req.uri().query().unwrap_or("")) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    let scan_body = !skip_body_scan(&path)
        && matches!(*req.method(), axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::PATCH | axum::http::Method::DELETE);

    let req = if scan_body {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, MAX_SCANNED_BODY).await {
            Ok(b) => b,
            Err(_) => {
                return ApiError::BadRequest("request body too large".into()).into_response();
            }
        };
        if let Ok(text) = std::str::from_utf8(&bytes) {
            for id in uuids_in_text(text) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Request::from_parts(parts, Body::from(bytes))
    } else {
        req
    };

    if ids.is_empty() {
        return next.run(req).await;
    }

    let Some(token) = bearer(&req) else {
        return next.run(req).await;
    };
    let org = match st.db.authenticate(&token).await {
        Ok(Some((key, org))) if !key.is_expired(Utc::now()) => org.id,
        Ok(_) => return next.run(req).await,
        Err(e) => return ApiError::from(e).into_response(),
    };

    for chunk in ids.chunks(MAX_CHECKED_IDS) {
        match st.db.foreign_ids(org, chunk).await {
            Ok(foreign) if foreign.is_empty() => {}
            Ok(foreign) => {
                tracing::warn!(
                    organization_id = %org,
                    path = %path,
                    foreign = ?foreign,
                    "cross-tenant reference rejected"
                );
                return ApiError::NotFound(resource_name(&path)).into_response();
            }
            Err(e) => return ApiError::from(e).into_response(),
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_names() {
        assert_eq!(resource_name("/api/v1/invoices/abc/void"), "invoice");
        assert_eq!(resource_name("/api/v1/webhook_endpoints/abc"), "webhook_endpoint");
        assert!(skip_body_scan("/api/v2/events/batch"));
        assert!(!skip_body_scan("/api/v1/credit_notes"));
    }
}
