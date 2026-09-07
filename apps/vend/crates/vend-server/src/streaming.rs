//! Incremental OpenAI-compatible SSE proxying with terminal-cost settlement.
//!
//! The HTTP response is returned as soon as Relay's headers arrive. A detached
//! pump forwards every body chunk unchanged and owns the hold until it observes
//! `[DONE]` with a valid terminal `usage.tensorzero_cost`. Losing the downstream
//! receiver is treated as a client disconnect and releases the hold.

use std::io;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use vend_core::error::VendError;
use vend_core::ids::ChargeId;
use vend_core::money::{price, FundingMode, Micros};
use vend_core::ports::{IdempotencyClaim, MeteredUsage, UsageOutcome};
use vend_core::transport::{parse_usage, HoldRequest, RelayRequest, RelayStreamResponse};
use vend_keys::{check_spend_cap, verify};
use vend_settlement::{execute_payout, prepare_payout, settle, HoldGuard, PayoutConfig, Terminal};
use vend_store::AuditOutcome;
use vend_tenancy::{funding_account_id, resolve};

use crate::{bearer, end_user_ref, idempotency_key, request_hash, AppState};

/// Execute the streaming variant after the edge handler has recognized
/// `stream:true`.
pub(crate) async fn run(
    state: &AppState,
    headers: &HeaderMap,
    body: serde_json::Value,
) -> Result<Response, VendError> {
    let secret = bearer(headers)?;
    let key = verify(state.keys.as_ref(), secret).await?;
    let ext_ref = end_user_ref(headers);
    let ctx = resolve(state.tenants.as_ref(), &key, ext_ref.as_deref()).await?;
    if ctx.app.markup_policy.funding_mode != FundingMode::TenantReserve {
        return Err(VendError::FundingModeUnsupported);
    }

    let model = body
        .get("model")
        .and_then(|value| value.as_str())
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
        // Streaming bytes are never persisted. A completed key is therefore a
        // consumed single-use token, not a request to replay a partial stream.
        IdempotencyClaim::Replay(_) => return Err(VendError::StreamingIdempotencyConsumed),
        IdempotencyClaim::InProgress => return Err(VendError::IdempotencyInProgress),
        IdempotencyClaim::Conflict => return Err(VendError::IdempotencyConflict),
    }

    let utc_day = Utc::now().format("%Y-%m-%d").to_string();
    let estimate = match state.pricing.estimate_provider_cost(&model, &body) {
        Ok(value) => value,
        Err(error) => {
            state.idempotency.abort(&replay_key, &body_hash).await?;
            return Err(error);
        }
    };
    let estimated_charge = match price(estimate, &ctx.app.markup_policy) {
        Ok(value) => value,
        Err(error) => {
            state.idempotency.abort(&replay_key, &body_hash).await?;
            return Err(error);
        }
    };
    if let Err(error) = check_spend_cap(
        state.usage.as_ref(),
        &ctx.sub_key,
        ctx.end_user.as_ref().map(|end_user| &end_user.id),
        estimated_charge.charge_micros,
        &utc_day,
    )
    .await
    {
        state.idempotency.abort(&replay_key, &body_hash).await?;
        return Err(error);
    }

    let charge_id = ChargeId::generate();
    let mint_idem_key = format!("vend:{replay_key}");
    let usage_row = MeteredUsage {
        charge_id: charge_id.clone(),
        sub_key_id: ctx.sub_key.id.clone(),
        tenant_id: ctx.tenant.id.clone(),
        app_id: ctx.app.id.clone(),
        end_user_id: ctx.end_user.as_ref().map(|end_user| end_user.id.clone()),
        idem_key: mint_idem_key.clone(),
        hold_id: String::new(),
        provider_cost_micros: estimate.get(),
        charge_micros: estimated_charge.charge_micros,
        platform_fee_micros: estimated_charge.platform_fee_micros,
        tenant_net_micros: estimated_charge.tenant_net_micros,
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

    let hold = match state
        .mint
        .create_hold(HoldRequest {
            from: funding_account_id(&ctx).to_string(),
            amount_micros: estimated_charge.charge_micros,
            idem_key: format!("{mint_idem_key}:hold"),
        })
        .await
    {
        Ok(hold) => hold,
        Err(error) => {
            let _ = state.usage.mark_released(&charge_id).await;
            let _ = state.idempotency.abort(&replay_key, &body_hash).await;
            crate::audit_money_event(
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
    crate::audit_money_event(
        state.security_ops.as_ref(),
        &ctx.tenant.id,
        &ctx.sub_key.id,
        &charge_id,
        "money.hold_opened",
        AuditOutcome::Succeeded,
        serde_json::json!({
            "hold_id": hold.hold_id,
            "amount_micros": estimated_charge.charge_micros
        }),
    )
    .await;
    let mut guard = HoldGuard::new(hold.hold_id.clone(), mint_idem_key);
    if let Err(error) = state.usage.attach_hold(&charge_id, &hold.hold_id).await {
        let release = settle(
            state.mint.as_ref(),
            state.usage.as_ref(),
            &charge_id,
            &mut guard,
            &state.settlement_account,
            Terminal::Release,
        )
        .await;
        crate::record_settlement(&state.metrics, &release);
        crate::audit_settlement_event(
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

    // From this point onward the key is single-use even if Relay fails before
    // producing a body: retrying could duplicate upstream work whose delivery
    // boundary Vend cannot prove.
    let upstream = match state
        .relay
        .chat_completion_stream(RelayRequest {
            base_path: state.relay_base_path.clone(),
            bearer: state.relay_key.clone(),
            body,
            zdr: ctx.app.zdr,
        })
        .await
    {
        Ok(upstream) => upstream,
        Err(error) => {
            let release = settle(
                state.mint.as_ref(),
                state.usage.as_ref(),
                &charge_id,
                &mut guard,
                &state.settlement_account,
                Terminal::Release,
            )
            .await;
            crate::record_settlement(&state.metrics, &release);
            crate::audit_settlement_event(
                state.security_ops.as_ref(),
                &ctx.tenant.id,
                &ctx.sub_key.id,
                &charge_id,
                &release,
                "relay_transport_failed",
            )
            .await;
            complete_claim(
                state,
                &replay_key,
                &body_hash,
                &charge_id,
                "relay_transport_error",
            )
            .await;
            return Err(error);
        }
    };

    let response_status = upstream.status;
    let response_headers = upstream.headers.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, io::Error>>(1);

    let mint = state.mint.clone();
    let usage = state.usage.clone();
    let payouts = state.payouts.clone();
    let idempotency = state.idempotency.clone();
    let metrics = state.metrics.clone();
    let settlement_account = state.settlement_account.clone();
    let payout_config = state.payout_config.clone();
    let tenant_payout_account = ctx.tenant.mint_account_id.clone();
    let payout_idem_key = guard.idem_key().to_string();
    let markup = ctx.app.markup_policy;
    let charge_for_task = charge_id.clone();
    let replay_for_task = replay_key.clone();
    let hash_for_task = body_hash.clone();
    let security_for_task = state.security_ops.clone();
    let tenant_for_task = ctx.tenant.id.clone();
    let key_for_task = ctx.sub_key.id.clone();
    tokio::spawn(async move {
        pump_and_settle(
            upstream,
            tx,
            mint,
            usage,
            payouts,
            idempotency,
            metrics,
            charge_for_task,
            &settlement_account,
            &payout_idem_key,
            &payout_config,
            &tenant_payout_account,
            &replay_for_task,
            &hash_for_task,
            estimated_charge.charge_micros,
            markup,
            guard,
            security_for_task,
            tenant_for_task,
            key_for_task,
        )
        .await;
    });

    let response_stream = futures::stream::unfold(rx, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    });
    let mut response = Response::builder()
        .status(StatusCode::from_u16(response_status).unwrap_or(StatusCode::BAD_GATEWAY))
        .body(Body::from_stream(response_stream))
        .map_err(|error| VendError::Transport(error.to_string()))?;
    copy_upstream_headers(response.headers_mut(), &response_headers);
    if let Ok(value) = charge_id.as_str().parse() {
        response.headers_mut().insert("Vend-Charge-Id", value);
    }
    if let Ok(value) = model.parse() {
        response.headers_mut().insert("Vend-Model", value);
    }
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
async fn pump_and_settle(
    mut upstream: RelayStreamResponse,
    tx: tokio::sync::mpsc::Sender<Result<Vec<u8>, io::Error>>,
    mint: std::sync::Arc<dyn vend_core::transport::MintTransport>,
    usage: std::sync::Arc<dyn vend_core::ports::UsageRepo>,
    payouts: std::sync::Arc<dyn vend_core::PayoutRepo>,
    idempotency: std::sync::Arc<dyn vend_core::ports::IdempotencyRepo>,
    metrics: std::sync::Arc<crate::observability::Metrics>,
    charge_id: ChargeId,
    settlement_account: &str,
    payout_idem_key: &str,
    payout_config: &PayoutConfig,
    tenant_payout_account: &str,
    replay_key: &str,
    body_hash: &str,
    held_charge_micros: i64,
    markup: vend_core::money::MarkupPolicy,
    mut guard: HoldGuard,
    security: Option<crate::DataPlaneSecurity>,
    tenant_id: vend_core::ids::TenantId,
    sub_key_id: vend_core::ids::SubKeyId,
) {
    let success = upstream.is_success();
    let mut tracker = SseCostTracker::default();
    let mut terminal_reason = if success {
        "missing_terminal_cost"
    } else {
        "upstream_non_2xx"
    };
    let mut disconnected = false;

    loop {
        let item = tokio::select! {
            _ = tx.closed() => {
                disconnected = true;
                terminal_reason = "client_disconnect";
                break;
            }
            item = upstream.stream.next() => item,
        };
        let Some(item) = item else { break };
        match item {
            Ok(bytes) => {
                if success {
                    tracker.push(&bytes);
                }
                // A dropped response body closes the receiver. Never continue
                // consuming/billing an inference the caller can no longer see.
                if tx.send(Ok(bytes)).await.is_err() {
                    disconnected = true;
                    terminal_reason = "client_disconnect";
                    break;
                }
            }
            Err(error) => {
                terminal_reason = "relay_body_transport_error";
                let _ = tx.send(Err(io::Error::other(error.client_message()))).await;
                break;
            }
        }
    }

    let mut terminal = if success && !disconnected {
        match tracker.finish() {
            Some(cost) => match price(Micros::new(cost), &markup) {
                Ok(actual) if actual.charge_micros <= held_charge_micros => {
                    terminal_reason = "captured";
                    Terminal::Capture { actual }
                }
                Ok(_) => {
                    terminal_reason = "billing_bound_exceeded";
                    Terminal::Release
                }
                Err(_) => {
                    terminal_reason = "invalid_terminal_cost";
                    Terminal::Release
                }
            },
            None => Terminal::Release,
        }
    } else {
        Terminal::Release
    };

    // A successful terminal may only be captured after its immutable payout
    // plan is durable. This keeps a post-capture process failure replayable by
    // the payout reconciler instead of stranding money in clearing.
    if let Terminal::Capture { actual } = terminal {
        if let Err(error) = prepare_payout(
            payouts.as_ref(),
            &charge_id,
            payout_idem_key,
            payout_config,
            tenant_payout_account,
            actual,
        )
        .await
        {
            tracing::error!(%error, charge_id = %charge_id.as_str(), "stream payout preparation failed");
            crate::audit_money_event(
                security.as_ref(),
                &tenant_id,
                &sub_key_id,
                &charge_id,
                "money.payout_failed",
                AuditOutcome::Failed,
                serde_json::json!({"phase": "prepare"}),
            )
            .await;
            terminal_reason = "payout_prepare_error";
            terminal = Terminal::Release;
        }
    }

    let settlement = settle(
        mint.as_ref(),
        usage.as_ref(),
        &charge_id,
        &mut guard,
        settlement_account,
        terminal,
    )
    .await;
    crate::audit_settlement_event(
        security.as_ref(),
        &tenant_id,
        &sub_key_id,
        &charge_id,
        &settlement,
        terminal_reason,
    )
    .await;
    match settlement {
        Ok(vend_settlement::SettleOutcome::Captured(_)) => {
            metrics.settlement_captured();
            let payout = execute_payout(mint.as_ref(), payouts.as_ref(), &charge_id).await;
            crate::audit_money_event(
                security.as_ref(),
                &tenant_id,
                &sub_key_id,
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
            if let Err(error) = payout {
                // Capture is already final. Keep the consumed key and durable
                // pending payout legs for the reconciler; never rerun inference.
                tracing::error!(%error, charge_id = %charge_id.as_str(), "stream payout execution failed after capture");
                terminal_reason = "payout_execution_pending";
                let _ = tx
                    .send(Err(io::Error::other(
                        "Vend captured the charge but payout reconciliation is pending",
                    )))
                    .await;
            }
        }
        Ok(vend_settlement::SettleOutcome::Released) => metrics.settlement_released(),
        Err(error) => {
            metrics.settlement_failed();
            tracing::error!(%error, charge_id = %charge_id.as_str(), "stream settlement failed");
            terminal_reason = "settlement_error";
            let _ = tx
                .send(Err(io::Error::other(
                    "Vend could not finalize stream settlement",
                )))
                .await;
        }
    }

    let metadata = serde_json::json!({
        "kind": "stream_terminal",
        "charge_id": charge_id.as_str(),
        "terminal": terminal_reason,
    })
    .to_string();
    if let Err(error) = idempotency.complete(replay_key, body_hash, &metadata).await {
        tracing::error!(%error, charge_id = %charge_id.as_str(), "failed to consume streaming idempotency key");
    }
}

async fn complete_claim(
    state: &AppState,
    replay_key: &str,
    body_hash: &str,
    charge_id: &ChargeId,
    terminal: &str,
) {
    let metadata = serde_json::json!({
        "kind": "stream_terminal",
        "charge_id": charge_id.as_str(),
        "terminal": terminal,
    })
    .to_string();
    if let Err(error) = state
        .idempotency
        .complete(replay_key, body_hash, &metadata)
        .await
    {
        tracing::error!(%error, charge_id = %charge_id.as_str(), "failed to consume streaming idempotency key");
    }
}

fn copy_upstream_headers(target: &mut HeaderMap, source: &[(String, Vec<u8>)]) {
    for (name, value) in source {
        // Relay is a separate trust/domain boundary. Forward only response
        // metadata that customers need; never pass cookies, auth challenges,
        // proxy credentials, hop-by-hop fields, or arbitrary vendor headers.
        let normalized = name.to_ascii_lowercase();
        if !matches!(
            normalized.as_str(),
            "content-type" | "cache-control" | "retry-after" | "x-request-id"
        ) && !normalized.starts_with("x-ratelimit-")
        {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::from_bytes(value),
        ) {
            target.append(name, value);
        }
    }
}

/// Incremental SSE parser used only for deciding settlement. It never changes
/// the forwarded bytes. Capture requires the final data event before `[DONE]`
/// to contain an exactly representable non-negative `tensorzero_cost`.
#[derive(Default)]
struct SseCostTracker {
    line: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    terminal_cost: Option<i64>,
    done: bool,
    malformed: bool,
}

impl SseCostTracker {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.process_line(line);
            } else {
                self.line.push(byte);
            }
        }
    }

    fn process_line(&mut self, line: Vec<u8>) {
        if line.is_empty() {
            self.process_event();
            return;
        }
        if line.starts_with(b":") {
            return;
        }
        if let Some(mut data) = line.strip_prefix(b"data:").map(ToOwned::to_owned) {
            if data.first() == Some(&b' ') {
                data.remove(0);
            }
            self.data_lines.push(data);
        }
    }

    fn process_event(&mut self) {
        if self.data_lines.is_empty() {
            return;
        }
        let mut payload = Vec::new();
        for (index, line) in std::mem::take(&mut self.data_lines).into_iter().enumerate() {
            if index > 0 {
                payload.push(b'\n');
            }
            payload.extend(line);
        }
        if self.done {
            self.malformed = true;
            return;
        }
        if payload == b"[DONE]" {
            self.done = true;
            return;
        }
        match serde_json::from_slice::<serde_json::Value>(&payload) {
            Ok(chunk) => {
                // The usage-bearing frame must be terminal, immediately before
                // `[DONE]`; a later ordinary chunk invalidates an earlier cost.
                self.terminal_cost =
                    parse_usage(&chunk).and_then(|usage| usage.provider_cost_micros);
            }
            Err(_) => {
                self.malformed = true;
                self.terminal_cost = None;
            }
        }
    }

    fn finish(mut self) -> Option<i64> {
        // A well-formed event must end at a line boundary and `[DONE]` is
        // required; EOF by itself is an aborted stream.
        if !self.line.is_empty() || !self.data_lines.is_empty() {
            self.malformed = true;
        }
        if self.done && !self.malformed {
            self.terminal_cost.filter(|cost| *cost >= 0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_upstream_headers, SseCostTracker};
    use axum::http::HeaderMap;

    #[test]
    fn forwards_only_customer_safe_stream_headers() {
        let mut target = HeaderMap::new();
        copy_upstream_headers(
            &mut target,
            &[
                ("content-type".into(), b"text/event-stream".to_vec()),
                ("x-ratelimit-remaining-requests".into(), b"42".to_vec()),
                ("set-cookie".into(), b"relay_session=secret".to_vec()),
                ("www-authenticate".into(), b"Bearer realm=relay".to_vec()),
                ("proxy-authenticate".into(), b"Basic".to_vec()),
                ("proxy-authorization".into(), b"Basic secret".to_vec()),
                ("te".into(), b"trailers".to_vec()),
                ("trailer".into(), b"digest".to_vec()),
                ("connection".into(), b"keep-alive".to_vec()),
            ],
        );
        assert_eq!(target["content-type"], "text/event-stream");
        assert_eq!(target["x-ratelimit-remaining-requests"], "42");
        for forbidden in [
            "set-cookie",
            "www-authenticate",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "connection",
        ] {
            assert!(!target.contains_key(forbidden));
        }
    }

    #[test]
    fn parses_split_terminal_cost_without_touching_bytes() {
        let mut tracker = SseCostTracker::default();
        tracker.push(b"data: {\"choices\":[{}],\"usage\":null}\n\n");
        tracker.push(b"data: {\"choices\":[],\"usage\":{\"input_tokens\":1,");
        tracker.push(b"\"output_tokens\":2,\"tensorzero_cost\":0.000003}}\n\n");
        tracker.push(b"data: [DO");
        tracker.push(b"NE]\n\n");
        assert_eq!(tracker.finish(), Some(3));
    }

    #[test]
    fn rejects_missing_done_or_nonterminal_usage() {
        let mut no_done = SseCostTracker::default();
        no_done.push(b"data: {\"usage\":{\"tensorzero_cost\":0.000003}}\n\n");
        assert_eq!(no_done.finish(), None);

        let mut followed = SseCostTracker::default();
        followed.push(b"data: {\"usage\":{\"tensorzero_cost\":0.000003}}\n\n");
        followed.push(b"data: {\"choices\":[{}],\"usage\":null}\n\n");
        followed.push(b"data: [DONE]\n\n");
        assert_eq!(followed.finish(), None);
    }
}
