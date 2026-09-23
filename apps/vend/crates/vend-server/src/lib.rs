//! # vend-server — the public data plane (axum metered proxy).
//!
//! `POST /v1/chat/completions` runs the full flow: verify the provisioning key,
//! resolve the tenant/app/end-user, enforce the model allowlist and the daily
//! spend cap, open a Mint hold, forward to Relay, then settle (capture the exact
//! actual on 2xx-with-usage, release otherwise). `GET /healthz|/readyz` are the
//! liveness/readiness probes.
//!
//! The state is built from injectable trait objects, so production wires the REAL
//! `vend-mint-client` + `vend-relay-client` + `vend-store`, while tests wire the
//! `vend-testkit` fakes + in-memory store.

pub mod control;
pub mod observability;
pub mod pricing;
pub mod readiness;
pub mod runtime_config;
pub mod security;
pub mod security_ops;
pub mod shutdown;
mod streaming;

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use vend_core::error::VendError;
use vend_core::ids::{ChargeId, SubKeyId, TenantId};
use vend_core::money::{price, FundingMode, Micros};
use vend_core::ports::{
    IdempotencyClaim, IdempotencyRepo, MeteredUsage, ReadinessRepo, UsageOutcome, UsageRepo,
};
use vend_core::transport::{HoldRequest, MintTransport, RelayRequest, RelayTransport};
use vend_core::PayoutRepo;
use vend_keys::{check_spend_cap, verify, KeyRepo};
use vend_settlement::{execute_payout, prepare_payout, settle, HoldGuard, PayoutConfig, Terminal};
use vend_store::{AuditActorKind, AuditOutcome, NewAuditEvent, SecurityOpsRepo};
use vend_tenancy::{funding_account_id, resolve, TenantRepo};

use crate::security_ops::{clear_rate_limit, enforce_rate_limit, SecurityOpsConfig};

#[derive(Clone)]
pub struct DataPlaneSecurity {
    pub repo: Arc<dyn SecurityOpsRepo>,
    pub config: SecurityOpsConfig,
}

/// Shared server state. All external dependencies are trait objects behind `Arc`.
#[derive(Clone)]
pub struct AppState {
    /// Process-local, low-cardinality HTTP and settlement metrics.
    pub metrics: Arc<observability::Metrics>,
    /// Mint transport (holds/capture/release/reserve).
    pub mint: Arc<dyn MintTransport>,
    /// Relay transport (inference).
    pub relay: Arc<dyn RelayTransport>,
    /// Tenancy repo.
    pub tenants: Arc<dyn TenantRepo>,
    /// Key repo.
    pub keys: Arc<dyn KeyRepo>,
    /// Usage repo.
    pub usage: Arc<dyn UsageRepo>,
    /// Durable split-payout external-operation projection.
    pub payouts: Arc<dyn PayoutRepo>,
    /// Postgres dependency probe.
    pub readiness: Arc<dyn ReadinessRepo>,
    /// Request replay persistence, scoped to the authenticated key.
    pub idempotency: Arc<dyn IdempotencyRepo>,
    /// Validated model-specific rate cards.
    pub pricing: Arc<pricing::PricingCatalog>,
    /// Relay base path for the OpenAI-compatible surface (e.g. `/openai/v1`).
    pub relay_base_path: String,
    /// Upstream Relay bearer credential.
    pub relay_key: String,
    /// Mint account credited when a successful hold is captured.
    pub settlement_account: String,
    /// Validated Mint destinations for provider/platform split payout.
    pub payout_config: PayoutConfig,
    /// Required in production. Tests may omit it when authentication throttling
    /// is not the behavior under test.
    pub security_ops: Option<DataPlaneSecurity>,
}

/// Build the router for a given [`AppState`].
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/admin", get(admin_page))
        .route("/console", get(console_page))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// `GET /admin` — a self-contained operator console (static HTML + vanilla JS,
/// no build step). The page is served unauthenticated, but every data request it
/// makes carries the operator's `Authorization: Bearer <admin-token>` against the
/// same authenticated `/v1/control` endpoints — there is no server-side bypass. It
/// only surfaces what those real endpoints return: tenants, apps, provisioning
/// keys, per-app daily usage, and tenant users.
async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../assets/admin.html"))
}

/// `GET /console` — the customer console (static HTML + vanilla JS) over the
/// authenticated `/v1/customer` and `/v1/auth` endpoints: sign in (password or
/// GitHub/Google OAuth), manage apps and provisioning keys, view usage, fund
/// the app reserve through Mint checkout, manage the team, and change password.
async fn console_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../assets/console.html"))
}

