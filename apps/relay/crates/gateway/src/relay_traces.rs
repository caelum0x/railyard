//! Per-API-key trace view (RY-RL3, OSS9).
//!
//! - `GET /relay/traces`: the caller's own traces, newest first. Operator keys
//!   (`RELAY_OPERATOR_KEY_IDS`) may pass `?scope=all`, optionally with `&key=`.
//! - `GET /relay/traces/{inference_id}`: one trace with its model calls and
//!   bodies. A trace owned by another key returns 404, exactly like a missing
//!   one.
//!
//! The handlers are thin: scope comes from the authenticated principal, the
//! reads live in the `relay-traces` crate.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_traces::TraceStore;
use relay_traces::query::{RawTraceQuery, TraceListParams, scope_for};
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tensorzero_core::utils::gateway::{AppState, AppStateData};
use uuid::Uuid;

use crate::relay_access::is_operator;

fn trace_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(relay_contract::ApiErrorEnvelope {
            error: relay_contract::ApiError {
                message: message.into(),
                kind: Some("relay_traces_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the finished HTTP response returned directly by handlers"
)]
fn resolve(
    principal: Option<&RequestApiKeyExtension>,
    raw: &RawTraceQuery,
) -> Result<relay_traces::TraceScope, Response> {
    let Some(principal) = principal else {
        return Err(trace_error(
            StatusCode::UNAUTHORIZED,
            "relay_traces_unauthorized",
            "authenticated principal required: enable `[gateway.auth]` and call with a gateway API key",
        ));
    };
    let public_id = principal.key_info.public_id.as_str();
    if raw.wants_all() && !is_operator(public_id) {
        return Err(trace_error(
            StatusCode::FORBIDDEN,
            "relay_traces_forbidden",
            "operator scope requires an API key listed in RELAY_OPERATOR_KEY_IDS",
        ));
    }
    scope_for(public_id, is_operator(public_id), raw).map_err(|error| {
        trace_error(
            StatusCode::BAD_REQUEST,
            "relay_traces_bad_request",
            &error.to_string(),
        )
    })
}

pub async fn list_traces_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(raw): Query<RawTraceQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let scope = match resolve(principal.as_ref().map(|p| &p.0), &raw) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match TraceListParams::from_raw(&raw) {
        Ok(params) => params,
        Err(error) => {
            return trace_error(
                StatusCode::BAD_REQUEST,
                "relay_traces_bad_request",
                &error.to_string(),
            );
        }
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return trace_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_traces_unavailable",
            "traces require Postgres",
        );
    };
    match TraceStore::new(pool.clone()).list(&scope, &params).await {
        Ok(page) => Json(relay_contract::DataEnvelope { data: page }).into_response(),
        Err(error) => {
            tracing::error!(error = %error, "Relay trace list failed");
            trace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_traces_failed",
                "traces are unavailable",
            )
        }
    }
}

pub async fn get_trace_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Path(inference_id): Path<String>,
    Query(raw): Query<RawTraceQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let scope = match resolve(principal.as_ref().map(|p| &p.0), &raw) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Ok(inference_id) = Uuid::parse_str(inference_id.trim()) else {
        return trace_error(
            StatusCode::NOT_FOUND,
            "relay_trace_not_found",
            "trace not found",
        );
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return trace_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_traces_unavailable",
            "traces require Postgres",
        );
    };
    match TraceStore::new(pool.clone()).get(&scope, inference_id).await {
        Ok(Some(detail)) => Json(relay_contract::DataEnvelope { data: detail }).into_response(),
        Ok(None) => trace_error(
            StatusCode::NOT_FOUND,
            "relay_trace_not_found",
            "trace not found",
        ),
        Err(error) => {
            tracing::error!(error = %error, "Relay trace read failed");
            trace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_traces_failed",
                "traces are unavailable",
            )
        }
    }
}
