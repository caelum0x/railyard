//! The `POST /v1/proxy` orchestration — the 7-step hot path.
//!
//! 1. Parse the request: agent id + run id (headers) and the verbatim body.
//! 2. Resolve the agent identity + budget policy via the [`BudgetRepo`].
//! 3. Price the MAX possible cost (`est_max`) from the pricing table.
//! 4. Reserve gate ([`enforce`]): open a Mint hold (direct or allowance-backed)
//!    or KILL the request before any upstream byte is sent.
//! 5. Forward the request verbatim to the real provider.
//! 6. Price the EXACT actual cost from reported usage.
//! 7. Settle ([`settle`]): capture on 2xx-with-usage / release otherwise, and
//!    persist the Mint receipt (the system-of-record for agent spend).

use axum::extract::State;
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use abacus_core::decision::EnforcementDecision;
use abacus_core::envelope::{ErrorEnvelope, SuccessEnvelope};
use abacus_core::error::AbacusError;
use abacus_core::hold_store::{Clock, HoldReceiptContext, HoldRecord, SystemClock};
use abacus_core::ids::{AgentId, RunId};
use abacus_core::money::Micros;
use abacus_core::transport::Release;
use abacus_core::transport::UpstreamRequest;

use crate::enforce::enforce;
use crate::settle::{settle, HoldGuard, Terminal};
use crate::AppState;

/// Header carrying the spending agent id (`agent_…`).
pub const AGENT_HEADER: &str = "x-abacus-agent";
/// Header carrying the run/session id (`run_…`); a fresh one is minted if absent.
pub const RUN_HEADER: &str = "x-abacus-run";
/// Stable business request id shared with Relay/Mint/Passport/Ledgerline.
pub const REQUEST_HEADER: &str = "x-request-id";

/// The `POST /v1/proxy` edge handler.
pub async fn proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    match run_flow(&state, &headers, body).await {
        Ok(value) => (StatusCode::OK, Json(SuccessEnvelope::new(value))).into_response(),
        Err(err) => error_response(&err),
    }
}

/// Map an [`AbacusError`] into an HTTP error response using the stable envelope.
fn error_response(err: &AbacusError) -> Response {
    let status =
        StatusCode::from_u16(err.status_for()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(ErrorEnvelope::from_error(err))).into_response()
}

fn header_str<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers.get(key).and_then(|v| v.to_str().ok())
}

