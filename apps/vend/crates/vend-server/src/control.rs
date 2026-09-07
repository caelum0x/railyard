//! Authenticated operator control plane for tenant products, apps, keys, end
//! users, and daily spend. Production uses [`vend_store::VendStore`]; no route
//! has a permissive or development authentication fallback.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vend_core::envelope::{ErrorEnvelope, SuccessEnvelope};
use vend_core::error::VendError;
use vend_core::ids::{AppId, SubKeyId, TenantId};
use vend_core::money::MarkupPolicy;
use vend_core::transport::{CheckoutRequest, MintTransport};
use vend_keys::KeyEnv;
use vend_store::{
    AuditActorKind, AuditOutcome, BootstrapConfig, ControlPlaneRepo, CreateApp, CreateAppKey,
    CustomerPrincipal, CustomerRole, NewAuditEvent, OAuthStart, SecurityOpsRepo,
    UpdateCustomerUser,
};

use crate::security_ops::{clear_rate_limit, enforce_rate_limit, SecurityOpsConfig};

/// SHA-256 digest of a high-entropy administrator bearer token.
#[derive(Clone)]
pub struct AdminAuth {
    current: [u8; 32],
    next: Option<[u8; 32]>,
}

impl AdminAuth {
    /// Parse exactly 64 lowercase or uppercase hex characters. Missing,
    /// malformed, or all-zero configuration refuses server startup.
    pub fn from_sha256_hex(value: &str) -> Result<Self, VendError> {
        Self::from_sha256_hexes(value, None)
    }

    /// Configure an overlap window for zero-downtime rotation. Deploy `next`,
    /// move operator clients, then promote it to `current` and remove `next`.
    /// Both candidates are always compared to avoid a timing distinction.
    pub fn from_sha256_hexes(value: &str, next: Option<&str>) -> Result<Self, VendError> {
        let current = parse_admin_digest(value, "VEND_ADMIN_TOKEN_SHA256")?;
        let next = next
            .map(|value| parse_admin_digest(value, "VEND_ADMIN_TOKEN_NEXT_SHA256"))
            .transpose()?;
        if next.is_some_and(|next| next.ct_eq(&current).unwrap_u8() == 1) {
            return Err(VendError::Config(
                "current and next administrator token digests must differ".into(),
            ));
        }
        Ok(Self { current, next })
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(secret) = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|secret| !secret.is_empty())
        else {
            return false;
        };
        let actual: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let current = self.current.ct_eq(&actual).unwrap_u8();
        let next_digest = self.next.unwrap_or([0_u8; 32]);
        let next = next_digest.ct_eq(&actual).unwrap_u8() & self.next.is_some() as u8;
        (current | next) == 1
    }
}

