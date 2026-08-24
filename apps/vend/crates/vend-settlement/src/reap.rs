//! # Hold-reaper — release panic-orphaned Mint holds after a TTL.
//!
//! [`HoldGuard`](crate::HoldGuard) can only *log* when a request path panics
//! between opening a Mint hold and driving [`settle`](crate::settle): async work
//! cannot run in `Drop`, so a panic (or a hard crash) leaves an **orphaned
//! hold** — reserve moved out of the payer's available balance with no capture
//! or release ever recorded. This module is the out-of-band reaper the
//! settlement docs promised.
//!
//! ## Safety (this is money code)
//!
//! The reaper is deliberately conservative. It releases a hold ONLY when all of:
//!
//! - **(a) past TTL** — the hold has lived longer than a configurable
//!   `ttl_seconds`. A hold younger than the TTL could still be legitimately
//!   settling on a live request path, so it is left untouched.
//! - **(b) still pending** — the caller only feeds the reaper rows whose local
//!   projection is still `pending` (no terminal outcome recorded). See
//!   `VendStore::lease_attached_holds`, which selects exactly these rows.
//! - **(c) no successful capture** — Mint's AUTHORITATIVE hold status is
//!   re-read; the reaper releases only a status-`Open` hold. A `Captured` hold
//!   is NEVER released — it is routed back to the capture-settlement path.
//!
//! Releasing is safe-by-idempotency: the reaper reuses the same
//! `{idem_key}:release` key the panic path would have used, so a real in-flight
//! settlement that also releases is deduped by Mint to one operation, and a
//! released hold can never subsequently be captured. Because condition (c)
//! re-reads Mint before acting, the reaper never even *attempts* to release a
//! hold whose capture already succeeded.

use vend_core::error::VendError;
use vend_core::ids::ChargeId;
use vend_core::ports::UsageRepo;
use vend_core::transport::{HoldStatus, HoldView, MintTransport, ReleaseRequest};

/// The money-safe action for one pending hold, decided from Mint's
/// AUTHORITATIVE status. Vend's local `pending` projection alone NEVER
/// authorizes a release — only Mint's reported [`HoldStatus`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapAction {
    /// Mint still reports the hold `Open`: release it (idempotent).
    Release,
    /// Mint already reports the hold `Released`: converge the local projection
    /// only; no Mint call is needed.
    ConvergeReleased,
    /// Mint reports the hold `Captured`: NEVER release. The capture must be
    /// settled (and its payout driven) by the capture-recovery path instead.
    SettleCapture,
}

/// Map Mint's authoritative hold status to the single safe reaper action.
/// Centralizes the "never release a captured hold" rule so both the reusable
/// [`reap_orphaned_hold`] primitive and the `vend-reconcile` binary share one
/// audited decision.
pub fn classify_hold(status: HoldStatus) -> ReapAction {
    match status {
        HoldStatus::Open => ReapAction::Release,
        HoldStatus::Released => ReapAction::ConvergeReleased,
        HoldStatus::Captured => ReapAction::SettleCapture,
    }
}

/// The result of assessing one candidate hold for reaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Younger than the TTL — left untouched so a live settlement can finish.
    WithinTtl,
    /// Was `Open` past the TTL: the orphaned hold was released and the local
    /// projection marked released.
    Reaped,
    /// Mint already had the hold `Released`: local projection converged, no
    /// funds movement.
    AlreadyReleased,
    /// Mint reports the hold `Captured`: intentionally NOT reaped.
    CapturedNotReaped,
}

/// One pending-hold candidate handed to the reaper. `age_seconds` is
/// `now - hold_opened_at` (the row's `created_at`), computed by the caller.
#[derive(Debug, Clone)]
pub struct ReapCandidate<'a> {
    /// The metered-usage row / charge whose hold may be orphaned.
    pub charge_id: &'a ChargeId,
    /// The Mint hold id backing the charge.
    pub hold_id: &'a str,
    /// The stable per-charge idempotency prefix; the release key is derived as
    /// `{idem_key}:release`, matching the settlement state machine verbatim.
    pub idem_key: &'a str,
    /// Seconds elapsed since the hold was opened (`now - created_at`).
    pub age_seconds: i64,
}

/// `true` iff a hold this old is eligible for reaping under `ttl_seconds`.
/// Pure and side-effect free so the TTL boundary is trivially testable.
pub fn past_ttl(age_seconds: i64, ttl_seconds: i64) -> bool {
    age_seconds >= ttl_seconds
}

