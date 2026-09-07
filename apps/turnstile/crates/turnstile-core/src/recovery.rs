//! Lease-based recovery for interrupted verification and settlement commands.
//!
//! Recovery never invokes application handlers. It only replays the exact Mint
//! command already recorded in the settlement journal. An abandoned `Held`
//! authorization is released; `CapturePending` and `ReleasePending` can only
//! replay their persisted terminal direction.

use std::sync::Arc;
use std::time::Duration;

use rand::RngCore;

use crate::clock::now_ms;
use crate::error::TurnstileError;
use crate::guard::verification_may_have_created_hold;
use crate::journal::{SettlementJournal, SettlementPhase, SettlementRecord};
use crate::verifier::PaymentVerifier;

/// Summary of one bounded recovery pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    pub claimed: usize,
    pub completed: usize,
    pub failed: usize,
}

/// Claim and reconcile up to `limit` expired commands.
pub async fn reconcile_once(
    journal: Arc<dyn SettlementJournal>,
    verifier: Arc<dyn PaymentVerifier>,
    worker_token: &str,
    lease: Duration,
    limit: usize,
) -> Result<RecoveryReport, TurnstileError> {
    let now = now_ms();
    let lease_until = now.saturating_add(lease.as_millis() as i64);
    let records = journal
        .claim_recoverable(worker_token, now, lease_until, limit)
        .await?;
    let mut report = RecoveryReport {
        claimed: records.len(),
        ..RecoveryReport::default()
    };

    for record in records {
        match reconcile_record(
            journal.as_ref(),
            verifier.as_ref(),
            worker_token,
            lease_until,
            record,
        )
        .await
        {
            Ok(()) => report.completed += 1,
            Err(error) => {
                report.failed += 1;
                eprintln!("turnstile settlement recovery failed: {error}");
            }
        }
    }
    Ok(report)
}

async fn reconcile_record(
    journal: &dyn SettlementJournal,
    verifier: &dyn PaymentVerifier,
    worker_token: &str,
    lease_until_ms: i64,
    record: SettlementRecord,
) -> Result<(), TurnstileError> {
    match record.phase {
        SettlementPhase::Verifying => match verifier.verify(&record.authorization).await {
            Ok(verified) => {
                let verified = journal
                    .bind_verified(
                        record.nonce(),
                        worker_token,
                        verified.hold.as_ref(),
                        verified.scheme,
                        lease_until_ms,
                    )
                    .await?;
                if verified.phase == SettlementPhase::Held {
                    release_held(journal, verifier, worker_token, lease_until_ms, &verified)
                        .await?;
                }
            }
            Err(error) if verification_may_have_created_hold(&error) => {
                journal
                    .record_error(record.nonce(), worker_token, &error.to_string())
                    .await?;
                return Err(error);
            }
            Err(error) => {
                journal
                    .reject_verification(record.nonce(), worker_token, &error.to_string())
                    .await?;
            }
        },
        SettlementPhase::Held => {
            release_held(journal, verifier, worker_token, lease_until_ms, &record).await?;
        }
        SettlementPhase::CapturePending => {
            let hold = required_hold(&record)?;
            let actual = record.actual_micros.ok_or_else(|| {
                TurnstileError::Settlement("capture command has no persisted amount".into())
            })?;
            let receipt = match verifier.capture(hold, actual).await {
                Ok(receipt) => receipt,
                Err(error) => {
                    journal
                        .record_error(record.nonce(), worker_token, &error.to_string())
                        .await?;
                    return Err(error);
                }
            };
            journal
                .complete_capture(record.nonce(), worker_token, &receipt)
                .await?;
        }
        SettlementPhase::ReleasePending => {
            let hold = required_hold(&record)?;
            if let Err(error) = verifier.release(hold).await {
                journal
                    .record_error(record.nonce(), worker_token, &error.to_string())
                    .await?;
                return Err(error);
            }
            journal
                .complete_release(record.nonce(), worker_token)
                .await?;
        }
        SettlementPhase::Captured
        | SettlementPhase::Released
        | SettlementPhase::TransferComplete
        | SettlementPhase::Rejected => {}
    }
    Ok(())
}

async fn release_held(
    journal: &dyn SettlementJournal,
    verifier: &dyn PaymentVerifier,
    worker_token: &str,
    lease_until_ms: i64,
    record: &SettlementRecord,
) -> Result<(), TurnstileError> {
    let hold = required_hold(record)?;
    journal
        .prepare_release(record.nonce(), worker_token, lease_until_ms)
        .await?;
    if let Err(error) = verifier.release(hold).await {
        journal
            .record_error(record.nonce(), worker_token, &error.to_string())
            .await?;
        return Err(error);
    }
    journal.complete_release(record.nonce(), worker_token).await
}

fn required_hold(record: &SettlementRecord) -> Result<&aitp_client::HoldRef, TurnstileError> {
    record.hold.as_ref().ok_or_else(|| {
        TurnstileError::Settlement(format!(
            "{} settlement command has no persisted hold identity",
            record.nonce()
        ))
    })
}

/// Spawn periodic bounded recovery. Database leases make multiple replicas
/// safe; the worker token identifies this task's claims.
#[must_use]
pub fn spawn_settlement_reaper(
    journal: Arc<dyn SettlementJournal>,
    verifier: Arc<dyn PaymentVerifier>,
    every: Duration,
    lease: Duration,
    batch_size: usize,
) -> tokio::task::JoinHandle<()> {
    let mut bytes = [0_u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let worker_token = hex::encode(bytes);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(error) = reconcile_once(
                journal.clone(),
                verifier.clone(),
                &worker_token,
                lease,
                batch_size,
            )
            .await
            {
                eprintln!("turnstile settlement recovery sweep failed: {error}");
            }
        }
    })
}