fn parse_admin_digest(value: &str, name: &str) -> Result<[u8; 32], VendError> {
    let value = value.trim();
    if value.len() != 64 {
        return Err(VendError::Config(format!(
            "{name} must be a 64-character SHA-256 hex digest"
        )));
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(pair[0]).ok_or_else(|| invalid_admin_digest(name))?;
        let low = hex_digit(pair[1]).ok_or_else(|| invalid_admin_digest(name))?;
        digest[index] = (high << 4) | low;
    }
    if digest.ct_eq(&[0_u8; 32]).unwrap_u8() == 1 {
        return Err(invalid_admin_digest(name));
    }
    Ok(digest)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_admin_digest(name: &str) -> VendError {
    VendError::Config(format!("{name} must contain non-zero hexadecimal bytes"))
}

#[derive(Clone)]
struct ControlState {
    repo: Arc<dyn ControlPlaneRepo>,
    security_repo: Arc<dyn SecurityOpsRepo>,
    security_config: SecurityOpsConfig,
    customer_auth: CustomerAuthConfig,
    mint: Arc<dyn MintTransport>,
}

#[derive(Clone)]
pub struct OAuthProviderConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Runtime settings for Vend-owned customer identity. Provider credentials are
/// optional as a pair; an unconfigured provider route fails closed with 503.
#[derive(Clone)]
pub struct CustomerAuthConfig {
    public_base_url: reqwest::Url,
    pub github: Option<OAuthProviderConfig>,
    pub google: Option<OAuthProviderConfig>,
    pub session_ttl_seconds: i64,
    pub setup_ttl_seconds: i64,
    pub oauth_state_ttl_seconds: i64,
    http: reqwest::Client,
}

impl CustomerAuthConfig {
    pub fn new(
        public_base_url: &str,
        github: Option<OAuthProviderConfig>,
        google: Option<OAuthProviderConfig>,
        session_ttl_seconds: i64,
        setup_ttl_seconds: i64,
    ) -> Result<Self, VendError> {
        let mut base = reqwest::Url::parse(public_base_url)
            .map_err(|error| VendError::Config(format!("invalid VEND_PUBLIC_BASE_URL: {error}")))?;
        if base.query().is_some()
            || base.fragment().is_some()
            || !matches!(base.scheme(), "http" | "https")
            || (base.scheme() == "http"
                && !matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        {
            return Err(VendError::Config(
                "VEND_PUBLIC_BASE_URL must be HTTPS (HTTP is allowed only for localhost) and have no query or fragment".into(),
            ));
        }
        let normalized_path = base.path().trim_end_matches('/').to_string();
        base.set_path(&normalized_path);
        if !(60..=30 * 24 * 60 * 60).contains(&session_ttl_seconds)
            || !(60..=30 * 24 * 60 * 60).contains(&setup_ttl_seconds)
        {
            return Err(VendError::Config(
                "customer session/setup TTL must be between 60 seconds and 30 days".into(),
            ));
        }
        Ok(Self {
            public_base_url: base,
            github,
            google,
            session_ttl_seconds,
            setup_ttl_seconds,
            oauth_state_ttl_seconds: 600,
            http: reqwest::Client::builder()
                .connect_timeout(StdDuration::from_secs(5))
                .timeout(StdDuration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| VendError::Config(format!("OAuth HTTP client: {error}")))?,
        })
    }

    fn callback_url(&self, provider: &str) -> String {
        format!(
            "{}/v1/auth/{provider}/callback",
            self.public_base_url.as_str().trim_end_matches('/')
        )
    }
}

/// Build the operator surface. Every route, including its OpenAPI document, is
/// protected by the same fail-closed bearer middleware.
pub fn build_control_router(
    repo: Arc<dyn ControlPlaneRepo>,
    security_repo: Arc<dyn SecurityOpsRepo>,
    auth: AdminAuth,
    customer_auth: CustomerAuthConfig,
    security_config: SecurityOpsConfig,
    mint: Arc<dyn MintTransport>,
) -> Router {
    let state = ControlState {
        repo,
        security_repo,
        security_config,
        customer_auth,
        mint,
    };
    let admin = Router::new()
        .route("/v1/control/openapi.json", get(openapi))
        .route("/v1/control/tenants", get(list_tenants).post(bootstrap))
        .route("/v1/control/tenants/:tenant_id", get(get_tenant))
        .route(
            "/v1/control/tenants/:tenant_id/apps",
            get(list_apps).post(create_app),
        )
        .route("/v1/control/apps/:app_id", get(get_app))
        .route("/v1/control/apps/:app_id/enabled", patch(set_app_enabled))
        .route(
            "/v1/control/apps/:app_id/keys",
            get(list_app_keys).post(issue_app_key),
        )
        .route(
            "/v1/control/apps/:app_id/end-users",
            post(provision_end_user),
        )
        .route("/v1/control/apps/:app_id/usage", get(app_usage))
        .route("/v1/control/keys/:key_id", delete(revoke_key))
        .route(
            "/v1/control/tenants/:tenant_id/users",
            get(admin_list_users).post(admin_invite_user),
        )
        .route(
            "/v1/control/tenants/:tenant_id/users/:user_id",
            patch(admin_update_user),
        )
        .route(
            "/v1/control/tenants/:tenant_id/users/:user_id/sessions",
            delete(admin_revoke_user_sessions),
        )
        .route_layer(middleware::from_fn_with_state(auth, require_admin))
        .with_state(state.clone());
    let customer = Router::new()
        .route("/v1/customer/openapi.json", get(openapi))
        .route("/v1/customer/tenant", get(customer_tenant))
        .route(
            "/v1/customer/apps",
            get(customer_list_apps).post(customer_create_app),
        )
        .route("/v1/customer/apps/:app_id", get(customer_get_app))
        .route(
            "/v1/customer/apps/:app_id/funding-checkout",
            post(customer_funding_checkout),
        )
        .route(
            "/v1/customer/apps/:app_id/enabled",
            patch(customer_set_app_enabled),
        )
        .route(
            "/v1/customer/apps/:app_id/keys",
            get(customer_list_app_keys).post(customer_issue_app_key),
        )
        .route(
            "/v1/customer/apps/:app_id/end-users",
            post(customer_provision_end_user),
        )
        .route("/v1/customer/apps/:app_id/usage", get(customer_app_usage))
        .route("/v1/customer/keys/:key_id", delete(customer_revoke_key))
        .route(
            "/v1/customer/users",
            get(customer_list_users).post(customer_invite_user),
        )
        .route("/v1/customer/users/:user_id", patch(customer_update_user))
        .route(
            "/v1/customer/users/:user_id/sessions",
            delete(customer_revoke_user_sessions),
        )
        .route("/v1/customer/logout", post(customer_logout))
        .route("/v1/customer/session/rotate", post(customer_rotate_session))
        .route("/v1/customer/password", patch(customer_change_password))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_customer,
        ))
        .with_state(state.clone());
    let native_auth = Router::new()
        .route("/v1/auth/setup", post(customer_setup))
        .route("/v1/auth/login", post(customer_login))
        .with_state(state.clone());
    let oauth = Router::new()
        .route("/v1/auth/github/start", get(github_start))
        .route("/v1/auth/github/callback", get(github_callback))
        .route("/v1/auth/google/start", get(google_start))
        .route("/v1/auth/google/callback", get(google_callback))
        .route_layer(middleware::from_fn(referrer_policy))
        .with_state(state.clone());
    admin
        .merge(customer)
        .merge(native_auth)
        .merge(oauth)
        .layer(middleware::from_fn_with_state(state, audit_control_request))
}

async fn audit_control_request(
    State(state): State<ControlState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let descriptor = audit_route_descriptor(method.as_str(), &path);
    if descriptor.is_none() {
        return next.run(request).await;
    }
    let customer = if path.starts_with("/v1/customer/") {
        match request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        {
            Some(token) => state
                .repo
                .authenticate_customer_session(token)
                .await
                .ok()
                .flatten(),
            None => None,
        }
    } else {
        None
    };
    let tenant_id = match &customer {
        Some(principal) => Some(principal.tenant_id.clone()),
        None => resolve_audit_tenant(state.repo.as_ref(), &path).await,
    };
    let response = next.run(request).await;
    let (action, target_kind, target_id) = descriptor.expect("checked above");
    let outcome = if response.status().is_success() || response.status().is_redirection() {
        AuditOutcome::Succeeded
    } else if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
    ) {
        AuditOutcome::Denied
    } else {
        AuditOutcome::Failed
    };
    let event = NewAuditEvent {
        tenant_id,
        actor_kind: if customer.is_some() {
            AuditActorKind::Customer
        } else if path.starts_with("/v1/control/") {
            AuditActorKind::Administrator
        } else {
            AuditActorKind::Customer
        },
        actor_id: customer.as_ref().map(|principal| principal.user_id.clone()),
        action,
        target_kind: Some(target_kind),
        target_id,
        outcome,
        request_id: None,
        source_hash: None,
        metadata: json!({"http_method": method.as_str()}),
    };
    if let Err(error) = state.security_repo.append_audit_event(event).await {
        tracing::error!(%error, "failed to append control-plane audit event");
    }
    response
}

