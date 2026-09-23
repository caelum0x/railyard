//! Authenticated billing summary endpoint for the Relay UI dashboard.
//!
//! `GET /relay/billing/summary` aggregates the caller's durable
//! `relay_cost_events` and `relay_settlement_evidence` rows into a
//! [`relay_contract::BillingSummary`] (wallet rollup, per-key spend, per-model
//! spend, spend/token totals, and recent settlements). Aggregation is scoped to
//! the authenticated principal, so a caller only ever sees its own spend, unless
//! an operator key (see [`crate::relay_access`]) explicitly asks for
//! `?scope=all`.
//!
//! This module also serves per-key monthly spend caps
//! (`/relay/billing/budgets`), which the gateway enforces before inference.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_storage_postgres::{
    DEFAULT_SETTLEMENT_HISTORY_LIMIT, KeyBudgetError, KeyBudgetInput, MAX_SETTLEMENT_HISTORY_LIMIT,
    OPERATOR_SCOPE_PRINCIPAL, PostgresBillingSummaryStore, PostgresKeyBudgetStore,
};
use serde::Deserialize;

use crate::relay_access::{BillingScope, require_operator, resolve_scope};
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tensorzero_core::utils::gateway::{AppState, AppStateData};

/// Optional query parameters for the billing summary.
#[derive(Debug, Default, Deserialize)]
pub struct BillingSummaryQuery {
    /// Number of recent settlement receipts to return. Clamped by the store.
    #[serde(default)]
    pub settlements: Option<i64>,
    /// `own` (default) or `all` (operator keys only).
    #[serde(default)]
    pub scope: BillingScope,
    /// Settlement-history cursor (`<settled_at_ms>:<request_id>`) returned as
    /// `next_before` by the previous page.
    #[serde(default)]
    pub before: Option<String>,
}

/// Parse a `<settled_at_ms>:<request_id>` settlement cursor.
fn parse_settlement_cursor(raw: &str) -> Option<(i64, uuid::Uuid)> {
    let (ms, id) = raw.trim().split_once(':')?;
    Some((ms.parse().ok()?, uuid::Uuid::parse_str(id).ok()?))
}

/// Scope-only query for endpoints without a settlement window.
#[derive(Debug, Default, Deserialize)]
pub struct BillingScopeQuery {
    #[serde(default)]
    pub scope: BillingScope,
}

pub async fn billing_summary_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(params): Query<BillingSummaryQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let owner = match resolve_scope(principal.as_ref().map(|p| &p.0), params.scope) {
        Ok(owner) => owner,
        Err(response) => return response,
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
    match store.summary(owner.as_deref(), limit).await {
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

pub async fn billing_settlements_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(params): Query<BillingSummaryQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let owner = match resolve_scope(principal.as_ref().map(|p| &p.0), params.scope) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing settlements require Postgres",
        );
    };
    let store = PostgresBillingSummaryStore::new(pool.clone());
    let limit = params
        .settlements
        .unwrap_or(DEFAULT_SETTLEMENT_HISTORY_LIMIT)
        .clamp(1, MAX_SETTLEMENT_HISTORY_LIMIT);
    let before = match params.before.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match parse_settlement_cursor(raw) {
            Some(cursor) => Some(cursor),
            None => {
                return billing_summary_error(StatusCode::BAD_REQUEST, "invalid settlement cursor");
            }
        },
    };
    match store
        .settlements_before(owner.as_deref(), limit, before)
        .await
    {
        Ok(settlements) => {
            let next_before = (settlements.len() as i64 == limit)
                .then(|| settlements.last())
                .flatten()
                .map(|last| format!("{}:{}", last.settled_at_ms, last.request_id));
            Json(relay_contract::DataEnvelope {
                data: relay_contract::SettlementHistory {
                    principal: owner.unwrap_or_else(|| OPERATOR_SCOPE_PRINCIPAL.to_string()),
                    settlements,
                    next_before,
                },
            })
            .into_response()
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay settlement history aggregation failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settlement history is unavailable",
            )
        }
    }
}

pub async fn billing_keys_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(params): Query<BillingScopeQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let owner = match resolve_scope(principal.as_ref().map(|p| &p.0), params.scope) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing key spend requires Postgres",
        );
    };
    let store = PostgresBillingSummaryStore::new(pool.clone());
    match store.key_spend_timeseries(owner.as_deref()).await {
        Ok(points) => Json(relay_contract::DataEnvelope {
            data: relay_contract::KeySpendSeries {
                principal: owner.unwrap_or_else(|| OPERATOR_SCOPE_PRINCIPAL.to_string()),
                points,
            },
        })
        .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "Relay key spend aggregation failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key spend is unavailable",
            )
        }
    }
}

/// `GET /relay/billing/budgets`: every cap for operators (`scope=all`), else
/// only the caller's own cap.
pub async fn list_budgets_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Query(params): Query<BillingScopeQuery>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let owner = match resolve_scope(principal.as_ref().map(|p| &p.0), params.scope) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "spend caps require Postgres",
        );
    };
    let store = PostgresKeyBudgetStore::new(pool.clone());
    let result = match owner.as_deref() {
        Some(owner) => store
            .get(owner)
            .await
            .map(|budget| budget.into_iter().collect()),
        None => store.list().await,
    };
    match result {
        Ok(budgets) => Json(relay_contract::DataEnvelope { data: budgets }).into_response(),
        Err(error) => {
            tracing::error!(error = %error, "Relay budget listing failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "spend caps are unavailable",
            )
        }
    }
}

/// `PUT /relay/billing/budgets/{key_id}`: create or replace a key's monthly
/// cap. Operator keys only.
pub async fn upsert_budget_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Path(key_id): Path<String>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(input): Json<KeyBudgetInput>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "spend caps require Postgres",
        );
    };
    let key_id = key_id.trim().to_string();
    if key_id.is_empty() || key_id.len() > 128 {
        return billing_summary_error(StatusCode::BAD_REQUEST, "invalid API key id");
    }
    match PostgresKeyBudgetStore::new(pool.clone())
        .upsert(&key_id, &input, &operator)
        .await
    {
        Ok(budget) => Json(relay_contract::DataEnvelope { data: budget }).into_response(),
        Err(KeyBudgetError::Invalid(message)) => {
            billing_summary_error(StatusCode::BAD_REQUEST, &message)
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay budget upsert failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "spend cap could not be saved",
            )
        }
    }
}

/// `DELETE /relay/billing/budgets/{key_id}`: remove a key's cap. Operator only.
pub async fn delete_budget_handler(
    State(AppStateData {
        postgres_connection_info,
        ..
    }): AppState,
    Path(key_id): Path<String>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    if let Err(response) = require_operator(principal.as_ref().map(|p| &p.0)) {
        return response;
    }
    let Some(pool) = postgres_connection_info.get_pool() else {
        return billing_summary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "spend caps require Postgres",
        );
    };
    match PostgresKeyBudgetStore::new(pool.clone())
        .delete(key_id.trim())
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => billing_summary_error(StatusCode::NOT_FOUND, "spend cap not found"),
        Err(error) => {
            tracing::error!(error = %error, "Relay budget delete failed");
            billing_summary_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "spend cap could not be removed",
            )
        }
    }
}

fn billing_summary_error(status: StatusCode, message: &str) -> Response {
    let code = match status {
        StatusCode::UNAUTHORIZED => "relay_billing_summary_unauthorized",
        StatusCode::BAD_REQUEST => "relay_billing_invalid_request",
        StatusCode::NOT_FOUND => "relay_billing_not_found",
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
