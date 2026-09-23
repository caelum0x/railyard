//! Operator scope and runtime billing status for Relay's dashboard endpoints.
//!
//! Every Relay billing read is principal-scoped by default: a gateway API key
//! only sees its own spend. Operators (the Relay UI's own key, finance tooling)
//! are listed in `RELAY_OPERATOR_KEY_IDS` (comma-separated API-key public ids)
//! and may request `?scope=all` to aggregate across every key, manage budgets,
//! and replay any dead-lettered cost event.

use axum::{
    Extension, Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tensorzero_auth::middleware::RequestApiKeyExtension;

/// Env var listing API-key public ids granted operator scope.
pub const OPERATOR_KEYS_ENV: &str = "RELAY_OPERATOR_KEY_IDS";

/// Requested aggregation scope for billing reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingScope {
    /// Only the authenticated key's own spend (default).
    #[default]
    Own,
    /// Every API key (operator keys only).
    All,
}

/// Parse the configured operator key ids. Blank entries are ignored.
pub fn operator_key_ids() -> Vec<String> {
    std::env::var(OPERATOR_KEYS_ENV)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the key public id is a configured operator.
pub fn is_operator(public_id: &str) -> bool {
    operator_key_ids().iter().any(|id| id == public_id)
}

/// Resolve the principal filter for a billing read.
///
/// Returns `Ok(Some(public_id))` for the caller's own scope, `Ok(None)` for the
/// operator-wide scope, or an error response when unauthenticated or when a
/// non-operator asks for `scope=all`.
#[expect(
    clippy::result_large_err,
    reason = "the error is the finished HTTP response returned directly by handlers"
)]
pub fn resolve_scope(
    principal: Option<&RequestApiKeyExtension>,
    scope: BillingScope,
) -> Result<Option<String>, Response> {
    let Some(principal) = principal else {
        return Err(access_error(
            StatusCode::UNAUTHORIZED,
            "relay_billing_unauthorized",
            "authenticated principal required: enable `[gateway.auth]` and call with a gateway API key",
        ));
    };
    let public_id = principal.key_info.public_id.clone();
    match scope {
        BillingScope::Own => Ok(Some(public_id)),
        BillingScope::All if is_operator(&public_id) => Ok(None),
        BillingScope::All => Err(access_error(
            StatusCode::FORBIDDEN,
            "relay_billing_forbidden",
            "operator scope requires an API key listed in RELAY_OPERATOR_KEY_IDS",
        )),
    }
}

/// Require an operator principal (budget writes, operator-wide mutation).
#[expect(
    clippy::result_large_err,
    reason = "the error is the finished HTTP response returned directly by handlers"
)]
pub fn require_operator(principal: Option<&RequestApiKeyExtension>) -> Result<String, Response> {
    resolve_scope(principal, BillingScope::All)?;
    principal
        .map(|principal| principal.key_info.public_id.clone())
        .ok_or_else(|| {
            access_error(
                StatusCode::UNAUTHORIZED,
                "relay_billing_unauthorized",
                "authenticated principal required",
            )
        })
}