async fn resolve_audit_tenant(repo: &dyn ControlPlaneRepo, path: &str) -> Option<TenantId> {
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if let Some(raw) = segments
        .iter()
        .position(|part| *part == "tenants")
        .and_then(|index| segments.get(index + 1))
    {
        if let Ok(tenant) = TenantId::parse(raw) {
            return Some(tenant);
        }
    }
    if let Some(raw) = segments
        .iter()
        .position(|part| *part == "apps")
        .and_then(|index| segments.get(index + 1))
    {
        if let Ok(app_id) = AppId::parse(raw) {
            if let Ok(Some(app)) = repo.get_app(&app_id).await {
                return Some(app.tenant_id);
            }
        }
    }
    if let Some(raw) = segments
        .iter()
        .position(|part| *part == "keys")
        .and_then(|index| segments.get(index + 1))
    {
        if let Ok(key_id) = SubKeyId::parse(raw) {
            if let Ok(Some(key)) = repo.get_key(&key_id).await {
                return Some(key.tenant_id);
            }
        }
    }
    None
}

fn audit_route_descriptor(method: &str, path: &str) -> Option<(String, String, Option<String>)> {
    if !path.starts_with("/v1/control/") && !path.starts_with("/v1/customer/") {
        return None;
    }
    // These two mutations append in the same database transaction as the
    // credential change, so a second best-effort row would be misleading.
    if path.ends_with("/session/rotate") || path.ends_with("/password") {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let (resource, marker) = if path.ends_with("/funding-checkout") {
        ("funding_checkout", "apps")
    } else if path.contains("/keys/") || path.ends_with("/keys") {
        ("provisioning_key", "keys")
    } else if path.contains("/users/") || path.ends_with("/users") {
        ("membership", "users")
    } else if path.contains("/end-users") {
        ("end_user", "end-users")
    } else if path.contains("/apps/") || path.ends_with("/apps") {
        ("app", "apps")
    } else if path.ends_with("/usage") {
        ("usage", "usage")
    } else if path.contains("/tenants/") || path.ends_with("/tenants") {
        ("tenant", "tenants")
    } else if path.ends_with("/logout") {
        ("customer_session", "logout")
    } else {
        ("control_plane", "control")
    };
    let target_id = segments
        .iter()
        .position(|part| *part == marker)
        .and_then(|index| segments.get(index + 1))
        .filter(|value| !matches!(**value, "enabled" | "keys" | "users" | "usage"))
        .map(|value| (*value).to_string());
    Some((
        format!("control.{}.{}", method.to_ascii_lowercase(), resource),
        resource.to_string(),
        target_id,
    ))
}

async fn referrer_policy(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = no_referrer(next.run(request).await);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

async fn require_admin(
    State(auth): State<AdminAuth>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if auth.authorizes(request.headers()) {
        next.run(request).await
    } else {
        control_error(VendError::Unauthorized)
    }
}

async fn require_customer(
    State(state): State<ControlState>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty());
    let principal = match token {
        Some(token) => state.repo.authenticate_customer_session(token).await,
        None => Ok(None),
    };
    match principal {
        Ok(Some(principal)) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Ok(None) => control_error(VendError::Unauthorized),
        Err(error) => control_error(error),
    }
}

#[allow(clippy::result_large_err)] // Axum handlers return the concrete response directly.
fn require_role(principal: &CustomerPrincipal, required: CustomerRole) -> Result<(), Response> {
    if principal.role.allows(required) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({"success":false,"error":{"code":"forbidden","message":"forbidden"}})),
        )
            .into_response())
    }
}

fn control_error(error: VendError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(ErrorEnvelope::from_error(&error))).into_response()
}

fn result<T: serde::Serialize>(value: Result<T, VendError>) -> Response {
    match value {
        Ok(value) => Json(SuccessEnvelope::new(value)).into_response(),
        Err(error) => control_error(error),
    }
}

fn secret_result<T: serde::Serialize>(value: Result<T, VendError>) -> Response {
    let mut response = result(value);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

fn no_referrer(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn audit_auth_outcome(
    state: &ControlState,
    tenant_id: Option<TenantId>,
    actor_id: Option<String>,
    action: &str,
    succeeded: bool,
    target_kind: Option<&str>,
    target_id: Option<&str>,
) {
    let event = NewAuditEvent {
        tenant_id,
        actor_kind: AuditActorKind::Customer,
        actor_id,
        action: action.to_string(),
        target_kind: target_kind.map(str::to_string),
        target_id: target_id.map(str::to_string),
        outcome: if succeeded {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Denied
        },
        request_id: None,
        source_hash: None,
        metadata: json!({}),
    };
    if let Err(error) = state.security_repo.append_audit_event(event).await {
        // Authentication must not issue a one-time bearer and then replace the
        // response with a 5xx that loses it. Atomic audit is used for session
        // and password rotation; provider/native login events are best-effort
        // until those flows are moved into one store transaction.
        tracing::error!(%error, action, "failed to append authentication audit event");
    }
}

#[derive(Deserialize)]
struct BootstrapRequest {
    tenant_name: String,
    owner_email: String,
    owner_display_name: String,
    app_name: String,
    mint_account_id: String,
    #[serde(default)]
    model_allowlist: Vec<String>,
    #[serde(default)]
    zdr: bool,
    markup_policy: MarkupPolicy,
    #[serde(default = "live_environment")]
    environment: String,
    daily_cap_micros: i64,
    per_end_user_daily_cap_micros: Option<i64>,
}

fn live_environment() -> String {
    "live".into()
}

fn key_env(value: &str) -> Result<KeyEnv, VendError> {
    match value {
        "live" => Ok(KeyEnv::Live),
        "test" => Ok(KeyEnv::Test),
        _ => Err(VendError::InvalidAmount),
    }
}

async fn bootstrap(
    State(state): State<ControlState>,
    Json(input): Json<BootstrapRequest>,
) -> Response {
    let environment = match key_env(&input.environment) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    let created = state
        .repo
        .bootstrap(BootstrapConfig {
            tenant_name: input.tenant_name,
            app_name: input.app_name,
            mint_account_id: input.mint_account_id,
            model_allowlist: input.model_allowlist,
            zdr: input.zdr,
            markup_policy: input.markup_policy,
            key_env: environment,
            daily_cap_micros: Some(input.daily_cap_micros),
            per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros,
            owner_email: input.owner_email,
            owner_display_name: input.owner_display_name,
            setup_token_ttl_seconds: state.customer_auth.setup_ttl_seconds,
        })
        .await;
    secret_result(created.map(|output| {
        json!({
            "tenant_id": output.tenant_id,
            "app_id": output.app_id,
            "key_id": output.sub_key_id,
            "provisioning_key": output.secret,
            "owner": output.owner,
            "owner_setup_token": output.owner_setup_token,
            "warning": "Store both one-time secrets now. Deliver owner_setup_token out of band; Vend sends no email."
        })
    }))
}

async fn list_tenants(State(state): State<ControlState>) -> Response {
    result(state.repo.list_tenants().await)
}

async fn get_tenant(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let id = match TenantId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .get_tenant(&id)
            .await
            .and_then(|value| value.ok_or(VendError::NotFound)),
    )
}

async fn list_apps(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let id = match TenantId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    match state.repo.get_tenant(&id).await {
        Ok(Some(_)) => {}
        Ok(None) => return control_error(VendError::NotFound),
        Err(error) => return control_error(error),
    }
    result(state.repo.list_apps(&id).await)
}

async fn create_app(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Json(input): Json<CreateApp>,
) -> Response {
    let id = match TenantId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(state.repo.create_app(&id, input).await)
}

async fn get_app(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .get_app(&id)
            .await
            .and_then(|value| value.ok_or(VendError::NotFound)),
    )
}

