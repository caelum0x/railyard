//! The white-label kit: "your own OpenRouter" for AI founders.
//!
//! Vend + Relay + Mint wired so a founder can launch a branded LLM API:
//!
//! - **End-user wallets** ([`wallets`]): each of the founder's users gets a
//!   Mint account; `end_user_reserve` apps hold and capture every request on
//!   that user's own balance. Founders create wallets and hosted top-up links
//!   (Creem / Polar / PayPal / Stripe / iyzico / USDC via Mint checkouts)
//!   from their backend with a provisioning key, or from the console.
//! - **Per-model markup** ([`pricing_admin`]): override the app markup for any
//!   priced model; the data plane applies it to holds, captures and
//!   `/v1/models`.
//! - **Self-serve signup** ([`signup`]): workspace + first app + key + owner
//!   session in one call when the operator enables it.
//! - **Earnings and payouts** ([`earnings`]): the founder's captured revenue,
//!   margin and split-payout progress; the operator's platform-wide view.
//!
//! Spend caps are the existing per-key and per-end-user daily caps, enforced
//! atomically in the usage store.

pub mod earnings;
pub mod pricing_admin;
pub mod signup;
pub mod wallets;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use vend_core::envelope::{ErrorEnvelope, SuccessEnvelope};
use vend_core::error::VendError;
use vend_core::ids::TenantId;
use vend_core::ports::UsageRepo;
use vend_core::transport::MintTransport;
use vend_core::whitelabel::WhiteLabelRepo;
use vend_keys::KeyRepo;
use vend_store::{
    AuditActorKind, AuditOutcome, ControlPlaneRepo, CustomerPrincipal, CustomerRole, NewAuditEvent,
    SecurityOpsRepo,
};
use vend_tenancy::TenantRepo;

use crate::control::AdminAuth;
use crate::pricing::PricingCatalog;
use crate::security_ops::SecurityOpsConfig;
use crate::DataPlaneSecurity;

pub use signup::SignupConfig;

/// State for the provisioning-key wallet API (the founder's backend).
#[derive(Clone)]
pub struct WalletApiState {
    pub keys: Arc<dyn KeyRepo>,
    pub tenants: Arc<dyn TenantRepo>,
    pub usage: Arc<dyn UsageRepo>,
    pub mint: Arc<dyn MintTransport>,
    pub security: Option<DataPlaneSecurity>,
}

/// State for the console (customer session) and operator (admin token) APIs.
#[derive(Clone)]
pub struct ConsoleApiState {
    pub control: Arc<dyn ControlPlaneRepo>,
    pub whitelabel: Arc<dyn WhiteLabelRepo>,
    pub tenants: Arc<dyn TenantRepo>,
    pub mint: Arc<dyn MintTransport>,
    pub pricing: Arc<PricingCatalog>,
    pub admin: AdminAuth,
    pub signup: Option<SignupConfig>,
    pub security_repo: Arc<dyn SecurityOpsRepo>,
    pub security_config: SecurityOpsConfig,
    pub session_ttl_seconds: i64,
    pub setup_ttl_seconds: i64,
}

/// `POST /v1/wallets`, `GET /v1/wallets/:external_ref`,
/// `POST /v1/wallets/:external_ref/top-ups` (provisioning-key auth).
pub fn build_wallet_router(state: WalletApiState) -> Router {
    Router::new()
        .route("/v1/wallets", post(wallets::create_wallet))
        .route("/v1/wallets/:external_ref", get(wallets::get_wallet))
        .route(
            "/v1/wallets/:external_ref/top-ups",
            post(wallets::create_top_up),
        )
        .with_state(state)
}

/// Customer console, operator and signup routes of the white-label kit.
pub fn build_console_router(state: ConsoleApiState) -> Router {
    let customer = Router::new()
        .route(
            "/v1/customer/apps/:app_id/model-pricing",
            get(pricing_admin::customer_get)
                .put(pricing_admin::customer_put)
                .delete(pricing_admin::customer_delete),
        )
        .route(
            "/v1/customer/apps/:app_id/wallets",
            get(wallets::customer_list).post(wallets::customer_create),
        )
        .route(
            "/v1/customer/apps/:app_id/wallets/:external_ref/top-ups",
            post(wallets::customer_top_up),
        )
        .route("/v1/customer/earnings", get(earnings::customer_earnings))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_customer,
        ));
    let operator = Router::new()
        .route(
            "/v1/control/apps/:app_id/model-pricing",
            get(pricing_admin::operator_get)
                .put(pricing_admin::operator_put)
                .delete(pricing_admin::operator_delete),
        )
        .route("/v1/control/payouts", get(earnings::operator_payouts))
        .route(
            "/v1/control/tenants/:tenant_id/earnings",
            get(earnings::operator_tenant_earnings),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));
    let public = Router::new().route(
        "/v1/auth/signup",
        get(signup::signup_options).post(signup::signup),
    );
    customer.merge(operator).merge(public).with_state(state)
}

async fn require_admin(
    State(state): State<ConsoleApiState>,
    request: Request,
    next: Next,
) -> Response {
    if state.admin.authorizes(request.headers()) {
        next.run(request).await
    } else {
        fail(VendError::Unauthorized)
    }
}

async fn require_customer(
    State(state): State<ConsoleApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let principal = match token {
        Some(token) => state.control.authenticate_customer_session(&token).await,
        None => Ok(None),
    };
    match principal {
        Ok(Some(principal)) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Ok(None) => fail(VendError::Unauthorized),
        Err(error) => fail(error),
    }
}

/// `{success:true,data}` envelope.
pub(crate) fn ok<T: serde::Serialize>(value: T) -> Response {
    Json(SuccessEnvelope::new(value)).into_response()
}

/// Success envelope marked `Cache-Control: no-store` (secrets, checkout URLs).
pub(crate) fn ok_no_store<T: serde::Serialize>(value: T) -> Response {
    let mut response = ok(value);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

/// `{success:false,error}` envelope with the error's HTTP status. Storage and
/// transport details are never echoed (see `VendError::client_message`).
pub(crate) fn fail(error: VendError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if matches!(error, VendError::Storage(_) | VendError::Transport(_)) {
        tracing::error!(%error, "white-label request failed");
    }
    (status, Json(ErrorEnvelope::from_error(&error))).into_response()
}

#[allow(clippy::result_large_err)] // handlers return the concrete response directly
pub(crate) fn require_role(
    principal: &CustomerPrincipal,
    required: CustomerRole,
) -> Result<(), Response> {
    if principal.role.allows(required) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({"success": false, "error": {"code": "forbidden", "message": "forbidden"}})),
        )
            .into_response())
    }
}

/// Append a secret-free audit row; failures are logged, never surfaced.
pub(crate) async fn audit(
    repo: &Arc<dyn SecurityOpsRepo>,
    tenant_id: Option<TenantId>,
    actor: (AuditActorKind, Option<String>),
    action: &str,
    target: (&str, &str),
    outcome: AuditOutcome,
    metadata: Value,
) {
    let event = NewAuditEvent {
        tenant_id,
        actor_kind: actor.0,
        actor_id: actor.1,
        action: action.to_string(),
        target_kind: Some(target.0.to_string()),
        target_id: Some(target.1.to_string()),
        outcome,
        request_id: None,
        source_hash: None,
        metadata,
    };
    if let Err(error) = repo.append_audit_event(event).await {
        tracing::error!(%error, action, "white-label audit append failed");
    }
}