pub fn access_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(relay_contract::ApiErrorEnvelope {
            error: relay_contract::ApiError {
                message: message.into(),
                kind: Some("relay_billing_access_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

/// Delivery target for durable cost events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CostFeedStatus {
    pub configured: bool,
    /// `raw` (Relay `CostEvent` JSON) or `rago` (Lago-compatible usage events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Host of the delivery endpoint (never the full URL or credentials).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billable_metric_code: Option<String>,
}

impl CostFeedStatus {
    pub fn unconfigured() -> Self {
        Self {
            configured: false,
            format: None,
            target_host: None,
            billable_metric_code: None,
        }
    }
}

/// Startup-resolved Relay billing wiring, exposed read-only to the dashboard so
/// an unconfigured deployment explains itself instead of showing zeros.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayRuntimeStatus {
    /// Prepaid Mint-settled billing (`RELAY_BILLING_ENABLED`).
    pub billing_enabled: bool,
    /// Postpaid meter-only usage recording (`RELAY_METERING_ENABLED`).
    pub metering_enabled: bool,
    /// Gateway API-key authentication (`[gateway.auth] enabled`).
    pub auth_enabled: bool,
    /// Postgres is configured (required for every durable billing surface).
    pub postgres_configured: bool,
    /// Per-key monthly spend caps are enforced.
    pub budgets_enforced: bool,
    pub cost_feed: CostFeedStatus,
}

/// `GET /relay/billing/status` response body.
#[derive(Debug, Clone, Serialize)]
pub struct RelayBillingStatusResponse {
    #[serde(flatten)]
    pub runtime: RelayRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    pub operator: bool,
    pub operator_keys_configured: bool,
    /// Human-readable configuration gaps, empty when fully wired.
    pub warnings: Vec<String>,
}

pub async fn billing_status_handler(
    Extension(runtime): Extension<RelayRuntimeStatus>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let principal_id = principal
        .as_ref()
        .map(|Extension(principal)| principal.key_info.public_id.clone());
    let operator = principal_id.as_deref().is_some_and(is_operator);
    let operator_keys_configured = !operator_key_ids().is_empty();
    let warnings = status_warnings(&runtime, operator_keys_configured);
    Json(relay_contract::DataEnvelope {
        data: RelayBillingStatusResponse {
            runtime,
            principal: principal_id,
            operator,
            operator_keys_configured,
            warnings,
        },
    })
    .into_response()
}

fn status_warnings(runtime: &RelayRuntimeStatus, operator_keys_configured: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    if !runtime.auth_enabled {
        warnings.push(
            "Gateway authentication is disabled, so usage cannot be attributed to an API key. Set `[gateway.auth] enabled = true`.".to_string(),
        );
    }
    if !runtime.postgres_configured {
        warnings.push(
            "Postgres is not configured (TENSORZERO_POSTGRES_URL); spend, settlements, budgets and cost events are unavailable.".to_string(),
        );
    }
    if !runtime.billing_enabled && !runtime.metering_enabled {
        warnings.push(
            "Neither RELAY_BILLING_ENABLED (prepaid Mint settlement) nor RELAY_METERING_ENABLED (postpaid metering) is set, so no usage is recorded.".to_string(),
        );
    }
    if (runtime.billing_enabled || runtime.metering_enabled) && !runtime.cost_feed.configured {
        warnings.push(
            "RELAY_COST_FEED_URL is unset; cost events stay pending and are not forwarded to invoicing (e.g. Rago).".to_string(),
        );
    }
    if !operator_keys_configured {
        warnings.push(
            "RELAY_OPERATOR_KEY_IDS is unset; the dashboard can only show the spend of the key it authenticates with.".to_string(),
        );
    }
    warnings
}

/// Prepaid Mint ledger handle, present only when `RELAY_BILLING_ENABLED` is on.
#[derive(Clone)]
pub struct WalletLedger(pub std::sync::Arc<dyn relay_billing::BillingLedger>);

/// `GET /relay/billing/wallet` response body.
#[derive(Debug, Clone, Serialize)]
pub struct WalletBalanceResponse {
    pub principal: String,
    pub billing_enabled: bool,
    /// Spendable Mint balance for the key's payer account (prepaid mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_micros: Option<i64>,
}

/// Live Mint balance for the calling key's payer account.
pub async fn wallet_balance_handler(
    ledger: Option<Extension<WalletLedger>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return access_error(
            StatusCode::UNAUTHORIZED,
            "relay_billing_unauthorized",
            "authenticated principal required",
        );
    };
    let public_id = principal.key_info.public_id.clone();
    let Some(Extension(WalletLedger(ledger))) = ledger else {
        return Json(relay_contract::DataEnvelope {
            data: WalletBalanceResponse {
                principal: public_id,
                billing_enabled: false,
                available_micros: None,
            },
        })
        .into_response();
    };
    match ledger
        .available_micros(&relay_billing::AccountId::new(public_id.clone()))
        .await
    {
        Ok(available) => Json(relay_contract::DataEnvelope {
            data: WalletBalanceResponse {
                principal: public_id,
                billing_enabled: true,
                available_micros: Some(available.get()),
            },
        })
        .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "Relay Mint balance lookup failed");
            access_error(
                StatusCode::BAD_GATEWAY,
                "relay_wallet_unavailable",
                "the Mint ledger balance is unavailable",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fully_wired_runtime_has_no_warnings_except_operator_hint() {
        let runtime = RelayRuntimeStatus {
            billing_enabled: false,
            metering_enabled: true,
            auth_enabled: true,
            postgres_configured: true,
            budgets_enforced: true,
            cost_feed: CostFeedStatus {
                configured: true,
                format: Some("rago".into()),
                target_host: Some("rago.internal".into()),
                billable_metric_code: Some("relay_llm_usage".into()),
            },
        };
        assert!(status_warnings(&runtime, true).is_empty());
        assert_eq!(status_warnings(&runtime, false).len(), 1);
    }

    #[test]
    fn unconfigured_runtime_explains_every_gap() {
        let runtime = RelayRuntimeStatus {
            billing_enabled: false,
            metering_enabled: false,
            auth_enabled: false,
            postgres_configured: false,
            budgets_enforced: false,
            cost_feed: CostFeedStatus::unconfigured(),
        };
        assert_eq!(status_warnings(&runtime, false).len(), 4);
    }
}