#[derive(Deserialize)]
struct EnabledRequest {
    enabled: bool,
}

async fn set_app_enabled(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Json(input): Json<EnabledRequest>,
) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(state.repo.set_app_enabled(&id, input.enabled).await)
}

async fn list_app_keys(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    match state.repo.get_app(&id).await {
        Ok(Some(_)) => {}
        Ok(None) => return control_error(VendError::NotFound),
        Err(error) => return control_error(error),
    }
    result(state.repo.list_app_keys(&id).await)
}

#[derive(Deserialize)]
struct IssueKeyRequest {
    #[serde(default = "live_environment")]
    environment: String,
    daily_cap_micros: i64,
    per_end_user_daily_cap_micros: Option<i64>,
}

async fn issue_app_key(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Json(input): Json<IssueKeyRequest>,
) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    let environment = match key_env(&input.environment) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .issue_app_key(
                &id,
                CreateAppKey {
                    env: environment,
                    daily_cap_micros: Some(input.daily_cap_micros),
                    per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros,
                },
            )
            .await,
    )
}

#[derive(Deserialize)]
struct ProvisionEndUserRequest {
    external_ref: String,
    mint_account_id: String,
}

async fn provision_end_user(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Json(input): Json<ProvisionEndUserRequest>,
) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .provision_end_user(&id, &input.external_ref, &input.mint_account_id)
            .await,
    )
}

async fn revoke_key(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let id = match SubKeyId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    result(state.repo.revoke_key(&id).await)
}

#[derive(Deserialize)]
struct UsageQuery {
    utc_day: Option<String>,
}

async fn app_usage(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let id = match AppId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    let utc_day = query
        .utc_day
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    if NaiveDate::parse_from_str(&utc_day, "%Y-%m-%d").is_err() {
        return control_error(VendError::BadId);
    }
    result(state.repo.app_usage(&id, &utc_day).await)
}

#[derive(Deserialize)]
struct InviteUserRequest {
    email: String,
    display_name: String,
    role: CustomerRole,
}

#[derive(Deserialize)]
struct UpdateUserRequest {
    role: Option<CustomerRole>,
    disabled: Option<bool>,
}

async fn admin_list_users(State(state): State<ControlState>, Path(raw): Path<String>) -> Response {
    let tenant = match TenantId::parse(&raw) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    result(state.repo.list_customer_users(&tenant).await)
}

async fn admin_invite_user(
    State(state): State<ControlState>,
    Path(raw): Path<String>,
    Json(input): Json<InviteUserRequest>,
) -> Response {
    let tenant = match TenantId::parse(&raw) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    secret_result(
        state
            .repo
            .invite_customer_user(
                &tenant,
                &input.email,
                &input.display_name,
                input.role,
                state.customer_auth.setup_ttl_seconds,
            )
            .await,
    )
}

async fn admin_update_user(
    State(state): State<ControlState>,
    Path((tenant_raw, user_id)): Path<(String, String)>,
    Json(input): Json<UpdateUserRequest>,
) -> Response {
    let tenant = match TenantId::parse(&tenant_raw) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .update_customer_user(
                &tenant,
                &user_id,
                UpdateCustomerUser {
                    role: input.role,
                    disabled: input.disabled,
                },
            )
            .await,
    )
}

async fn admin_revoke_user_sessions(
    State(state): State<ControlState>,
    Path((tenant_raw, user_id)): Path<(String, String)>,
) -> Response {
    let tenant = match TenantId::parse(&tenant_raw) {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .revoke_customer_sessions(&tenant, &user_id)
            .await
            .map(|revoked_sessions| json!({"revoked_sessions": revoked_sessions})),
    )
}

async fn customer_tenant(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
) -> Response {
    result(
        state
            .repo
            .get_tenant(&principal.tenant_id)
            .await
            .and_then(|tenant| tenant.ok_or(VendError::NotFound)),
    )
}

async fn customer_list_apps(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
) -> Response {
    result(state.repo.list_apps(&principal.tenant_id).await)
}

async fn customer_create_app(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Json(input): Json<CreateApp>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    result(state.repo.create_app(&principal.tenant_id, input).await)
}

async fn owned_app(
    state: &ControlState,
    principal: &CustomerPrincipal,
    raw: &str,
) -> Result<vend_store::AppSummary, VendError> {
    let id = AppId::parse(raw)?;
    let app = state.repo.get_app(&id).await?.ok_or(VendError::NotFound)?;
    if app.tenant_id != principal.tenant_id {
        return Err(VendError::NotFound);
    }
    Ok(app)
}

async fn customer_get_app(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
) -> Response {
    result(owned_app(&state, &principal, &raw).await)
}

#[derive(Deserialize)]
struct FundingCheckoutRequest {
    provider: String,
    amount_micros: i64,
}