/// Reap a single panic-orphaned hold, conservatively.
///
/// Enforces safety condition (a) locally (TTL) and re-reads Mint for conditions
/// (c) capture-safety before any release. The caller is responsible for
/// condition (b): only feed rows whose local projection is still `pending`.
///
/// - Within TTL → [`ReapOutcome::WithinTtl`] (no Mint call, nothing changed).
/// - Mint `Open` → `release_hold({idem}:release)` then `mark_released` →
///   [`ReapOutcome::Reaped`].
/// - Mint `Released` → `mark_released` (idempotent converge) →
///   [`ReapOutcome::AlreadyReleased`].
/// - Mint `Captured` → returns [`ReapOutcome::CapturedNotReaped`] WITHOUT
///   releasing or mutating the local outcome.
///
/// Running twice is safe: the second pass sees Mint `Released` and converges
/// idempotently, and `mark_released` is a no-op on an already-released row.
pub async fn reap_orphaned_hold(
    mint: &dyn MintTransport,
    usage: &dyn UsageRepo,
    candidate: &ReapCandidate<'_>,
    ttl_seconds: i64,
) -> Result<ReapOutcome, VendError> {
    // (a) Never touch a hold that could still be legitimately settling.
    if !past_ttl(candidate.age_seconds, ttl_seconds) {
        return Ok(ReapOutcome::WithinTtl);
    }

    // (c) Mint is authoritative. Re-read the hold before any decision so a
    // capture that already succeeded is observed and never released.
    let hold: HoldView = mint.get_hold(candidate.hold_id).await?;
    if hold.id != candidate.hold_id {
        return Err(VendError::Storage("Mint returned the wrong hold id".into()));
    }

    match classify_hold(hold.status) {
        ReapAction::Release => {
            mint.release_hold(
                candidate.hold_id,
                ReleaseRequest {
                    idem_key: format!("{}:release", candidate.idem_key),
                },
            )
            .await?;
            usage.mark_released(candidate.charge_id).await?;
            Ok(ReapOutcome::Reaped)
        }
        ReapAction::ConvergeReleased => {
            usage.mark_released(candidate.charge_id).await?;
            Ok(ReapOutcome::AlreadyReleased)
        }
        ReapAction::SettleCapture => Ok(ReapOutcome::CapturedNotReaped),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vend_core::ids::{AppId, SubKeyId, TenantId};
    use vend_core::ports::{MeteredUsage, UsageOutcome, UsageRepo};
    use vend_core::transport::{CaptureRequest, HoldRequest, MintTransport};
    use vend_testkit::{FakeMint, InMemoryStore};

    const TTL: i64 = 1_800;
    const PAYER: &str = "acct_payer";
    const SETTLEMENT: &str = "acct_settlement";

    fn pending_usage(charge_id: &ChargeId, hold_id: &str, idem_key: &str) -> MeteredUsage {
        MeteredUsage {
            charge_id: charge_id.clone(),
            sub_key_id: SubKeyId::generate(),
            tenant_id: TenantId::generate(),
            app_id: AppId::generate(),
            end_user_id: None,
            idem_key: idem_key.to_string(),
            hold_id: hold_id.to_string(),
            provider_cost_micros: 0,
            charge_micros: 6_000,
            platform_fee_micros: 0,
            tenant_net_micros: 0,
            outcome: UsageOutcome::Pending,
            utc_day: "2026-08-24".to_string(),
        }
    }

    async fn open_hold(mint: &FakeMint, idem_key: &str) -> String {
        mint.create_hold(HoldRequest {
            from: PAYER.into(),
            amount_micros: 6_000,
            idem_key: format!("{idem_key}:hold"),
        })
        .await
        .unwrap()
        .hold_id
    }

    #[tokio::test]
    async fn orphaned_past_ttl_hold_is_reaped_and_released() {
        let mint = FakeMint::new().with_balance(PAYER, 10_000);
        let store = InMemoryStore::new();
        let charge_id = ChargeId::generate();
        let idem = "vend:job1";
        let hold_id = open_hold(&mint, idem).await;
        // The reserve is held out of available balance while orphaned.
        assert_eq!(mint.balance(PAYER), 4_000);
        store
            .record_pending(&pending_usage(&charge_id, &hold_id, idem))
            .await
            .unwrap();

        let candidate = ReapCandidate {
            charge_id: &charge_id,
            hold_id: &hold_id,
            idem_key: idem,
            age_seconds: TTL, // exactly at the TTL boundary → eligible
        };
        let outcome = reap_orphaned_hold(&mint, &store, &candidate, TTL)
            .await
            .unwrap();

        assert_eq!(outcome, ReapOutcome::Reaped);
        // Released reserve is returned to the payer, and the local row converges.
        assert_eq!(mint.balance(PAYER), 10_000);
        assert_eq!(
            store
                .usage_by_charge_id(charge_id.as_str())
                .unwrap()
                .outcome,
            UsageOutcome::Released
        );
    }

    #[tokio::test]
    async fn fresh_pending_hold_within_ttl_is_not_reaped() {
        let mint = FakeMint::new().with_balance(PAYER, 10_000);
        let store = InMemoryStore::new();
        let charge_id = ChargeId::generate();
        let idem = "vend:job2";
        let hold_id = open_hold(&mint, idem).await;
        store
            .record_pending(&pending_usage(&charge_id, &hold_id, idem))
            .await
            .unwrap();

        let candidate = ReapCandidate {
            charge_id: &charge_id,
            hold_id: &hold_id,
            idem_key: idem,
            age_seconds: TTL - 1, // one second short of the TTL
        };
        let outcome = reap_orphaned_hold(&mint, &store, &candidate, TTL)
            .await
            .unwrap();

        assert_eq!(outcome, ReapOutcome::WithinTtl);
        // Hold is left intact: reserve still held, row still pending, Mint Open.
        assert_eq!(mint.balance(PAYER), 4_000);
        assert_eq!(
            store
                .usage_by_charge_id(charge_id.as_str())
                .unwrap()
                .outcome,
            UsageOutcome::Pending
        );
        assert_eq!(
            mint.get_hold(&hold_id).await.unwrap().status,
            HoldStatus::Open
        );
    }

    #[tokio::test]
    async fn captured_hold_is_never_reaped() {
        let mint = FakeMint::new().with_balance(PAYER, 10_000);
        let store = InMemoryStore::new();
        let charge_id = ChargeId::generate();
        let idem = "vend:job3";
        let hold_id = open_hold(&mint, idem).await;
        // A crash AFTER Mint captured but BEFORE the local projection updated:
        // Mint is Captured, yet the row is still pending.
        mint.capture_hold(
            &hold_id,
            CaptureRequest {
                amount_micros: 5_000,
                to: SETTLEMENT.into(),
                idem_key: format!("{idem}:capture"),
            },
        )
        .await
        .unwrap();
        store
            .record_pending(&pending_usage(&charge_id, &hold_id, idem))
            .await
            .unwrap();
        let settlement_before = mint.balance(SETTLEMENT);

        let candidate = ReapCandidate {
            charge_id: &charge_id,
            hold_id: &hold_id,
            idem_key: idem,
            age_seconds: TTL * 10, // very old — but capture protects it
        };
        let outcome = reap_orphaned_hold(&mint, &store, &candidate, TTL)
            .await
            .unwrap();

        assert_eq!(outcome, ReapOutcome::CapturedNotReaped);
        // The reaper must NOT release a captured hold or touch the projection.
        assert_eq!(
            mint.get_hold(&hold_id).await.unwrap().status,
            HoldStatus::Captured
        );
        assert_eq!(mint.balance(SETTLEMENT), settlement_before);
        assert_eq!(
            store
                .usage_by_charge_id(charge_id.as_str())
                .unwrap()
                .outcome,
            UsageOutcome::Pending
        );
    }

    #[tokio::test]
    async fn reaping_twice_is_idempotent() {
        let mint = FakeMint::new().with_balance(PAYER, 10_000);
        let store = InMemoryStore::new();
        let charge_id = ChargeId::generate();
        let idem = "vend:job4";
        let hold_id = open_hold(&mint, idem).await;
        store
            .record_pending(&pending_usage(&charge_id, &hold_id, idem))
            .await
            .unwrap();
        let candidate = ReapCandidate {
            charge_id: &charge_id,
            hold_id: &hold_id,
            idem_key: idem,
            age_seconds: TTL,
        };

        let first = reap_orphaned_hold(&mint, &store, &candidate, TTL)
            .await
            .unwrap();
        assert_eq!(first, ReapOutcome::Reaped);
        assert_eq!(mint.balance(PAYER), 10_000);

        // A second reaper pass over the same hold must not double-release funds.
        let second = reap_orphaned_hold(&mint, &store, &candidate, TTL)
            .await
            .unwrap();
        assert_eq!(second, ReapOutcome::AlreadyReleased);
        assert_eq!(mint.balance(PAYER), 10_000);
        assert_eq!(
            store
                .usage_by_charge_id(charge_id.as_str())
                .unwrap()
                .outcome,
            UsageOutcome::Released
        );
    }

    #[test]
    fn classification_never_releases_a_captured_hold() {
        assert_eq!(classify_hold(HoldStatus::Open), ReapAction::Release);
        assert_eq!(
            classify_hold(HoldStatus::Released),
            ReapAction::ConvergeReleased
        );
        assert_eq!(
            classify_hold(HoldStatus::Captured),
            ReapAction::SettleCapture
        );
    }

    #[test]
    fn ttl_boundary_is_inclusive_of_the_ttl_second() {
        assert!(!past_ttl(0, TTL));
        assert!(!past_ttl(TTL - 1, TTL));
        assert!(past_ttl(TTL, TTL));
        assert!(past_ttl(TTL + 1, TTL));
    }
}