/// `GET /v1/models` — the OpenAI-compatible model list for the calling key:
/// every priced model the key's app allowlist permits, with the effective
/// per-million-token rates the app bills at (provider rate plus the app's
/// markup) and the per-request minimum fee.
async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = preflight_provisioning_key(&state, &headers).await {
        return response;
    }
    let app = match async {
        let secret = bearer(&headers)?;
        let key = verify(state.keys.as_ref(), secret).await?;
        let ctx = resolve(state.tenants.as_ref(), &key, end_user_ref(&headers).as_deref()).await?;
        Ok::<_, VendError>(ctx.app)
    }
    .await
    {
        Ok(app) => app,
        Err(error) => return error_response(error),
    };
    let policy = &app.markup_policy;
    let marked_up = |micros: i64| -> i64 {
        let scaled = i128::from(micros) * (10_000 + i128::from(policy.markup_bps)) / 10_000;
        i64::try_from(scaled).unwrap_or(i64::MAX)
    };
    let data: Vec<serde_json::Value> = state
        .pricing
        .models()
        .into_iter()
        .filter(|(model, _)| app.allows_model(model))
        .map(|(model, rate)| {
            serde_json::json!({
                "id": model,
                "object": "model",
                "owned_by": "vend",
                "pricing": {
                    "input_micros_per_million_tokens": marked_up(rate.input_micros_per_million_tokens),
                    "output_micros_per_million_tokens": marked_up(rate.output_micros_per_million_tokens),
                    "min_fee_micros_per_request": policy.min_fee_micros,
                    "max_input_tokens": rate.max_input_tokens,
                    "max_output_tokens": rate.max_output_tokens,
                },
            })
        })
        .collect();
    Json(serde_json::json!({ "object": "list", "data": data })).into_response()
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match state.readiness.ready().await {
        Ok(()) => (StatusCode::OK, "ready"),
        Err(error) => {
            tracing::warn!(%error, "Vend dependency readiness probe failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready")
        }
    }
}

/// Map a [`VendError`] into OpenAI's stable JSON error shape.
fn error_response(err: VendError) -> Response {
    let status =
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    openai_error(status, err.client_message(), err.code()).into_response()
}

fn openai_error(
    status: StatusCode,
    message: impl Into<String>,
    code: impl Into<String>,
) -> Response {
    (status, Json(openai_error_body(message, code))).into_response()
}

pub(crate) fn openai_error_body(
    message: impl Into<String>,
    code: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": message.into(),
            "type": "vend_error",
            "param": serde_json::Value::Null,
            "code": code.into(),
        }
    })
}

pub(crate) fn bearer(headers: &HeaderMap) -> Result<&str, VendError> {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(VendError::Unauthorized)
}

pub(crate) fn end_user_ref(headers: &HeaderMap) -> Option<String> {
    headers
        .get("vend-end-user")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// The `POST /v1/chat/completions` edge handler.
async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(response) = preflight_provisioning_key(&state, &headers).await {
        return response;
    }
    if body.get("stream").and_then(|value| value.as_bool()) == Some(true) {
        return match streaming::run(&state, &headers, body).await {
            Ok(response) => response,
            Err(error) => error_response(error),
        };
    }
    match run_flow(&state, &headers, body).await {
        Ok(out) => out.into_response(),
        Err(err) => error_response(err),
    }
}

async fn preflight_provisioning_key(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let Some(security) = &state.security_ops else {
        return Ok(());
    };
    let Ok(secret) = bearer(headers) else {
        return Ok(());
    };
    let Some(prefix) = vend_keys::lookup_prefix(secret) else {
        return Ok(());
    };
    let record = state
        .keys
        .find_by_prefix(&prefix)
        .await
        .map_err(error_response)?;
    let Some(record) = record else {
        // Unknown random prefixes do not trigger Argon2 and reveal no tenant.
        // Known-prefix guessing is the expensive inference path protected below.
        return Ok(());
    };
    enforce_rate_limit(
        &security.repo,
        &security.config,
        "provisioning_key",
        record.tenant_id.as_str(),
        &prefix,
        security.config.provisioning_key,
    )
    .await?;
    match verify(state.keys.as_ref(), secret).await {
        Ok(_) => {
            if let Err(error) = clear_rate_limit(
                &security.repo,
                &security.config,
                "provisioning_key",
                record.tenant_id.as_str(),
                &prefix,
            )
            .await
            {
                tracing::error!(%error, "failed to clear provisioning-key rate limit");
            }
            Ok(())
        }
        Err(error) => Err(error_response(error)),
    }
}