async fn customer_funding_checkout(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<FundingCheckoutRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    if input.amount_micros <= 0 || input.amount_micros % 10_000 != 0 {
        return control_error(VendError::InvalidAmount);
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    secret_result(
        state
            .mint
            .create_checkout(CheckoutRequest {
                provider: input.provider,
                account_id: app.mint_account_id,
                amount_micros: input.amount_micros,
            })
            .await,
    )
}

async fn customer_set_app_enabled(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<EnabledRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    result(state.repo.set_app_enabled(&app.id, input.enabled).await)
}

async fn customer_list_app_keys(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
) -> Response {
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    result(state.repo.list_app_keys(&app.id).await)
}

async fn customer_issue_app_key(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<IssueKeyRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    let env = match key_env(&input.environment) {
        Ok(env) => env,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .issue_app_key(
                &app.id,
                CreateAppKey {
                    env,
                    daily_cap_micros: Some(input.daily_cap_micros),
                    per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros,
                },
            )
            .await,
    )
}

async fn customer_provision_end_user(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<ProvisionEndUserRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    result(
        state
            .repo
            .provision_end_user(&app.id, &input.external_ref, &input.mint_account_id)
            .await,
    )
}

async fn customer_app_usage(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return control_error(error),
    };
    let day = query
        .utc_day
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    if NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err() {
        return control_error(VendError::BadId);
    }
    result(state.repo.app_usage(&app.id, &day).await)
}

async fn customer_revoke_key(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let id = match SubKeyId::parse(&raw) {
        Ok(id) => id,
        Err(error) => return control_error(error),
    };
    let key = match state.repo.get_key(&id).await {
        Ok(Some(key)) if key.tenant_id == principal.tenant_id => key,
        Ok(_) => return control_error(VendError::NotFound),
        Err(error) => return control_error(error),
    };
    result(state.repo.revoke_key(&key.id).await)
}

async fn customer_list_users(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Owner) {
        return response;
    }
    result(state.repo.list_customer_users(&principal.tenant_id).await)
}

async fn customer_invite_user(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Json(input): Json<InviteUserRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Owner) {
        return response;
    }
    secret_result(
        state
            .repo
            .invite_customer_user(
                &principal.tenant_id,
                &input.email,
                &input.display_name,
                input.role,
                state.customer_auth.setup_ttl_seconds,
            )
            .await,
    )
}

async fn customer_update_user(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(user_id): Path<String>,
    Json(input): Json<UpdateUserRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Owner) {
        return response;
    }
    result(
        state
            .repo
            .update_customer_user(
                &principal.tenant_id,
                &user_id,
                UpdateCustomerUser {
                    role: input.role,
                    disabled: input.disabled,
                },
            )
            .await,
    )
}

async fn customer_revoke_user_sessions(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(user_id): Path<String>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Owner) {
        return response;
    }
    result(
        state
            .repo
            .revoke_customer_sessions(&principal.tenant_id, &user_id)
            .await
            .map(|revoked_sessions| json!({"revoked_sessions":revoked_sessions})),
    )
}

async fn customer_logout(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
) -> Response {
    result(
        state
            .repo
            .logout_customer(&principal)
            .await
            .map(|()| json!({"logged_out":true})),
    )
}

async fn customer_rotate_session(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
) -> Response {
    secret_result(
        state
            .security_repo
            .rotate_customer_session(&principal, state.customer_auth.session_ttl_seconds)
            .await,
    )
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

async fn customer_change_password(
    State(state): State<ControlState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Json(input): Json<ChangePasswordRequest>,
) -> Response {
    secret_result(
        state
            .security_repo
            .change_customer_password(
                &principal,
                &input.current_password,
                &input.new_password,
            )
            .await
            .map(|other_sessions_revoked| {
                json!({"password_changed":true,"other_sessions_revoked":other_sessions_revoked})
            }),
    )
}

#[derive(Deserialize)]
struct SetupRequest {
    setup_token: String,
    password: String,
}

async fn customer_setup(
    State(state): State<ControlState>,
    Json(input): Json<SetupRequest>,
) -> Response {
    let bucket = "setup_token";
    let rate_subject = opaque_bearer_shard(&input.setup_token, "vset");
    if let Err(response) = enforce_rate_limit(
        &state.security_repo,
        &state.security_config,
        bucket,
        "global",
        &rate_subject,
        state.security_config.setup_token,
    )
    .await
    {
        return response;
    }
    let output = state
        .repo
        .setup_customer_password(&input.setup_token, &input.password)
        .await;
    if output.is_ok() {
        if let Err(error) = clear_rate_limit(
            &state.security_repo,
            &state.security_config,
            bucket,
            "global",
            &rate_subject,
        )
        .await
        {
            tracing::error!(%error, "failed to clear successful setup rate limit");
        }
    }
    audit_auth_outcome(
        &state,
        output.as_ref().ok().map(|user| user.tenant_id.clone()),
        output.as_ref().ok().map(|user| user.id.clone()),
        "auth.password_setup",
        output.is_ok(),
        Some("customer_user"),
        output.as_ref().ok().map(|user| user.id.as_str()),
    )
    .await;
    secret_result(output)
}

fn opaque_bearer_shard(token: &str, kind: &str) -> String {
    let mut parts = token.splitn(3, '_');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(actual_kind), Some(prefix), Some(_))
            if actual_kind == kind
                && prefix.len() == 16
                && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            format!("{kind}_{prefix}")
        }
        _ => "malformed".into(),
    }
}

#[derive(Deserialize)]
struct LoginRequest {
    tenant_id: String,
    email: String,
    password: String,
}

async fn customer_login(
    State(state): State<ControlState>,
    Json(input): Json<LoginRequest>,
) -> Response {
    let tenant = match TenantId::parse(&input.tenant_id) {
        Ok(tenant) => tenant,
        Err(_) => return control_error(VendError::Unauthorized),
    };
    let subject = input.email.trim().to_ascii_lowercase();
    let bucket = "native_login";
    if let Err(response) = enforce_rate_limit(
        &state.security_repo,
        &state.security_config,
        bucket,
        tenant.as_str(),
        &subject,
        state.security_config.native_login,
    )
    .await
    {
        return response;
    }
    let output = state
        .repo
        .login_customer(
            &tenant,
            &input.email,
            &input.password,
            state.customer_auth.session_ttl_seconds,
        )
        .await;
    if output.is_ok() {
        if let Err(error) = clear_rate_limit(
            &state.security_repo,
            &state.security_config,
            bucket,
            tenant.as_str(),
            &subject,
        )
        .await
        {
            tracing::error!(%error, "failed to clear successful login rate limit");
        }
    }
    audit_auth_outcome(
        &state,
        Some(tenant),
        output.as_ref().ok().map(|session| session.user.id.clone()),
        "auth.native_login",
        output.is_ok(),
        Some("customer_session"),
        output
            .as_ref()
            .ok()
            .map(|session| session.session_id.as_str()),
    )
    .await;
    secret_result(output)
}

