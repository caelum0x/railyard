//! # Hold-reaper decision — converge panic-orphaned Mint holds after a TTL.
//!
//! [`HoldGuard`](crate::HoldGuard) can only *log* when a request path panics
//! between opening a Mint hold and driving [`settle`](crate::settle): async work
//! cannot run in `Drop`, so a panic (or a hard crash) leaves an **orphaned
//! hold** — reserve moved out of the payer's available balance with no capture
//! or release ever recorded. The out-of-band reaper is the `vend-reconcile`
//! binary; this module owns the single, unit-tested *decision* that binary
//! executes for every stale attached hold, so the money-safety rules live in one
//! audited place rather than being re-derived inline.
//!
//! ## Safety (this is money code)
//!
//! The reaper is deliberately conservative. Candidate selection already enforces
//! two conditions in SQL (see `VendStore::lease_attached_holds`):
//!
//! - **(a) past TTL** — only rows whose hold has lived past the reconcile TTL are
//!   leased. A hold younger than the TTL could still be legitimately settling on
//!   a live request path, so it is never selected.
//! - **(b) still pending** — only rows whose local projection is still `pending`
//!   (no terminal outcome recorded) are leased.
//!
//! This module enforces the remaining, non-SQL conditions from Mint's
//! AUTHORITATIVE hold status plus the fail-closed payout signal:
//!
//! - **(c) no successful capture** — a `Captured` hold is NEVER released; it is
//!   routed to capture restoration (or deferred in reap-only mode).
//! - **(d) fail closed on observed payout** — an `Open`/`Released` hold whose
//!   payout plan already moved any transfer leg is refused, never released,
//!   because releasing would contradict money that already left clearing.
//!
//! Releasing is safe-by-idempotency: the binary reuses the same
//! `{idem_key}:release` key the panic path would have used, so a real in-flight
//! settlement that also releases is deduped by Mint to one operation, and a
//! released hold can never subsequently be captured. Because the decision keys
//! off a fresh Mint re-read, the reaper never even *attempts* to release a hold
//! whose capture already succeeded.

use vend_core::error::VendError;
use vend_core::transport::HoldStatus;

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
/// Centralizes the "never release a captured hold" rule that
/// [`plan_attached_reap`] and, through it, the `vend-reconcile` binary share.
pub fn classify_hold(status: HoldStatus) -> ReapAction {
    match status {
        HoldStatus::Open => ReapAction::Release,
        HoldStatus::Released => ReapAction::ConvergeReleased,
        HoldStatus::Captured => ReapAction::SettleCapture,
    }
}

/// The concrete convergence step the reconcile worker must run for one stale
/// attached hold, once Mint's authoritative status and the payout signal are
/// known. Each variant maps to exactly one durable `VendStore` transition, so
/// the binary is a thin executor over this decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedReap {
    /// Mint `Open`, no payout leg observed: release the hold at Mint (idempotent
    /// `{idem}:release`) then converge the projection to `released`.
    Release,
    /// Mint `Released`, no payout leg observed: converge the projection to
    /// `released` only; no Mint call is needed.
    ConvergeReleased,
    /// Mint `Captured` in a full reconcile pass: restore the exact rated charge
    /// from the immutable payout plan and drive the payout legs.
    RestoreCapture,
    /// Mint `Captured` in `--reap-holds` mode: defer to the next full reconcile
    /// pass. The crash-expiring lease means deferring never loses the row.
    DeferCapture,
}

/// Decide the money-safe convergence step for one stale attached hold.
///
/// This is the SINGLE decision the `vend-reconcile` binary executes; keeping it
/// pure makes every money-safety branch unit-testable without a database or a
/// live Mint. It layers the fail-closed payout guard (condition (d)) and the
/// reap-only capture deferral on top of the shared [`classify_hold`] mapping.
///
/// - `status`: Mint's authoritative, freshly re-read [`HoldStatus`].
/// - `payout_started`: `true` iff any payout transfer leg was already observed.
/// - `reap_only`: `true` in `--reap-holds` mode, where a `Captured` hold is
///   deferred rather than restored (the reaper never moves money out of
///   clearing).
///
/// Fails closed (never releases) when an `Open` or `Released` hold already has an
/// observed payout leg — releasing would contradict money that already left
/// clearing.
pub fn plan_attached_reap(
    status: HoldStatus,
    payout_started: bool,
    reap_only: bool,
) -> Result<AttachedReap, VendError> {
    match classify_hold(status) {
        ReapAction::Release => {
            if payout_started {
                return Err(VendError::Storage(
                    "open hold has observed payout transfers; refusing release".into(),
                ));
            }
            Ok(AttachedReap::Release)
        }
        ReapAction::ConvergeReleased => {
            if payout_started {
                return Err(VendError::Storage(
                    "released hold has observed payout transfers; manual audit required".into(),
                ));
            }
            Ok(AttachedReap::ConvergeReleased)
        }
        ReapAction::SettleCapture => {
            if reap_only {
                Ok(AttachedReap::DeferCapture)
            } else {
                Ok(AttachedReap::RestoreCapture)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn open_hold_with_no_payout_is_released() {
        assert_eq!(
            plan_attached_reap(HoldStatus::Open, false, false).unwrap(),
            AttachedReap::Release
        );
        assert_eq!(
            plan_attached_reap(HoldStatus::Open, false, true).unwrap(),
            AttachedReap::Release
        );
    }

    #[test]
    fn released_hold_with_no_payout_converges() {
        assert_eq!(
            plan_attached_reap(HoldStatus::Released, false, false).unwrap(),
            AttachedReap::ConvergeReleased
        );
        assert_eq!(
            plan_attached_reap(HoldStatus::Released, false, true).unwrap(),
            AttachedReap::ConvergeReleased
        );
    }

    #[test]
    fn open_hold_with_observed_payout_fails_closed() {
        let err = plan_attached_reap(HoldStatus::Open, true, false).unwrap_err();
        assert!(matches!(err, VendError::Storage(_)));
        assert!(err.to_string().contains("refusing release"));
        // The fail-closed guard is independent of reap-only mode.
        assert!(plan_attached_reap(HoldStatus::Open, true, true).is_err());
    }

    #[test]
    fn released_hold_with_observed_payout_requires_manual_audit() {
        let err = plan_attached_reap(HoldStatus::Released, true, false).unwrap_err();
        assert!(matches!(err, VendError::Storage(_)));
        assert!(err.to_string().contains("manual audit"));
        assert!(plan_attached_reap(HoldStatus::Released, true, true).is_err());
    }

    #[test]
    fn captured_hold_is_restored_in_full_pass_and_deferred_when_reaping() {
        // A captured hold is NEVER released regardless of the payout signal.
        assert_eq!(
            plan_attached_reap(HoldStatus::Captured, false, false).unwrap(),
            AttachedReap::RestoreCapture
        );
        assert_eq!(
            plan_attached_reap(HoldStatus::Captured, true, false).unwrap(),
            AttachedReap::RestoreCapture
        );
        assert_eq!(
            plan_attached_reap(HoldStatus::Captured, false, true).unwrap(),
            AttachedReap::DeferCapture
        );
        assert_eq!(
            plan_attached_reap(HoldStatus::Captured, true, true).unwrap(),
            AttachedReap::DeferCapture
        );
    }
}