pub(crate) async fn audit_money_event(
    security: Option<&DataPlaneSecurity>,
    tenant_id: &TenantId,
    sub_key_id: &SubKeyId,
    charge_id: &ChargeId,
    action: &str,
    outcome: AuditOutcome,
    metadata: serde_json::Value,
) {
    let Some(security) = security else { return };
    let event = NewAuditEvent {
        tenant_id: Some(tenant_id.clone()),
        actor_kind: AuditActorKind::ProvisioningKey,
        actor_id: Some(sub_key_id.as_str().to_string()),
        action: action.to_string(),
        target_kind: Some("charge".into()),
        target_id: Some(charge_id.as_str().to_string()),
        outcome,
        request_id: None,
        source_hash: None,
        metadata,
    };
    if let Err(error) = security.repo.append_audit_event(event).await {
        // Mint and Postgres cannot share a transaction. Usage/payout rows remain
        // authoritative; audit is an immediate, secret-free observation.
        tracing::error!(%error, charge_id = charge_id.as_str(), action, "money audit append failed");
    }
}

pub(crate) async fn audit_settlement_event(
    security: Option<&DataPlaneSecurity>,
    tenant_id: &TenantId,
    sub_key_id: &SubKeyId,
    charge_id: &ChargeId,
    result: &Result<vend_settlement::SettleOutcome, VendError>,
    reason: &str,
) {
    let (action, outcome) = match result {
        Ok(vend_settlement::SettleOutcome::Captured(_)) => {
            ("money.settlement_captured", AuditOutcome::Succeeded)
        }
        Ok(vend_settlement::SettleOutcome::Released) => {
            ("money.settlement_released", AuditOutcome::Succeeded)
        }
        Err(_) => ("money.settlement_failed", AuditOutcome::Failed),
    };
    audit_money_event(
        security,
        tenant_id,
        sub_key_id,
        charge_id,
        action,
        outcome,
        serde_json::json!({"reason": reason}),
    )
    .await;
}

/// The settlement result of one metered call, carrying the settlement headers the
/// data plane surfaces alongside the OpenAI-compatible body.
#[derive(Clone, Serialize, Deserialize)]
struct FlowOutcome {
    /// Original upstream status (or Vend's gateway status).
    status: u16,
    /// The OpenAI-compatible upstream body.
    body: serde_json::Value,
    /// `chg_<ULID>` id for this call (`Vend-Charge-Id`).
    charge_id: String,
    /// Exact provider cost settled, in µ$ (`Vend-Base-Cost-Micros`). `0` on release.
    base_cost_micros: i64,
    /// Exact end-user price captured, in µ$ (`Vend-Price-Micros`). `0` on release.
    price_micros: i64,
    /// The caller-selected Relay model id (`Vend-Model`). Absent only for a
    /// recovered capture whose upstream response metadata was lost.
    model: Option<String>,
}

impl IntoResponse for FlowOutcome {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut resp = (status, Json(self.body)).into_response();
        let h = resp.headers_mut();
        if let Ok(v) = self.charge_id.parse() {
            h.insert("Vend-Charge-Id", v);
        }
        h.insert("Vend-Base-Cost-Micros", self.base_cost_micros.into());
        h.insert("Vend-Price-Micros", self.price_micros.into());
        if let Some(model) = self.model {
            if let Ok(v) = model.parse() {
                h.insert("Vend-Model", v);
            }
        }
        resp
    }
}

#[derive(Serialize, Deserialize)]
struct ReplayRecord {
    request_hash: String,
    outcome: FlowOutcome,
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<&str, VendError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(VendError::IdempotencyRequired)?;
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(VendError::IdempotencyRequired);
    }
    Ok(key)
}