#[derive(Deserialize)]
struct OAuthStartQuery {
    tenant_id: String,
}

fn provider_unavailable(provider: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"success":false,"error":{"code":"provider_unavailable","message":format!("{provider} login is not configured")}})),
    )
        .into_response()
}

async fn oauth_start(state: ControlState, query: OAuthStartQuery, provider: &str) -> Response {
    let config = match provider {
        "github" => state.customer_auth.github.as_ref(),
        "google" => state.customer_auth.google.as_ref(),
        _ => None,
    };
    let Some(config) = config else {
        return provider_unavailable(provider);
    };
    let tenant = match TenantId::parse(&query.tenant_id) {
        Ok(tenant) => tenant,
        Err(error) => return control_error(error),
    };
    if let Err(response) = enforce_rate_limit(
        &state.security_repo,
        &state.security_config,
        "oauth_start",
        tenant.as_str(),
        provider,
        state.security_config.oauth_start,
    )
    .await
    {
        return response;
    }
    let flow = match state
        .repo
        .create_oauth_state(
            &tenant,
            provider,
            state.customer_auth.oauth_state_ttl_seconds,
        )
        .await
    {
        Ok(flow) => flow,
        Err(error) => return control_error(error),
    };
    let url = match oauth_authorize_url(
        provider,
        config,
        &state.customer_auth.callback_url(provider),
        &flow,
    ) {
        Ok(url) => url,
        Err(error) => return control_error(error),
    };
    let location = match HeaderValue::from_str(url.as_str()) {
        Ok(value) => value,
        Err(_) => return control_error(VendError::Storage("invalid OAuth redirect".into())),
    };
    audit_auth_outcome(
        &state,
        Some(tenant),
        None,
        &format!("auth.{provider}_oauth_started"),
        true,
        Some("oauth_flow"),
        None,
    )
    .await;
    (
        StatusCode::SEE_OTHER,
        [(axum::http::header::LOCATION, location)],
    )
        .into_response()
}

fn oauth_authorize_url(
    provider: &str,
    config: &OAuthProviderConfig,
    callback: &str,
    flow: &OAuthStart,
) -> Result<reqwest::Url, VendError> {
    let endpoint = if provider == "github" {
        "https://github.com/login/oauth/authorize"
    } else if provider == "google" {
        "https://accounts.google.com/o/oauth2/v2/auth"
    } else {
        return Err(VendError::InvalidAmount);
    };
    let mut url = reqwest::Url::parse(endpoint).expect("static OAuth URL");
    {
        let mut pairs = url.query_pairs_mut();
        pairs
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", callback)
            .append_pair("response_type", "code")
            .append_pair("state", &flow.state)
            .append_pair("code_challenge", &flow.code_challenge)
            .append_pair("code_challenge_method", "S256");
        if provider == "github" {
            pairs.append_pair("scope", "read:user user:email");
        } else {
            pairs
                .append_pair("scope", "openid email")
                .append_pair("nonce", &flow.nonce);
        }
    }
    Ok(url)
}

async fn github_start(
    State(state): State<ControlState>,
    Query(query): Query<OAuthStartQuery>,
) -> Response {
    oauth_start(state, query, "github").await
}

async fn google_start(
    State(state): State<ControlState>,
    Query(query): Query<OAuthStartQuery>,
) -> Response {
    oauth_start(state, query, "google").await
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    id: i64,
}

#[derive(Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

async fn github_callback(
    State(state): State<ControlState>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    let Some(config) = state.customer_auth.github.as_ref() else {
        return provider_unavailable("github");
    };
    if query.error.is_some() {
        return control_error(VendError::Unauthorized);
    }
    let (code, oauth_state) = match (query.code, query.state) {
        (Some(code), Some(oauth_state)) => (code, oauth_state),
        _ => return control_error(VendError::Unauthorized),
    };
    let rate_subject = opaque_bearer_shard(&oauth_state, "vost");
    if let Err(response) = enforce_rate_limit(
        &state.security_repo,
        &state.security_config,
        "oauth_callback",
        "github",
        &rate_subject,
        state.security_config.oauth_callback,
    )
    .await
    {
        return response;
    }
    let consumed = match state.repo.consume_oauth_state(&oauth_state, "github").await {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    let token: OAuthTokenResponse = match state
        .customer_auth
        .http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code.as_str()),
            (
                "redirect_uri",
                state.customer_auth.callback_url("github").as_str(),
            ),
            ("code_verifier", consumed.code_verifier.as_str()),
        ])
        .send()
        .await
        .and_then(|response| response.error_for_status())
    {
        Ok(response) => match response.json().await {
            Ok(value) => value,
            Err(error) => return control_error(VendError::Transport(error.to_string())),
        },
        Err(error) => return control_error(VendError::Transport(error.to_string())),
    };
    if token.error.is_some() {
        return control_error(VendError::Unauthorized);
    }
    let Some(access_token) = token.access_token else {
        return control_error(VendError::Unauthorized);
    };
    let github_user: GithubUser = match github_get(&state, &access_token, "/user").await {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    let emails: Vec<GithubEmail> = match github_get(&state, &access_token, "/user/emails").await {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    let Some(email) = select_github_email(emails) else {
        return control_error(VendError::Unauthorized);
    };
    let output = state
        .repo
        .login_customer_external(
            &consumed.tenant_id,
            "github",
            &github_user.id.to_string(),
            &email,
            state.customer_auth.session_ttl_seconds,
        )
        .await;
    if output.is_ok() {
        if let Err(error) = clear_rate_limit(
            &state.security_repo,
            &state.security_config,
            "oauth_callback",
            "github",
            &rate_subject,
        )
        .await
        {
            tracing::error!(%error, "failed to clear successful GitHub callback rate limit");
        }
    }
    audit_auth_outcome(
        &state,
        Some(consumed.tenant_id),
        output.as_ref().ok().map(|session| session.user.id.clone()),
        "auth.github_login",
        output.is_ok(),
        Some("customer_session"),
        output
            .as_ref()
            .ok()
            .map(|session| session.session_id.as_str()),
    )
    .await;
    secret_result(output)
}

fn select_github_email(emails: Vec<GithubEmail>) -> Option<String> {
    emails
        .into_iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email)
}

