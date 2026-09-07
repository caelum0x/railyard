//! One-shot, lease-safe convergence of Vend's local money projection to Mint.

use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use vend_core::error::VendError;
use vend_core::payout::{PayoutExecution, PayoutRepo};
use vend_core::transport::{HoldRequest, HoldStatus, HoldView, MintTransport, ReleaseRequest};
use vend_mint_client::{MintClient, MintClientConfig};
use vend_settlement::{execute_payout, plan_attached_reap, AttachedReap};
use vend_store::{AuditActorKind, AuditOutcome, NewAuditEvent, SecurityOpsRepo, VendStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    // `--reap-holds`: run the conservative hold-reaper only. It releases
    // panic-orphaned holds (pre-attachment intents and status-`Open` attached
    // holds past the TTL) and converges already-`Released` holds, but never
    // restores captures or drives payouts — those richer, money-moving passes
    // belong to a full reconcile run (no flag). A `Captured` hold is skipped and
    // left for the next full pass; its crash-expiring lease releases on its own.
    let reap_holds = std::env::args().skip(1).any(|arg| arg == "--reap-holds");
    let database_url = require("DATABASE_URL")?;
    let mint = MintClient::with_config(
        require("MINT_BASE")?,
        require("MINT_API_KEY")?,
        MintClientConfig::default(),
    )
    .context("invalid Mint HTTP configuration")?;
    let minimum_age_seconds = parse("VEND_RECONCILE_MIN_AGE_SECONDS", 1_800_i64)?;
    let maximum_stream_seconds = parse("VEND_RELAY_MAX_STREAM_SECONDS", 900_i64)?;
    let limit = parse("VEND_RECONCILE_LIMIT", 100_i64)?;
    let lease_seconds = parse("VEND_RECONCILE_LEASE_SECONDS", 120_i64)?;
    if !(30..=3_600).contains(&maximum_stream_seconds) {
        bail!("VEND_RELAY_MAX_STREAM_SECONDS must be between 30 and 3600")
    }
    if minimum_age_seconds < maximum_stream_seconds.saturating_add(60) {
        bail!("VEND_RECONCILE_MIN_AGE_SECONDS must be at least VEND_RELAY_MAX_STREAM_SECONDS + 60")
    }
    if !(30..=3_600).contains(&lease_seconds) {
        bail!("VEND_RECONCILE_LEASE_SECONDS must be between 30 and 3600")
    }
    if !(1..=1_000).contains(&limit) {
        bail!("VEND_RECONCILE_LIMIT must be between 1 and 1000")
    }

    let store = VendStore::connect(&database_url)
        .await
        .context("connecting to Vend Postgres")?;
    store.migrate().await.context("running Vend migrations")?;
    let older_than = Utc::now()
        - chrono::Duration::from_std(Duration::from_secs(minimum_age_seconds as u64))
            .context("invalid reconciliation age")?;
    let worker_token = format!(
        "{}:{}:{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "vend-reconcile".into()),
        std::process::id(),
        Utc::now().timestamp_micros()
    );
    let unattached = store
        .lease_unattached_holds(older_than, limit, lease_seconds, &worker_token)
        .await
        .context("leasing unattached hold intents")?;
    let mut unattached_released = 0_u64;
    let mut attached_released = 0_u64;
    let mut captures_restored = 0_u64;
    let mut payouts_completed = 0_u64;
    let mut reaper_skipped_capture = 0_u64;
    let mut failed = 0_u64;
    for candidate in unattached {
        let result = async {
            let hold = match mint
                .create_hold(HoldRequest {
                    from: candidate.payer_account.clone(),
                    amount_micros: candidate.amount_micros,
                    idem_key: format!("{}:hold", candidate.idem_key),
                })
                .await
            {
                Ok(hold) => Some(hold),
                Err(VendError::InsufficientFunds | VendError::ReserveInsufficient) => None,
                Err(error) => return Err(error),
            };
            if let Some(hold) = hold {
                mint.release_hold(
                    &hold.hold_id,
                    ReleaseRequest {
                        idem_key: format!("{}:release", candidate.idem_key),
                    },
                )
                .await?;
            }
            store
                .finish_leased_unattached_release(&candidate.charge_id, &worker_token)
                .await
        }
        .await;
        match result {
            Ok(()) => {
                unattached_released += 1;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.hold_released",
                    AuditOutcome::Succeeded,
                    "unattached_hold",
                    None,
                )
                .await;
            }
            Err(error) => {
                failed += 1;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.failed",
                    AuditOutcome::Failed,
                    "unattached_hold",
                    Some(error.code()),
                )
                .await;
                tracing::error!(
                    charge_id = candidate.charge_id.as_str(),
                    phase = "unattached_hold",
                    %error,
                    "reconciliation candidate failed"
                );
            }
        }
    }

    // The attached-hold pass IS the reaper for panic-orphaned holds: it leases
    // stale (older_than = TTL) still-`pending` rows that already carry a Mint
    // hold id, re-reads Mint's authoritative status, and releases only a hold
    // Mint still reports `Open`. `classify_hold` centralizes the "never release
    // a captured hold" rule shared with `vend_settlement::reap`.
    let attached_phase = if reap_holds {
        "reaped"
    } else {
        "attached_hold"
    };
    let attached = store
        .lease_attached_holds(older_than, limit, lease_seconds, &worker_token)
        .await
        .context("leasing attached hold intents")?;
    for candidate in attached {
        let result: Result<&'static str, VendError> = async {
            let hold = mint.get_hold(&candidate.hold_id).await?;
            if hold.id != candidate.hold_id {
                return Err(VendError::Storage("Mint returned the wrong hold id".into()));
            }
            // One audited, unit-tested decision (`plan_attached_reap`) maps
            // Mint's authoritative status + the fail-closed payout signal to the
            // single convergence step; this binary is a thin executor over it.
            match plan_attached_reap(hold.status, candidate.payout_started, reap_holds)? {
                AttachedReap::Release => {
                    mint.release_hold(
                        &candidate.hold_id,
                        ReleaseRequest {
                            idem_key: format!("{}:release", candidate.idem_key),
                        },
                    )
                    .await?;
                    store
                        .finish_attached_release(&candidate.charge_id, &worker_token)
                        .await?;
                    Ok("released")
                }
                AttachedReap::ConvergeReleased => {
                    store
                        .finish_attached_release(&candidate.charge_id, &worker_token)
                        .await?;
                    Ok("released")
                }
                // The reaper never restores captures or moves money. Leave a
                // captured hold for the next full reconcile pass; its lease is
                // crash-expiring, so deferring never loses the row.
                AttachedReap::DeferCapture => Ok("skipped_capture"),
                AttachedReap::RestoreCapture => {
                    let plan = store
                        .load_payout(&candidate.charge_id)
                        .await?
                        .ok_or_else(|| {
                            VendError::Storage(
                                "captured Mint hold has no immutable payout plan".into(),
                            )
                        })?;
                    validate_captured_plan(&hold, &plan)?;
                    store
                        .finish_attached_capture(&candidate.charge_id, &worker_token)
                        .await?;
                    execute_payout(&mint, &store, &candidate.charge_id).await?;
                    Ok("captured")
                }
            }
        }
        .await;
        match result {
            Ok("released") => {
                attached_released += 1;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.hold_released",
                    AuditOutcome::Succeeded,
                    attached_phase,
                    None,
                )
                .await;
            }
            Ok("skipped_capture") => {
                reaper_skipped_capture += 1;
                tracing::info!(
                    charge_id = candidate.charge_id.as_str(),
                    hold_id = candidate.hold_id,
                    "hold-reaper skipped a captured hold; deferring to a full reconcile pass"
                );
            }
            Ok("captured") => {
                captures_restored += 1;
                payouts_completed += 1;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.capture_restored",
                    AuditOutcome::Succeeded,
                    "attached_hold",
                    None,
                )
                .await;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.payout_completed",
                    AuditOutcome::Succeeded,
                    "attached_hold",
                    None,
                )
                .await;
            }
            Ok(other) => {
                // Defensive: the async block above only ever yields the three
                // strings handled above. Treat any unexpected outcome as a
                // skipped candidate + audit failure rather than panicking the
                // whole reconcile worker on one row (which would strand every
                // remaining candidate in this and later passes).
                failed += 1;
                let _ = store
                    .fail_attached_reconciliation(
                        &candidate.charge_id,
                        &worker_token,
                        &format!("unexpected reconcile outcome: {other:?}"),
                    )
                    .await;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.failed",
                    AuditOutcome::Failed,
                    attached_phase,
                    Some("unexpected_outcome"),
                )
                .await;
                tracing::error!(
                    charge_id = candidate.charge_id.as_str(),
                    hold_id = candidate.hold_id,
                    phase = attached_phase,
                    outcome = other,
                    "reconciliation candidate returned an unexpected outcome; skipping"
                );
            }
            Err(error) => {
                failed += 1;
                let _ = store
                    .fail_attached_reconciliation(
                        &candidate.charge_id,
                        &worker_token,
                        &error.to_string(),
                    )
                    .await;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.failed",
                    AuditOutcome::Failed,
                    attached_phase,
                    Some(error.code()),
                )
                .await;
                tracing::error!(
                    charge_id = candidate.charge_id.as_str(),
                    hold_id = candidate.hold_id,
                    phase = attached_phase,
                    %error,
                    "reconciliation candidate failed"
                );
            }
        }
    }

    // A process may have committed local capture and then died between payout
    // legs. Re-read Mint for every row; local state alone never authorizes a
    // transfer from clearing.
    let payout_cutoff = Utc::now()
        - chrono::Duration::from_std(Duration::from_secs(minimum_age_seconds as u64))
            .context("invalid payout reconciliation age")?;
    // The hold-reaper never drives payouts (money movement out of clearing);
    // that is a full-reconcile responsibility. Skip leasing in `--reap-holds`.
    let payouts = if reap_holds {
        Vec::new()
    } else {
        store
            .lease_pending_payouts(payout_cutoff, limit, lease_seconds, &worker_token)
            .await
            .context("leasing incomplete payouts")?
    };
    for candidate in payouts {
        let result: Result<(), VendError> = async {
            let hold = mint.get_hold(&candidate.hold_id).await?;
            let plan = store
                .load_payout(&candidate.charge_id)
                .await?
                .ok_or(VendError::NotFound)?;
            if hold.id != candidate.hold_id {
                return Err(VendError::Storage("Mint returned the wrong hold id".into()));
            }
            validate_captured_plan(&hold, &plan)?;
            execute_payout(&mint, &store, &candidate.charge_id).await
        }
        .await;
        match result {
            Ok(()) => {
                payouts_completed += 1;
                store
                    .finish_payout_lease(&candidate.charge_id, &worker_token, None)
                    .await?;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.payout_completed",
                    AuditOutcome::Succeeded,
                    "payout",
                    None,
                )
                .await;
            }
            Err(error) => {
                failed += 1;
                let _ = store
                    .finish_payout_lease(
                        &candidate.charge_id,
                        &worker_token,
                        Some(&error.to_string()),
                    )
                    .await;
                audit_reconciliation(
                    &store,
                    &worker_token,
                    &candidate.charge_id,
                    "reconciliation.payout_pending",
                    AuditOutcome::Failed,
                    "payout",
                    Some(error.code()),
                )
                .await;
                tracing::error!(
                    charge_id = candidate.charge_id.as_str(),
                    hold_id = candidate.hold_id,
                    phase = "payout",
                    %error,
                    "reconciliation candidate failed"
                );
            }
        }
    }

    println!(
        "{}",
        serde_json::json!({
            "worker": worker_token,
            "mode": if reap_holds { "reap_holds" } else { "reconcile" },
            "unattached_released": unattached_released,
            "attached_released": attached_released,
            "captures_restored": captures_restored,
            "payouts_completed": payouts_completed,
            "reaper_skipped_capture": reaper_skipped_capture,
            "failed": failed
        })
    );
    if failed > 0 {
        bail!("{failed} candidate(s) still require reconciliation")
    }
    Ok(())
}

