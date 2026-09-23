//! Authenticated billing summary endpoint for the Relay UI dashboard.
//!
//! `GET /relay/billing/summary` aggregates the caller's durable
//! `relay_cost_events` and `relay_settlement_evidence` rows into a
//! [`relay_contract::BillingSummary`] (wallet rollup, per-key spend, per-model
//! spend, spend/token totals, and recent settlements). Aggregation is scoped to
//! the authenticated principal, so a caller only ever sees its own spend.

use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_storage_postgres::{DEFAULT_SETTLEMENT_HISTORY_LIMIT, PostgresBillingSummaryStore};
use serde::Deserialize;
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tensorzero_core::utils::gateway::{AppState, AppStateData};

/// Optional query parameters for the billing summary.
#[derive(Debug, Default, Deserialize)]
pub struct BillingSummaryQuery {
    /// Number of recent settlement receipts to return. Clamped by the store.
    #[serde(default)]
    pub settlements: Option<i64>,
}

pub async fn billing_summary_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(params): Query<BillingSummaryQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return billing_summary_error(
            StatusCode::UNAUTHORIZED,
            "authenticated principal required",
        );
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing summary requires Postgres",
        );
    };
    let store = PostgresBillingSummaryStore::new(pool.clone());
    let limit = params
        .settlements
        .unwrap_or(DEFAULT_SETTLEMENT_HISTORY_LIMIT);
    match store.summary(&principal.key_info.public_id, limit).await {
        Ok(summary) => Json(relay_contract::DataEnvelope { data: summary }).into_response(),
        Err(error) => {
            tracing::error!(error = %error, "Relay billing summary aggregation failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "billing summary is unavailable",
            )
        }
    }
}

fn billing_summary_error(status: StatusCode, message: &str) -> Response {
    let code = match status {
        StatusCode::UNAUTHORIZED => "relay_billing_summary_unauthorized",
        StatusCode::SERVICE_UNAVAILABLE => "relay_billing_summary_unconfigured",
        _ => "relay_billing_summary_unavailable",
    };
    (
        status,
        Json(relay_contract::ApiErrorEnvelope {
            error: relay_contract::ApiError {
                message: message.into(),
                kind: Some("relay_billing_summary_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}