async fn github_get<T: serde::de::DeserializeOwned>(
    state: &ControlState,
    token: &str,
    path: &str,
) -> Result<T, VendError> {
    state
        .customer_auth
        .http
        .get(format!("https://api.github.com{path}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "Vend")
        .send()
        .await
        .map_err(|error| VendError::Transport(error.to_string()))?
        .error_for_status()
        .map_err(|error| VendError::Transport(error.to_string()))?
        .json()
        .await
        .map_err(|error| VendError::Transport(error.to_string()))
}

#[derive(Debug, Deserialize)]
struct GoogleClaims {
    sub: String,
    email: String,
    email_verified: bool,
    nonce: String,
}

#[derive(Deserialize)]
struct GoogleJwks {
    keys: Vec<GoogleJwk>,
}

#[derive(Deserialize)]
struct GoogleJwk {
    kid: String,
    kty: String,
    alg: String,
    n: String,
    e: String,
}

async fn google_callback(
    State(state): State<ControlState>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    let Some(config) = state.customer_auth.google.as_ref() else {
        return provider_unavailable("google");
    };
    if query.error.is_some() {
        return control_error(VendError::Unauthorized);
    }
    let (code, oauth_state) = match (query.code, query.state) {
        (Some(code), Some(oauth_state)) => (code, oauth_state),
        _ => return control_error(VendError::Unauthorized),
    };
    let rate_subject = opaque_bearer_shard(&oauth_state, "vost");
    if let Err(response) = enforce_rate_limit(
        &state.security_repo,
        &state.security_config,
        "oauth_callback",
        "google",
        &rate_subject,
        state.security_config.oauth_callback,
    )
    .await
    {
        return response;
    }
    let consumed = match state.repo.consume_oauth_state(&oauth_state, "google").await {
        Ok(value) => value,
        Err(error) => return control_error(error),
    };
    let token: OAuthTokenResponse = match state
        .customer_auth
        .http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            (
                "redirect_uri",
                state.customer_auth.callback_url("google").as_str(),
            ),
            ("code_verifier", consumed.code_verifier.as_str()),
        ])
        .send()
        .await
        .and_then(|response| response.error_for_status())
    {
        Ok(response) => match response.json().await {
            Ok(value) => value,
            Err(error) => return control_error(VendError::Transport(error.to_string())),
        },
        Err(error) => return control_error(VendError::Transport(error.to_string())),
    };
    let Some(id_token) = token.id_token else {
        return control_error(VendError::Unauthorized);
    };
    let claims = match validate_google_id_token(&state, config, &id_token).await {
        Ok(claims) => claims,
        Err(error) => return control_error(error),
    };
    if !google_claims_authorized(&claims, &consumed.nonce) {
        return control_error(VendError::Unauthorized);
    }
    let output = state
        .repo
        .login_customer_external(
            &consumed.tenant_id,
            "google",
            &claims.sub,
            &claims.email,
            state.customer_auth.session_ttl_seconds,
        )
        .await;
    if output.is_ok() {
        if let Err(error) = clear_rate_limit(
            &state.security_repo,
            &state.security_config,
            "oauth_callback",
            "google",
            &rate_subject,
        )
        .await
        {
            tracing::error!(%error, "failed to clear successful Google callback rate limit");
        }
    }
    audit_auth_outcome(
        &state,
        Some(consumed.tenant_id),
        output.as_ref().ok().map(|session| session.user.id.clone()),
        "auth.google_login",
        output.is_ok(),
        Some("customer_session"),
        output
            .as_ref()
            .ok()
            .map(|session| session.session_id.as_str()),
    )
    .await;
    secret_result(output)
}

fn google_claims_authorized(claims: &GoogleClaims, expected_nonce: &str) -> bool {
    let expected: [u8; 32] = Sha256::digest(expected_nonce.as_bytes()).into();
    let actual: [u8; 32] = Sha256::digest(claims.nonce.as_bytes()).into();
    expected.ct_eq(&actual).unwrap_u8() == 1 && claims.email_verified
}

async fn validate_google_id_token(
    state: &ControlState,
    config: &OAuthProviderConfig,
    token: &str,
) -> Result<GoogleClaims, VendError> {
    let jwks: GoogleJwks = state
        .customer_auth
        .http
        .get("https://www.googleapis.com/oauth2/v3/certs")
        .send()
        .await
        .map_err(|error| VendError::Transport(error.to_string()))?
        .error_for_status()
        .map_err(|error| VendError::Transport(error.to_string()))?
        .json()
        .await
        .map_err(|error| VendError::Transport(error.to_string()))?;
    decode_google_id_token(config, token, jwks)
}

fn decode_google_id_token(
    config: &OAuthProviderConfig,
    token: &str,
    jwks: GoogleJwks,
) -> Result<GoogleClaims, VendError> {
    let header = decode_header(token).map_err(|_| VendError::Unauthorized)?;
    if header.alg != Algorithm::RS256 {
        return Err(VendError::Unauthorized);
    }
    let kid = header.kid.ok_or(VendError::Unauthorized)?;
    let key = jwks
        .keys
        .into_iter()
        .find(|key| key.kid == kid && key.kty == "RSA" && key.alg == "RS256")
        .ok_or(VendError::Unauthorized)?;
    let decoding_key =
        DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| VendError::Unauthorized)?;
    let validation = google_validation(&config.client_id);
    decode::<GoogleClaims>(token, &decoding_key, &validation)
        .map(|data| data.claims)
        .map_err(|_| VendError::Unauthorized)
}

fn google_validation(client_id: &str) -> Validation {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation
}