async fn audit_reconciliation(
    store: &VendStore,
    worker_token: &str,
    charge_id: &vend_core::ChargeId,
    action: &str,
    outcome: AuditOutcome,
    phase: &str,
    error_code: Option<&str>,
) {
    let event = NewAuditEvent {
        tenant_id: None,
        actor_kind: AuditActorKind::System,
        actor_id: Some(worker_token.to_string()),
        action: action.to_string(),
        target_kind: Some("charge".into()),
        target_id: Some(charge_id.as_str().to_string()),
        outcome,
        request_id: None,
        source_hash: None,
        metadata: serde_json::json!({"phase": phase, "error_code": error_code}),
    };
    if let Err(error) = store.append_audit_event(event).await {
        tracing::error!(%error, charge_id = charge_id.as_str(), action, "reconciliation audit append failed");
    }
}

fn validate_captured_plan(hold: &HoldView, plan: &PayoutExecution) -> Result<(), VendError> {
    if hold.status != HoldStatus::Captured {
        return Err(VendError::Storage(
            "Mint does not confirm a captured hold".into(),
        ));
    }
    if hold.captured_micros != plan.captured_charge_micros {
        return Err(VendError::Storage(format!(
            "Mint captured {} micros but payout plan records {}",
            hold.captured_micros, plan.captured_charge_micros
        )));
    }
    Ok(())
}

