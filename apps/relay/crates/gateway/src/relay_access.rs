//! Operator scope and runtime billing status for Relay's dashboard endpoints.
//!
//! Every Relay billing read is principal-scoped by default: a gateway API key
//! only sees its own spend. Operators (the Relay UI's own key, finance tooling)
//! are listed in `RELAY_OPERATOR_KEY_IDS` (comma-separated API-key public ids)
//! and may request `?scope=all` to aggregate across every key, manage budgets,
//! and replay any dead-lettered cost event.

use axum::{
    Extension, Json,
    extract::{Query, Request},
    http::StatusCode,
    middleware::Next,
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

/// Route-layer middleware for a whole sub-router: reject a non-operator key
/// instead of calling the handler. Used for the raw TensorZero observability/
/// dataset/evaluation and `/internal/*` surfaces (RY-RL5): those routes were
/// built for a single trusted operator (the Relay UI's own key) and carry
/// **no** per-key scoping at all — unlike `/relay/traces`, `/relay/billing/*`
/// and the provider-key/catalog-price editors, which each check
/// `require_operator` or scope to the caller's own key inside the handler. On
/// a shared multi-tenant Relay deployment (many Railyard products' API keys
/// against one gateway), any one of those keys could otherwise list or fetch
/// every other tenant's stored inferences, feedback, datasets and raw
/// object-storage bytes.
///
/// Unlike `require_operator`, a **missing** principal is let through rather
/// than rejected: `[gateway.auth]` is opt-in, and when it is off (the default
/// for a self-hosted, single-tenant TensorZero gateway, and for every e2e
/// test that doesn't configure auth) `tensorzero_auth_middleware` is never
/// layered on at all, so there is no `RequestApiKeyExtension` for *any*
/// request, not just these routes — there is exactly one tenant and nothing
/// to isolate, so that deployment's existing no-auth behavior for these
/// routes must be unchanged. When auth IS enabled, a request reaching this
/// `route_layer` has already passed `tensorzero_auth_middleware` (added
/// after this one wraps the whole router, so it runs first) — any key-less
/// request was already rejected there with 401, so a `None` here only ever
/// means "auth is off for this whole gateway".
pub async fn require_operator_middleware(request: Request, next: Next) -> Response {
    let principal = request
        .extensions()
        .get::<RequestApiKeyExtension>()
        .cloned();
    if let Some(principal) = &principal
        && !is_operator(&principal.key_info.public_id)
    {
        return access_error(
            StatusCode::FORBIDDEN,
            "relay_internal_forbidden",
            "this endpoint is not scoped per API key; it requires an operator key \
             (set RELAY_OPERATOR_KEY_IDS on the gateway)",
        );
    }
    next.run(request).await
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
    /// Per-key monthly and daily spend caps are enforced.
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

/// Env var listing Mint checkout providers offered for wallet top-ups
/// (comma-separated Mint provider tags, e.g. `creem,stripe`).
pub const TOPUP_PROVIDERS_ENV: &str = "RELAY_TOPUP_PROVIDERS";
/// Env var naming the Mint treasury account operator credits are drawn from.
pub const TREASURY_ACCOUNT_ENV: &str = "RELAY_WALLET_TREASURY_ACCOUNT";
/// Smallest and largest self-serve top-up, in micro-USD ($5 .. $10,000).
pub const MIN_TOPUP_MICROS: i64 = 5_000_000;
pub const MAX_TOPUP_MICROS: i64 = 10_000_000_000;
/// Largest single operator credit, in micro-USD ($100,000).
pub const MAX_CREDIT_MICROS: i64 = 100_000_000_000;
const MAX_CREDIT_NOTE_CHARS: usize = 280;

fn topup_providers() -> Vec<String> {
    std::env::var(TOPUP_PROVIDERS_ENV)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|tag| tag.trim().to_ascii_lowercase())
                .filter(|tag| !tag.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn treasury_account() -> Option<String> {
    std::env::var(TREASURY_ACCOUNT_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `GET /relay/billing/wallet` response body.
#[derive(Debug, Clone, Serialize)]
pub struct WalletBalanceResponse {
    pub principal: String,
    pub billing_enabled: bool,
    /// Mint account that pays for this key's inference (prepaid mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_account: Option<String>,
    /// Spendable Mint balance for the key's payer account (prepaid mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_micros: Option<i64>,
    /// Checkout providers offered for self-serve top-ups; empty when none are
    /// configured (`RELAY_TOPUP_PROVIDERS`).
    pub top_up_providers: Vec<String>,
    pub min_top_up_micros: i64,
    pub max_top_up_micros: i64,
    /// Whether the caller may grant operator credit (operator key and
    /// `RELAY_WALLET_TREASURY_ACCOUNT` configured).
    pub operator_credit_available: bool,
}

/// Query for wallet reads: operators may inspect another key's wallet.
#[derive(Debug, Default, Deserialize)]
pub struct WalletQuery {
    #[serde(default)]
    pub principal: Option<String>,
}

/// Resolve whose wallet a request targets: the caller's own, or (operators
/// only) another key's.
#[expect(
    clippy::result_large_err,
    reason = "the error is the finished HTTP response returned directly by handlers"
)]
fn wallet_target(
    principal: Option<&RequestApiKeyExtension>,
    requested: Option<&str>,
) -> Result<(String, bool), Response> {
    let Some(principal) = principal else {
        return Err(access_error(
            StatusCode::UNAUTHORIZED,
            "relay_billing_unauthorized",
            "authenticated principal required",
        ));
    };
    let own = principal.key_info.public_id.clone();
    let operator = is_operator(&own);
    match requested.map(str::trim).filter(|id| !id.is_empty()) {
        None => Ok((own, operator)),
        Some(id) if id == own => Ok((own, operator)),
        Some(_) if !operator => Err(access_error(
            StatusCode::FORBIDDEN,
            "relay_billing_forbidden",
            "only operator keys can manage another key's wallet",
        )),
        Some(id) if !is_valid_key_id(id) => Err(access_error(
            StatusCode::BAD_REQUEST,
            "relay_invalid_principal",
            "invalid API key public id",
        )),
        Some(id) => Ok((id.to_string(), operator)),
    }
}

fn is_valid_key_id(id: &str) -> bool {
    id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Live Mint balance for a key's payer account (the caller's own by default).
pub async fn wallet_balance_handler(
    ledger: Option<Extension<WalletLedger>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Query(query): Query<WalletQuery>,
) -> Response {
    let (target, operator) =
        match wallet_target(principal.as_ref().map(|p| &p.0), query.principal.as_deref()) {
            Ok(target) => target,
            Err(response) => return response,
        };
    let Some(Extension(WalletLedger(ledger))) = ledger else {
        return Json(relay_contract::DataEnvelope {
            data: WalletBalanceResponse {
                principal: target,
                billing_enabled: false,
                payer_account: None,
                available_micros: None,
                top_up_providers: Vec::new(),
                min_top_up_micros: MIN_TOPUP_MICROS,
                max_top_up_micros: MAX_TOPUP_MICROS,
                operator_credit_available: false,
            },
        })
        .into_response();
    };
    let account = relay_billing::AccountId::new(target.clone());
    let payer = ledger.resolve_payer(&account);
    match ledger.available_micros(&account).await {
        Ok(available) => Json(relay_contract::DataEnvelope {
            data: WalletBalanceResponse {
                principal: target,
                billing_enabled: true,
                payer_account: Some(payer.as_str().to_string()),
                available_micros: Some(available.get()),
                top_up_providers: topup_providers(),
                min_top_up_micros: MIN_TOPUP_MICROS,
                max_top_up_micros: MAX_TOPUP_MICROS,
                operator_credit_available: operator && treasury_account().is_some(),
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

/// Body of `POST /relay/billing/wallet/top_up`.
#[derive(Debug, Deserialize)]
pub struct TopUpRequest {
    pub amount_micros: i64,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub success_url: Option<String>,
    #[serde(default)]
    pub cancel_url: Option<String>,
    /// Operators may fund another key's wallet.
    #[serde(default)]
    pub principal: Option<String>,
}

/// `POST /relay/billing/wallet/top_up`: create a hosted checkout that funds a
/// key's Mint payer wallet. The provider webhook credits Mint once paid.
pub async fn wallet_top_up_handler(
    ledger: Option<Extension<WalletLedger>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(request): Json<TopUpRequest>,
) -> Response {
    let (target, _) = match wallet_target(
        principal.as_ref().map(|p| &p.0),
        request.principal.as_deref(),
    ) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let Some(Extension(WalletLedger(ledger))) = ledger else {
        return access_error(
            StatusCode::CONFLICT,
            "relay_wallet_disabled",
            "wallets exist only in prepaid mode (RELAY_BILLING_ENABLED); postpaid usage is invoiced instead",
        );
    };
    let providers = topup_providers();
    if providers.is_empty() {
        return access_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_topup_unconfigured",
            "no top-up checkout provider is configured; set RELAY_TOPUP_PROVIDERS on the gateway (and activate that provider in Mint)",
        );
    }
    let provider = match request
        .provider
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase())
    {
        Some(provider) if providers.contains(&provider) => provider,
        Some(_) => {
            return access_error(
                StatusCode::BAD_REQUEST,
                "relay_topup_provider_unsupported",
                "that checkout provider is not offered for top-ups",
            );
        }
        None => providers[0].clone(),
    };
    if !(MIN_TOPUP_MICROS..=MAX_TOPUP_MICROS).contains(&request.amount_micros)
        || request.amount_micros % 10_000 != 0
    {
        return access_error(
            StatusCode::BAD_REQUEST,
            "relay_topup_invalid_amount",
            &format!(
                "top-ups must be whole cents between ${} and ${}",
                MIN_TOPUP_MICROS / 1_000_000,
                MAX_TOPUP_MICROS / 1_000_000
            ),
        );
    }
    let checkout = ledger
        .create_top_up_checkout(
            &relay_billing::AccountId::new(target.clone()),
            &relay_billing::TopUpCheckoutRequest {
                provider,
                amount_micros: request.amount_micros,
                success_url: request.success_url.filter(|url| !url.trim().is_empty()),
                cancel_url: request.cancel_url.filter(|url| !url.trim().is_empty()),
            },
        )
        .await;
    match checkout {
        Ok(checkout) => {
            tracing::info!(principal = %target, checkout_id = %checkout.checkout_id, amount_micros = checkout.amount_micros, "Relay wallet top-up checkout created");
            (
                StatusCode::CREATED,
                Json(relay_contract::DataEnvelope { data: checkout }),
            )
                .into_response()
        }
        Err(error) => ledger_error_response(&error, "the top-up checkout could not be created"),
    }
}

/// Body of `POST /relay/billing/wallet/credit`.
#[derive(Debug, Deserialize)]
pub struct CreditRequest {
    pub principal: String,
    pub amount_micros: i64,
    /// Client-generated key so a retried submit never double-credits.
    pub idempotency_key: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /relay/billing/wallet/credit`: move funds from the operator treasury
/// into a key's payer wallet. Operator only.
pub async fn wallet_credit_handler(
    ledger: Option<Extension<WalletLedger>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(request): Json<CreditRequest>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let (target, _) = match wallet_target(
        principal.as_ref().map(|p| &p.0),
        Some(request.principal.as_str()),
    ) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let Some(Extension(WalletLedger(ledger))) = ledger else {
        return access_error(
            StatusCode::CONFLICT,
            "relay_wallet_disabled",
            "wallets exist only in prepaid mode (RELAY_BILLING_ENABLED)",
        );
    };
    let Some(treasury) = treasury_account() else {
        return access_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_treasury_unconfigured",
            "operator credit needs a funded Mint treasury account; set RELAY_WALLET_TREASURY_ACCOUNT on the gateway",
        );
    };
    if request.amount_micros <= 0 || request.amount_micros > MAX_CREDIT_MICROS {
        return access_error(
            StatusCode::BAD_REQUEST,
            "relay_credit_invalid_amount",
            &format!(
                "credit must be positive and at most ${}",
                MAX_CREDIT_MICROS / 1_000_000
            ),
        );
    }
    let idem = request.idempotency_key.trim();
    if idem.is_empty()
        || idem.len() > 128
        || !idem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return access_error(
            StatusCode::BAD_REQUEST,
            "relay_credit_invalid_idempotency_key",
            "idempotency_key must be 1-128 letters, digits, '-' or '_'",
        );
    }
    let note = request.note.as_deref().map(str::trim).unwrap_or_default();
    if note.chars().count() > MAX_CREDIT_NOTE_CHARS {
        return access_error(
            StatusCode::BAD_REQUEST,
            "relay_credit_invalid_note",
            "note must be at most 280 characters",
        );
    }
    let reference = if note.is_empty() {
        format!("relay-credit by {operator}")
    } else {
        format!("relay-credit by {operator}: {note}")
    };
    match ledger
        .credit_account(
            &treasury,
            &relay_billing::AccountId::new(target.clone()),
            relay_billing::Micros::new(request.amount_micros),
            &format!("relay-credit:{target}:{idem}"),
            &reference,
        )
        .await
    {
        Ok(credit) => {
            tracing::info!(principal = %target, operator = %operator, amount_micros = credit.amount_micros, replay = credit.idempotent_replay, "Relay operator credited a wallet");
            Json(relay_contract::DataEnvelope { data: credit }).into_response()
        }
        Err(relay_billing::BillingError::InsufficientCredits { .. }) => access_error(
            StatusCode::CONFLICT,
            "relay_treasury_insufficient",
            "the treasury account does not hold enough funds for this credit",
        ),
        Err(error) => ledger_error_response(&error, "the credit could not be applied"),
    }
}

/// Map a Mint failure to a response without leaking transport internals.
fn ledger_error_response(error: &relay_billing::BillingError, fallback: &str) -> Response {
    match error {
        relay_billing::BillingError::InvalidAmount(message) => access_error(
            StatusCode::BAD_REQUEST,
            "relay_wallet_invalid_amount",
            message,
        ),
        relay_billing::BillingError::Store(message) if message.contains("(status 4") => {
            // Mint rejected the request (inactive provider, unknown account,
            // bad redirect URL): its message is actionable for the operator.
            let detail = message
                .split_once("): ")
                .map_or(message.as_str(), |(_, detail)| detail);
            access_error(StatusCode::BAD_REQUEST, "relay_wallet_rejected", detail)
        }
        other => {
            tracing::error!(error = %other, "Relay wallet operation failed");
            access_error(
                StatusCode::BAD_GATEWAY,
                "relay_wallet_unavailable",
                fallback,
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

    fn test_principal(public_id: &str) -> RequestApiKeyExtension {
        use chrono::Utc;
        use secrecy::SecretString;
        use tensorzero_auth::key::TensorZeroApiKey;
        use tensorzero_auth::postgres::KeyInfo;

        RequestApiKeyExtension {
            api_key: std::sync::Arc::new(TensorZeroApiKey {
                public_id: public_id.to_string(),
                hashed_long_key: SecretString::from("unused-in-this-test".to_string()),
            }),
            key_info: KeyInfo {
                public_id: public_id.to_string(),
                organization: "test-org".to_string(),
                workspace: "test-workspace".to_string(),
                description: None,
                created_at: Utc::now(),
                disabled_at: None,
                expires_at: None,
            },
        }
    }

    async fn status_through_middleware(principal: Option<RequestApiKeyExtension>) -> StatusCode {
        use axum::{Router, routing::get};
        use tower::ServiceExt as _;

        let app = Router::new()
            .route("/guarded", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn(require_operator_middleware));
        let mut request = axum::http::Request::builder()
            .uri("/guarded")
            .body(axum::body::Body::empty())
            .expect("build request");
        if let Some(principal) = principal {
            request.extensions_mut().insert(principal);
        }
        app.oneshot(request)
            .await
            .expect("router call should not fail")
            .status()
    }

    /// No `RequestApiKeyExtension` at all (gateway auth disabled, e.g. a
    /// single-tenant deployment or any e2e test that doesn't configure
    /// `[gateway.auth]`) must still reach the handler, exactly as it did
    /// before this gate existed — there's one tenant, nothing to isolate.
    #[tokio::test]
    async fn no_principal_passes_through_unchanged() {
        assert_eq!(status_through_middleware(None).await, StatusCode::OK);
    }

    /// An authenticated key that is not in `RELAY_OPERATOR_KEY_IDS` is the
    /// exact shape of a real Railyard product's own Relay key — it must be
    /// rejected, not handed every other tenant's data. This workspace forbids
    /// `unsafe_code`, and setting `RELAY_OPERATOR_KEY_IDS` from Rust requires
    /// an `unsafe` block (edition 2024's `std::env::set_var`), so this test
    /// relies on the (realistic, standard-for-a-test-process) precondition
    /// that it isn't already set rather than clearing it itself; the
    /// operator-allowed path is verified against the real deployed gateway
    /// instead (see the 2026-10-29 `plans/railyard.md` entry for this fix).
    #[tokio::test]
    async fn non_operator_principal_is_forbidden() {
        assert!(
            std::env::var(OPERATOR_KEYS_ENV).is_err(),
            "{OPERATOR_KEYS_ENV} must be unset for this test to mean anything"
        );
        let status = status_through_middleware(Some(test_principal("tenant-key-1"))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