/// The full enforced-proxy flow. Returns the upstream body on success.
pub async fn run_flow(
    state: &AppState,
    headers: &HeaderMap,
    body: serde_json::Value,
) -> Result<serde_json::Value, AbacusError> {
    // 1. Parse identity headers + the upstream bearer (agent's own provider key).
    let agent = AgentId::parse(header_str(headers, AGENT_HEADER).ok_or(AbacusError::BadId)?)?;
    let run = match header_str(headers, RUN_HEADER) {
        Some(s) => RunId::parse(s)?,
        None => RunId::generate(),
    };
    let bearer = header_str(headers, AUTHORIZATION.as_str())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();

    // 2. Resolve the agent identity + budget policy.
    let identity = state.budgets.resolve(&agent, &run).await?;

    // 3. Price the MAX possible cost (est_max) for the reserve gate.
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(AbacusError::ModelNotPriced)?
        .to_string();
    let estimate = state.pricing.estimate(&state.provider, &body)?;

    // 4. Reserve gate — the enforcement point. A Kill short-circuits here; no
    //    upstream request is ever made.
    //
    //    The idempotency key MUST be unique per invocation, otherwise two distinct
    //    requests in the same run with the same est_max would collide on Mint's
    //    idempotent-replay path and share ONE hold — admitting multiple upstream
    //    calls against a single budget reservation (a budget bypass). Genuine
    //    client retries are collapsed ONLY when the caller supplies an explicit
    //    `Idempotency-Key` header; absent that, a fresh nonce is minted per request.
    let request_key = header_str(headers, "idempotency-key")
        .map(str::to_string)
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let request_id = header_str(headers, REQUEST_HEADER)
        .unwrap_or(&request_key)
        .trim()
        .to_string();
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AbacusError::BadId);
    }
    let idem_key = format!(
        "{}:{}:{}",
        run,
        request_key,
        estimate.reserve_amount().get()
    );
    let (decision, reservation) =
        enforce(state.mint.as_ref(), &identity, &estimate, &idem_key).await?;
    let reservation = match decision {
        EnforcementDecision::Allow { .. } => reservation.expect("allow implies a reservation"),
        EnforcementDecision::Kill { reason, status } => {
            return Err(kill_to_error(&reason, status));
        }
    };

    // Persist the reserve before a single upstream byte is sent. If tracking
    // cannot be made durable, roll the Mint hold back and fail closed.
    let hold_record = HoldRecord {
        hold_id: reservation.hold_id.clone(),
        idem_key: idem_key.clone(),
        reserved_micros: reservation.reserved_micros,
        created_at_ms: SystemClock.now_ms(),
        deadline_ms: None,
        receipt_context: Some(HoldReceiptContext {
            agent_id: agent.clone(),
            run_id: run.clone(),
            budget_id: identity.budget.budget_id.clone(),
            payer_account: identity.treasury.as_str().to_string(),
            request_id: request_id.clone(),
        }),
    };
    if let Err(store_error) = state.holds.create(hold_record).await {
        // An idempotency conflict means another request already owns this hold.
        // Never release it from the losing request; doing so would pull the
        // reservation out from under the live owner. Other storage failures mean
        // this newly-opened hold is untracked, so roll it back before returning.
        if store_error != AbacusError::IdempotencyConflict {
            if let Err(release_error) = state
                .mint
                .release_hold(
                    &reservation.hold_id,
                    Release {
                        idem_key: idem_key.clone(),
                    },
                )
                .await
            {
                tracing::error!(%release_error, "failed to roll back untracked Mint hold");
            }
        }
        return Err(store_error);
    }

    // Arm the RAII guard the instant the hold is open.
    let mut guard = HoldGuard::new(
        reservation.hold_id.clone(),
        idem_key.clone(),
        reservation.reserved_micros,
    );

    // 5. Forward the request verbatim to the real provider.
    let upstream_result = state
        .upstream
        .send(UpstreamRequest {
            path: state.upstream_path.clone(),
            bearer,
            body,
        })
        .await;

    // 6 + 7. Derive the terminal, settle (capture actual / release), record the
    //         receipt. Release covers non-2xx, no-usage, AND transport/timeout.
    let (terminal, response_body) = match &upstream_result {
        Ok(resp) if resp.is_success() && resp.usage.is_some() => {
            let usage = resp.usage.unwrap_or_default();
            let actual = state
                .pricing
                .actual(&state.provider, &model, &usage)
                .unwrap_or(Micros::zero());
            (
                Terminal::Capture {
                    actual_micros: actual.get(),
                },
                Some(resp.body.clone()),
            )
        }
        Ok(resp) => (Terminal::Release, Some(resp.body.clone())),
        Err(_) => (Terminal::Release, None),
    };

    settle(
        state.mint.as_ref(),
        state.receipts.as_ref(),
        &identity,
        &request_id,
        &mut guard,
        terminal,
    )
    .await?;
    state.holds.remove(guard.hold_id()).await?;

    match (upstream_result, response_body) {
        (Ok(resp), _) => Ok(resp.body),
        (Err(e), _) => Err(e),
    }
}

/// Rebuild the enforcing error from a kill decision so the edge surfaces the exact
/// status + code the budget refused with.
fn kill_to_error(reason: &str, status: u16) -> AbacusError {
    match reason {
        "allowance_exceeded" => AbacusError::AllowanceExceeded,
        "insufficient_funds" => AbacusError::InsufficientFunds,
        "account_frozen" => AbacusError::AccountFrozen,
        "payout_limit_exceeded" => AbacusError::PayoutLimitExceeded,
        "allowance_not_found" => AbacusError::AllowanceNotFound,
        "account_not_found" => AbacusError::AccountNotFound,
        _ => AbacusError::Upstream {
            status,
            code: reason.to_string(),
            message: "budget refused request".into(),
        },
    }
}