fn require(name: &str) -> anyhow::Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => bail!("{name} must be set"),
    }
}

fn parse<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse()
            .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}")),
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vend_core::payout::PayoutState;

    fn plan() -> PayoutExecution {
        PayoutExecution {
            charge_id: vend_core::ChargeId::generate(),
            idem_key: "vend:test".into(),
            clearing_account_id: "clearing".into(),
            provider_account_id: "provider".into(),
            tenant_account_id: "tenant".into(),
            platform_account_id: "platform".into(),
            provider_cost_micros: 700,
            tenant_net_micros: 240,
            platform_fee_micros: 60,
            captured_charge_micros: 1_000,
            provider_transferred: false,
            tenant_transferred: false,
            platform_transferred: false,
            state: PayoutState::Pending,
            last_error: None,
        }
    }

    fn hold(status: HoldStatus, captured_micros: i64) -> HoldView {
        HoldView {
            id: "hold_test".into(),
            account_id: "payer".into(),
            amount_micros: 1_200,
            captured_micros,
            status,
            ref_: None,
        }
    }

    #[test]
    fn payout_authorization_requires_authoritative_exact_capture() {
        let plan = plan();
        assert!(validate_captured_plan(&hold(HoldStatus::Captured, 1_000), &plan).is_ok());
        assert!(validate_captured_plan(&hold(HoldStatus::Open, 0), &plan).is_err());
        assert!(validate_captured_plan(&hold(HoldStatus::Released, 0), &plan).is_err());
        assert!(validate_captured_plan(&hold(HoldStatus::Captured, 999), &plan).is_err());
    }
}
