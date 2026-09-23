//! Background maintenance loops, spawned once by the binary next to the HTTP
//! server:
//!
//! - **payout reconciliation**: every `pending` payout (PayPal batch, USDC
//!   transfer, Stripe transfer …) is re-checked against its provider so a
//!   payout that settles or fails off-platform leaves `pending` without an
//!   operator opening it;
//! - **hold auto-release**: OPEN holds past their deadline (AITP-402 hold
//!   settlements, holds created with `expires_in_ms`) are released back to
//!   the payer, so an abandoned hold never locks funds forever;
//! - **payer-key mirror refresh**: the AITP payer-key registry re-reads the DB
//!   so keys registered / revoked on another instance take effect here.
//!
//! Every loop is best-effort: a failed pass is logged and retried on the next
//! tick; nothing here can move money beyond what the operator-visible API
//! already allows (release an expired hold, refresh a payout status).

use std::time::Duration;

use mint_core::MintError;
use mint_ledger::HoldReleaseRequest;
use mint_store::{LedgerStore, PayoutQuery, PayoutRecordStatus};

use crate::state::AppState;

/// Default sweep interval (60 s).
const DEFAULT_INTERVAL_MS: u64 = 60_000;
/// Smallest accepted sweep interval (5 s) — protects provider rate limits.
const MIN_INTERVAL_MS: u64 = 5_000;
/// Pending payouts refreshed per pass.
const PAYOUT_BATCH: usize = 200;
/// Expired holds released per pass.
const HOLD_BATCH: usize = 500;
/// Default AITP-402 settle window added past the challenge expiry (15 min).
const DEFAULT_AITP_HOLD_TTL_MS: i64 = 15 * 60 * 1000;
/// Upper bound for any hold deadline (30 days).
pub const MAX_HOLD_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Wall-clock unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

/// How long an AITP-402 hold settlement stays open past the challenge expiry
/// before it auto-releases (`MINT_AITP_HOLD_TTL_MS`, default 15 min, capped
/// at 30 days).
pub fn aitp_hold_ttl_ms() -> i64 {
    env_u64("MINT_AITP_HOLD_TTL_MS")
        .map(|v| (v as i64).clamp(1_000, MAX_HOLD_TTL_MS))
        .unwrap_or(DEFAULT_AITP_HOLD_TTL_MS)
}

/// Spawn the maintenance loops for `state` (interval `MINT_SWEEP_INTERVAL_MS`,
/// default 60 s; `MINT_SWEEP_DISABLED=1` turns them off, e.g. for a
/// read-replica instance).
pub fn spawn_background_jobs<S: LedgerStore + 'static>(state: AppState<S>) {
    if std::env::var("MINT_SWEEP_DISABLED").ok().as_deref() == Some("1") {
        tracing::warn!("MINT_SWEEP_DISABLED=1: payout / hold-expiry sweeps are OFF");
        return;
    }
    let interval_ms = env_u64("MINT_SWEEP_INTERVAL_MS")
        .unwrap_or(DEFAULT_INTERVAL_MS)
        .max(MIN_INTERVAL_MS);
    tracing::info!(interval_ms, "background sweeps started");
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            run_once(&state).await;
        }
    });
}

/// One full maintenance pass (also callable from tests / an admin trigger).
pub async fn run_once<S: LedgerStore + 'static>(state: &AppState<S>) -> SweepReport {
    let mut report = SweepReport::default();
    match sweep_payouts(state).await {
        Ok((checked, changed)) => {
            report.payouts_checked = checked;
            report.payouts_changed = changed;
        }
        Err(e) => tracing::warn!(error = %e, "payout sweep failed"),
    }
    match release_expired_holds(state).await {
        Ok(n) => report.holds_released = n,
        Err(e) => tracing::warn!(error = %e, "hold-expiry sweep failed"),
    }
    match state.payer_keys.reload(state.ledger.store()).await {
        Ok(n) => report.payer_keys_active = n,
        Err(e) => tracing::warn!(error = %e, "payer-key refresh failed"),
    }
    report
}

/// What one pass did (logged; returned for callers).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    /// Pending payouts re-checked against their provider.
    pub payouts_checked: usize,
    /// Of those, how many left `pending` (paid / failed).
    pub payouts_changed: usize,
    /// Expired holds released back to their payer.
    pub holds_released: usize,
    /// Active DB-backed payer keys after the refresh.
    pub payer_keys_active: usize,
}

/// Refresh every pending payout from its provider.
async fn sweep_payouts<S: LedgerStore + 'static>(
    state: &AppState<S>,
) -> Result<(usize, usize), MintError> {
    let pending = state
        .ledger
        .store()
        .list_payouts(PayoutQuery {
            status: Some(PayoutRecordStatus::Pending),
            account_id: None,
            before: None,
            limit: PAYOUT_BATCH,
        })
        .await?;
    let mut changed = 0usize;
    for rec in &pending {
        match state.reserve.refresh_payout(&rec.payout_ref).await {
            Ok(Some(after)) if after.status != PayoutRecordStatus::Pending => {
                changed += 1;
                tracing::info!(
                    payout_ref = %after.payout_ref,
                    status = after.status.as_str(),
                    "payout settled by provider"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(payout_ref = %rec.payout_ref, error = %e, "payout refresh failed"),
        }
    }
    Ok((pending.len(), changed))
}

/// Release every OPEN hold whose deadline has passed.
async fn release_expired_holds<S: LedgerStore + 'static>(
    state: &AppState<S>,
) -> Result<usize, MintError> {
    let due = state
        .ledger
        .store()
        .list_expired_holds(now_ms(), HOLD_BATCH)
        .await?;
    let mut released = 0usize;
    for hold_id in due {
        let out = state
            .ledger
            .release_hold(HoldReleaseRequest {
                hold_id: hold_id.clone(),
                // Deterministic key: a crash-retry replays instead of failing.
                idem_key: format!("hold-expiry:{}", hold_id.as_str()),
                ref_: Some("auto-release: hold expired".to_string()),
            })
            .await;
        match out {
            Ok(_) => {
                released += 1;
                tracing::info!(hold_id = %hold_id.as_str(), "expired hold released");
            }
            // Captured / released concurrently by the merchant: nothing to do.
            Err(MintError::HoldNotFound) => {}
            Err(e) => tracing::warn!(hold_id = %hold_id.as_str(), error = %e, "hold auto-release failed"),
        }
    }
    Ok(released)
}