async fn openapi() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        include_str!("../openapi-control.json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(secret: &str) -> String {
        Sha256::digest(secret.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn admin_auth_is_exact_and_fail_closed() {
        let auth = AdminAuth::from_sha256_hex(&digest("correct horse battery staple")).unwrap();
        let mut headers = HeaderMap::new();
        assert!(!auth.authorizes(&headers));
        headers.insert(AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!auth.authorizes(&headers));
        headers.insert(
            AUTHORIZATION,
            "Bearer correct horse battery staple".parse().unwrap(),
        );
        assert!(auth.authorizes(&headers));
    }

    #[test]
    fn admin_digest_rejects_missing_malformed_and_zero_values() {
        assert!(AdminAuth::from_sha256_hex("").is_err());
        assert!(AdminAuth::from_sha256_hex(&"g".repeat(64)).is_err());
        assert!(AdminAuth::from_sha256_hex(&"0".repeat(64)).is_err());
    }

    #[test]
    fn admin_rotation_accepts_current_and_next_but_rejects_duplicate_digests() {
        let auth = AdminAuth::from_sha256_hexes(
            &digest("current-admin-secret"),
            Some(&digest("next-admin-secret")),
        )
        .unwrap();
        for token in ["current-admin-secret", "next-admin-secret"] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
            assert!(auth.authorizes(&headers));
        }
        let same = digest("same-secret");
        assert!(AdminAuth::from_sha256_hexes(&same, Some(&same)).is_err());
    }

    #[test]
    fn rate_limiting_groups_guesses_by_opaque_bearer_prefix() {
        let first = "vset_0123456789abcdef_0123456789abcdefaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "vset_0123456789abcdef_0123456789abcdefbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert_eq!(
            opaque_bearer_shard(first, "vset"),
            opaque_bearer_shard(second, "vset")
        );
        assert_eq!(opaque_bearer_shard("bad", "vset"), "malformed");
    }

    #[test]
    fn public_base_url_requires_tls_outside_localhost() {
        assert!(CustomerAuthConfig::new("http://example.com", None, None, 3600, 3600).is_err());
        assert!(CustomerAuthConfig::new("http://127.0.0.1:8090", None, None, 3600, 3600).is_ok());
        assert!(CustomerAuthConfig::new("https://vend.example", None, None, 3600, 3600).is_ok());
    }

    #[test]
    fn oauth_authorize_urls_bind_callback_state_pkce_nonce_and_scopes() {
        let config = OAuthProviderConfig {
            client_id: "client-123".into(),
            client_secret: "secret".into(),
        };
        let flow = OAuthStart {
            state: "state-123".into(),
            code_challenge: "challenge-123".into(),
            nonce: "nonce-123".into(),
        };
        let github = oauth_authorize_url(
            "github",
            &config,
            "https://vend.example/v1/auth/github/callback",
            &flow,
        )
        .unwrap();
        let github_query: std::collections::HashMap<_, _> = github
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            github_query.get("state").map(String::as_str),
            Some("state-123")
        );
        assert_eq!(
            github_query.get("redirect_uri").map(String::as_str),
            Some("https://vend.example/v1/auth/github/callback")
        );
        assert_eq!(
            github_query.get("code_challenge").map(String::as_str),
            Some("challenge-123")
        );
        assert_eq!(
            github_query
                .get("code_challenge_method")
                .map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            github_query.get("scope").map(String::as_str),
            Some("read:user user:email")
        );
        assert!(!github_query.contains_key("nonce"));

        let google = oauth_authorize_url(
            "google",
            &config,
            "https://vend.example/v1/auth/google/callback",
            &flow,
        )
        .unwrap();
        let google_query: std::collections::HashMap<_, _> = google
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            google_query.get("scope").map(String::as_str),
            Some("openid email")
        );
        assert_eq!(
            google_query.get("nonce").map(String::as_str),
            Some("nonce-123")
        );
    }

    #[test]
    fn github_requires_a_verified_primary_email() {
        let emails = vec![
            GithubEmail {
                email: "primary-unverified@example.com".into(),
                primary: true,
                verified: false,
            },
            GithubEmail {
                email: "verified-secondary@example.com".into(),
                primary: false,
                verified: true,
            },
        ];
        assert!(select_github_email(emails).is_none());
        assert_eq!(
            select_github_email(vec![GithubEmail {
                email: "owner@example.com".into(),
                primary: true,
                verified: true,
            }]),
            Some("owner@example.com".into())
        );
    }

    #[test]
    fn google_claims_require_verified_email_and_exact_nonce() {
        let mut claims = GoogleClaims {
            sub: "123".into(),
            email: "owner@example.com".into(),
            email_verified: true,
            nonce: "expected".into(),
        };
        assert!(google_claims_authorized(&claims, "expected"));
        assert!(!google_claims_authorized(&claims, "other"));
        claims.email_verified = false;
        assert!(!google_claims_authorized(&claims, "expected"));
    }

    #[test]
    fn google_jwt_validation_is_fail_closed_and_requires_core_claims() {
        let validation = google_validation("client-123");
        assert!(validation.validate_exp);
        for claim in ["exp", "iss", "aud", "sub"] {
            assert!(validation.required_spec_claims.contains(claim));
        }
        assert!(validation.aud.as_ref().unwrap().contains("client-123"));
        assert!(validation
            .iss
            .as_ref()
            .unwrap()
            .contains("https://accounts.google.com"));

        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &json!({"sub":"123","email":"owner@example.com","email_verified":true,
                    "nonce":"n","iss":"https://accounts.google.com","aud":"client-123",
                    "exp":4102444800_u64}),
            &jsonwebtoken::EncodingKey::from_secret(b"not-rsa"),
        )
        .unwrap();
        assert_eq!(
            decode_google_id_token(
                &OAuthProviderConfig {
                    client_id: "client-123".into(),
                    client_secret: "x".into()
                },
                &token,
                GoogleJwks { keys: vec![] },
            )
            .unwrap_err(),
            VendError::Unauthorized
        );
    }

    #[test]
    fn secret_responses_are_never_cacheable() {
        let response = secret_result(Ok(json!({"session_token":"secret"})));
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );
    }
}