pub(crate) fn request_hash(body: &serde_json::Value) -> Result<String, VendError> {
    let bytes = serde_json::to_vec(body).map_err(|error| VendError::Storage(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// The full metered-proxy flow. Returns the upstream body + settlement headers.
async fn run_flow(
    state: &AppState,
    headers: &HeaderMap,
    body: serde_json::Value,
) -> Result<FlowOutcome, VendError> {
    // 1. Authenticate and resolve the provisioning key.
    let secret = bearer(headers)?;
    let key = verify(state.keys.as_ref(), secret).await?;
    let ext_ref = end_user_ref(headers);
    let ctx = resolve(state.tenants.as_ref(), &key, ext_ref.as_deref()).await?;

    // Sx0 settles one complete customer charge into one clearing account. BYOK
    // and end-user-reserve need split settlement semantics that are not present.
    if ctx.app.markup_policy.funding_mode != FundingMode::TenantReserve {
        return Err(VendError::FundingModeUnsupported);
    }

    // 2. Validate model/rate and establish a key-scoped replay identity.
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(VendError::InvalidAmount)?
        .to_string();
    if !ctx.app.allows_model(&model) {
        return Err(VendError::ModelNotAllowed);
    }
    let client_key = idempotency_key(headers)?;
    let body_hash = request_hash(&body)?;
    let key_hash = format!("{:x}", Sha256::digest(client_key.as_bytes()));
    let replay_key = format!("{}:{key_hash}", ctx.sub_key.id.as_str());
    match state.idempotency.claim(&replay_key, &body_hash).await? {
        IdempotencyClaim::Acquired => {}
        IdempotencyClaim::Replay(raw) => {
            let replay: ReplayRecord = serde_json::from_str(&raw)
                .map_err(|error| VendError::Storage(format!("invalid replay row: {error}")))?;
            return Ok(replay.outcome);
        }
        IdempotencyClaim::InProgress => return Err(VendError::IdempotencyInProgress),
        IdempotencyClaim::Conflict => return Err(VendError::IdempotencyConflict),
    }

    // 3. Price a conservative input+output ceiling and run the fast cap check.
    //    rejects clearly-over-cap requests before opening any hold (so no Mint
    //    hold is taken and Relay is never called). It is advisory under
    //    concurrency; the authoritative TOCTOU-safe gate is the atomic
    //    `record_pending_within_cap` in step 6.
    let utc_day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let estimate = match state.pricing.estimate_provider_cost(&model, &body) {
        Ok(estimate) => estimate,
        Err(error) => {
            state.idempotency.abort(&replay_key, &body_hash).await?;
            return Err(error);
        }
    };
    let est_charge = match price(estimate, &ctx.app.markup_policy) {
        Ok(charge) => charge,
        Err(error) => {
            state.idempotency.abort(&replay_key, &body_hash).await?;
            return Err(error);
        }
    };
    if let Err(error) = check_spend_cap(
        state.usage.as_ref(),
        &ctx.sub_key,
        ctx.end_user.as_ref().map(|e| &e.id),
        est_charge.charge_micros,
        &utc_day,
    )
    .await
    {
        state.idempotency.abort(&replay_key, &body_hash).await?;
        return Err(error);
    }

    // 4. Commit the usage intent and authoritative cap reservation before
    // touching Mint. The empty hold id is a recoverable outbox state: if this
    // process dies after Mint accepts the idempotent hold, a reconciler can
    // repeat `:hold`, recover the same hold id, and release it safely.
    let charge_id = ChargeId::generate();
    let idem_key = format!("vend:{replay_key}");
    let usage_row = MeteredUsage {
        charge_id: charge_id.clone(),
        sub_key_id: ctx.sub_key.id.clone(),
        tenant_id: ctx.tenant.id.clone(),
        app_id: ctx.app.id.clone(),
        end_user_id: ctx.end_user.as_ref().map(|e| e.id.clone()),
        idem_key: idem_key.clone(),
        hold_id: String::new(),
        provider_cost_micros: estimate.get(),
        charge_micros: est_charge.charge_micros,
        platform_fee_micros: est_charge.platform_fee_micros,
        tenant_net_micros: est_charge.tenant_net_micros,
        outcome: UsageOutcome::Pending,
        settlement_proof: None,
        utc_day,
    };
    if let Err(error) = state
        .usage
        .record_pending_within_cap(
            &usage_row,
            ctx.sub_key.daily_cap_micros,
            ctx.sub_key.per_end_user_daily_cap_micros,
        )
        .await
    {
        state.idempotency.abort(&replay_key, &body_hash).await?;
        return Err(error);
    }

    // 5. Open and attach the Mint hold. Operation-specific suffixes keep Mint's
    // global idempotency namespace collision-free. If Mint rejects the hold,
    // release the local cap reservation and let the caller retry.
    let account = funding_account_id(&ctx).to_string();
    let hold = match state
        .mint
        .create_hold(HoldRequest {
            from: account,
            amount_micros: est_charge.charge_micros,
            idem_key: format!("{idem_key}:hold"),
        })
        .await
    {
        Ok(hold) => hold,
        Err(error) => {
            let _ = state.usage.mark_released(&charge_id).await;
            let _ = state.idempotency.abort(&replay_key, &body_hash).await;
            audit_money_event(
                state.security_ops.as_ref(),
                &ctx.tenant.id,
                &ctx.sub_key.id,
                &charge_id,
                "money.hold_failed",
                AuditOutcome::Failed,
                serde_json::json!({"reason": error.code()}),
            )
            .await;
            return Err(error);
        }
    };
    audit_money_event(
        state.security_ops.as_ref(),
        &ctx.tenant.id,
        &ctx.sub_key.id,
        &charge_id,
        "money.hold_opened",
        AuditOutcome::Succeeded,
        serde_json::json!({
            "hold_id": hold.hold_id,
            "amount_micros": est_charge.charge_micros
        }),
    )
    .await;
    let mut guard = HoldGuard::new(hold.hold_id.clone(), idem_key.clone());
    if let Err(error) = state.usage.attach_hold(&charge_id, &hold.hold_id).await {
        // Mint has the hold, so money safety takes precedence over the failed
        // local projection update. The operation-specific release is retry-safe.
        let release = settle(
            state.mint.as_ref(),
            state.usage.as_ref(),
            &charge_id,
            &mut guard,
            &state.settlement_account,
            Terminal::Release,
        )
        .await;
        record_settlement(&state.metrics, &release);
        audit_settlement_event(
            state.security_ops.as_ref(),
            &ctx.tenant.id,
            &ctx.sub_key.id,
            &charge_id,
            &release,
            "hold_projection_attach_failed",
        )
        .await;
        let _ = state.idempotency.abort(&replay_key, &body_hash).await;
        return Err(error);
    }

    // 6. Forward to Relay with the app's no-observability-storage policy.
    let relay_result = state
        .relay
        .chat_completion(RelayRequest {
            base_path: state.relay_base_path.clone(),
            bearer: state.relay_key.clone(),
            body,
            zdr: ctx.app.zdr,
        })
        .await;

    // 7. Rate valid terminal usage. Missing/invalid/out-of-bounds usage is a
    // gateway failure and releases the hold; it is never silently clamped.
    let (terminal, status, response_body, actual_cost_micros) = match &relay_result {
        Ok(resp) if !resp.is_success() => (Terminal::Release, resp.status, resp.body.clone(), 0),
        Ok(resp) => match resp.usage.as_ref() {
            Some(usage) => match usage.provider_cost_micros {
                Some(cost) if cost >= 0 => {
                    let actual_cost = Micros::new(cost);
                    match price(actual_cost, &ctx.app.markup_policy) {
                        Ok(actual_charge) if actual_charge.charge_micros == 0 => {
                            // Mint correctly rejects zero captures. A truly
                            // free terminal is successful but uncharged.
                            (Terminal::Release, resp.status, resp.body.clone(), 0)
                        }
                        Ok(actual_charge)
                            if actual_charge.charge_micros > est_charge.charge_micros => (
                            Terminal::Release,
                            502,
                            openai_error_body(
                                "reported usage exceeded the reserved billing bound",
                                "billing_bound_exceeded",
                            ),
                            0,
                        ),
                        Ok(actual_charge) => (
                            Terminal::from_relay(resp, actual_charge),
                            resp.status,
                            resp.body.clone(),
                            actual_cost.get(),
                        ),
                        Err(error) => (
                            Terminal::Release,
                            502,
                            openai_error_body(error.client_message(), error.code()),
                            0,
                        ),
                    }
                }
                _ => (
                    Terminal::Release,
                    502,
                    openai_error_body(
                        "Relay did not report an exact microdollar tensorzero_cost; request was not charged",
                        "invalid_provider_cost",
                    ),
                    0,
                ),
            },
            None => (
                Terminal::Release,
                502,
                openai_error_body(
                    "Relay returned success without terminal usage; request was not charged",
                    "missing_usage",
                ),
                0,
            ),
        },
        Err(error) => (
            Terminal::Release,
            502,
            openai_error_body(error.client_message(), error.code()),
            0,
        ),
    };
    if let Terminal::Capture { actual } = terminal {
        if let Err(error) = prepare_payout(
            state.payouts.as_ref(),
            &charge_id,
            &idem_key,
            &state.payout_config,
            &ctx.tenant.mint_account_id,
            actual,
        )
        .await
        {
            // Never capture into clearing without a durable, replayable payout
            // plan. Release is idempotent and restores the customer's reserve.
            let release = settle(
                state.mint.as_ref(),
                state.usage.as_ref(),
                &charge_id,
                &mut guard,
                &state.settlement_account,
                Terminal::Release,
            )
            .await;
            record_settlement(&state.metrics, &release);
            audit_settlement_event(
                state.security_ops.as_ref(),
                &ctx.tenant.id,
                &ctx.sub_key.id,
                &charge_id,
                &release,
                "payout_prepare_failed",
            )
            .await;
            audit_money_event(
                state.security_ops.as_ref(),
                &ctx.tenant.id,
                &ctx.sub_key.id,
                &charge_id,
                "money.payout_failed",
                AuditOutcome::Failed,
                serde_json::json!({"phase": "prepare"}),
            )
            .await;
            return Err(error);
        }
    }
    let settle_outcome = settle(
        state.mint.as_ref(),
        state.usage.as_ref(),
        &charge_id,
        &mut guard,
        &state.settlement_account,
        terminal,
    )
    .await;
    record_settlement(&state.metrics, &settle_outcome);
    audit_settlement_event(
        state.security_ops.as_ref(),
        &ctx.tenant.id,
        &ctx.sub_key.id,
        &charge_id,
        &settle_outcome,
        if relay_result.is_ok() {
            "relay_terminal"
        } else {
            "relay_transport_failed"
        },
    )
    .await;
    let settle_outcome = settle_outcome?;

    if matches!(settle_outcome, vend_settlement::SettleOutcome::Captured(_)) {
        let payout = execute_payout(state.mint.as_ref(), state.payouts.as_ref(), &charge_id).await;
        audit_money_event(
            state.security_ops.as_ref(),
            &ctx.tenant.id,
            &ctx.sub_key.id,
            &charge_id,
            if payout.is_ok() {
                "money.payout_completed"
            } else {
                "money.payout_pending"
            },
            if payout.is_ok() {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
            serde_json::json!({"phase": "execute"}),
        )
        .await;
        payout?;
    }

    // The captured price (0 on release) and the matching base cost.
    let (price_micros, base_cost_micros) = match settle_outcome {
        vend_settlement::SettleOutcome::Captured(price) => (price, actual_cost_micros),
        vend_settlement::SettleOutcome::Released => (0, 0),
    };

    // 8. Preserve the upstream status/body and persist a replayable terminal
    // response with identical settlement metadata.
    let outcome = FlowOutcome {
        status,
        body: response_body,
        charge_id: charge_id.as_str().to_string(),
        base_cost_micros,
        price_micros,
        model: Some(model),
    };
    let replay = serde_json::to_string(&ReplayRecord {
        request_hash: body_hash.clone(),
        outcome: outcome.clone(),
    })
    .map_err(|error| VendError::Storage(error.to_string()))?;
    state
        .idempotency
        .complete(&replay_key, &body_hash, &replay)
        .await?;
    Ok(outcome)
}

fn record_settlement(
    metrics: &observability::Metrics,
    result: &Result<vend_settlement::SettleOutcome, VendError>,
) {
    match result {
        Ok(vend_settlement::SettleOutcome::Captured(_)) => metrics.settlement_captured(),
        Ok(vend_settlement::SettleOutcome::Released) => metrics.settlement_released(),
        Err(_) => metrics.settlement_failed(),
    }
}

#[cfg(test)]
mod reconciliation_replay_tests {
    use super::*;

    #[test]
    fn recovered_capture_is_accepted_by_the_live_replay_schema() {
        let charge_id = ChargeId::generate();
        let raw = vend_store::captured_recovery_replay("request-hash", &charge_id, 700, 1_000);
        let replay: ReplayRecord = serde_json::from_str(&raw).unwrap();
        assert_eq!(replay.request_hash, "request-hash");
        assert_eq!(replay.outcome.status, 409);
        assert_eq!(replay.outcome.charge_id, charge_id.as_str());
        assert_eq!(replay.outcome.base_cost_micros, 700);
        assert_eq!(replay.outcome.price_micros, 1_000);
        assert_eq!(replay.outcome.model, None);
        assert_eq!(
            replay.outcome.body["error"]["code"],
            "settled_response_unavailable"
        );
    }
}
