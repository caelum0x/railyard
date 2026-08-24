//! The success-only billing engine over the append-only microdollar ledger.
//!
//! Lifecycle of one job (ADR-0002):
//! * [`Ledger::reserve`] — hold `estimate` (append `Reserve` delta `-estimate`).
//! * [`Ledger::settle`] — success: append `Release +estimate` AND `Debit
//!   -actual` in one atomic batch (net debit == actual, NOT the estimate),
//!   plus the matching `UsageRecord`. The ONLY path that emits a `Debit`.
//! * [`Ledger::release`] — failure: append `Release +estimate` only. Nets
//!   exactly zero. NO `Debit`, NO `UsageRecord`. A failed job costs nothing.
//!
//! Both settle and release are idempotent per job id (the request id): the
//! first terminal action wins and any repeat is a no-op. All arithmetic is
//! exact integer micros so reservations always reconcile to zero.

use std::sync::Arc;

use crate::domain::{AccountId, JobId, ModelId};
use crate::error::BillingError;
use crate::money::Micros;
use crate::store::{LedgerEntry, LedgerKind, ReserveOutcome, Store, UsageRecord};
use crate::usage::Usage;

/// A derived hold tracked per job between reserve and settle/release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub job_id: JobId,
    pub account_id: AccountId,
    pub estimate_micros: Micros,
}

/// The authoritative billed amount for a completed job is carried on the usage
/// block (computed upstream by normalization, exact integer micros).
pub fn cost_micros(usage: &Usage) -> Micros {
    usage.cost_micros
}

/// Per-job ledger snapshot used to enforce idempotency and locate the hold.
struct JobState {
    reserved: bool,
    /// True once any `Release` exists for the job — meaning it was already
    /// settled or released, so further settle/release calls are no-ops.
    terminal: bool,
    /// The `Reserve` entry's (negative) delta; its negation is the estimate.
    reserve_delta: Micros,
}

/// The billing service. Holds only an `Arc<dyn Store>` — it never persists
/// anything itself; all state lives in the injected store.
#[derive(Clone)]
pub struct Ledger {
    store: Arc<dyn Store>,
}

impl Ledger {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Available balance = exact integer sum of the account's ledger, which
    /// already nets any outstanding `Reserve` holds against deposits.
    pub async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError> {
        Ok(self.store.balance_micros(account_id).await?)
    }

    /// Reserve-on-submit: assert the account can cover `estimate`, then append a
    /// `Reserve` hold of `-estimate`. Fails with [`BillingError::InsufficientCredits`]
    /// (no hold appended) when the available balance is short.
    pub async fn reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        if estimate.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative reserve estimate {estimate:?}"
            )));
        }
        let hold = Micros::ZERO.checked_sub(estimate)?; // -estimate
        let entry = LedgerEntry::new(
            account_id.clone(),
            LedgerKind::Reserve,
            hold,
            "reserve",
            Some(job),
        );
        // Atomic compare-and-append: the credit gate and the hold land under a
        // single store lock, so two concurrent reserves can never both pass the
        // gate and drive the account negative (TOCTOU-safe).
        match self.store.try_reserve(account_id, estimate, entry).await? {
            ReserveOutcome::Insufficient { available } => Err(BillingError::InsufficientCredits {
                available,
                requested: estimate,
            }),
            ReserveOutcome::Reserved => Ok(Reservation {
                job_id: job,
                account_id: account_id.clone(),
                estimate_micros: estimate,
            }),
        }
    }

    /// Settle-actual-on-success (a.k.a. [`Ledger::debit_on_success`]). Appends
    /// `Release +estimate` AND `Debit -actual` in ONE atomic batch, so the net
    /// effect of the reservation is EXACTLY the actual usage — never the
    /// reserved estimate. Writes the matching `UsageRecord`. Idempotent: a
    /// repeat call on an already-terminal job is a no-op (never double-debits).
    pub async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError> {
        let state = self
            .job_state(&reservation.account_id, &reservation.job_id)
            .await?;
        if !state.reserved {
            return Err(BillingError::UnknownReservation(reservation.job_id));
        }
        if state.terminal {
            return Ok(()); // already settled or released — idempotent no-op.
        }

        let actual = usage.cost_micros;
        if actual.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative settle cost {actual:?}"
            )));
        }

        let estimate = state.reserve_delta.checked_neg()?; // +estimate
        let debit = Micros::ZERO.checked_sub(actual)?; // -actual

        let release = LedgerEntry::new(
            reservation.account_id.clone(),
            LedgerKind::Release,
            estimate,
            "settle_release",
            Some(reservation.job_id),
        );
        let debit_entry = LedgerEntry::new(
            reservation.account_id.clone(),
            LedgerKind::Debit,
            debit,
            // `reason` labels wallet_debit_micros{reason}: the billed model.
            model.as_str(),
            Some(reservation.job_id),
        );
        // Release + Debit land together so a settle is never half-applied.
        self.store.append_ledger(vec![release, debit_entry]).await?;

        // Invariant: every Debit has a matching completed UsageRecord.
        let record = UsageRecord::new(
            reservation.job_id,
            reservation.account_id.clone(),
            model,
            usage,
        );
        self.store.put_usage(record).await?;
        Ok(())
    }

    /// Alias for [`Ledger::settle`] — the success path that debits actual usage.
    pub async fn debit_on_success(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError> {
        self.settle(reservation, usage, model).await
    }

    /// Release-full-refund-on-failure: append `Release +estimate` ONLY. The
    /// reservation nets exactly zero — NO `Debit`, NO `UsageRecord`. Idempotent:
    /// a repeat (or a release after a settle) is a no-op, so a failed job always
    /// costs exactly nothing.
    pub async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let state = self
            .job_state(&reservation.account_id, &reservation.job_id)
            .await?;
        if !state.reserved {
            return Err(BillingError::UnknownReservation(reservation.job_id));
        }
        if state.terminal {
            return Ok(()); // already terminal — never emit a second Release/Debit.
        }
        let estimate = state.reserve_delta.checked_neg()?; // +estimate
        let entry = LedgerEntry::new(
            reservation.account_id.clone(),
            LedgerKind::Release,
            estimate,
            "release_refund",
            Some(reservation.job_id),
        );
        self.store.append_ledger(vec![entry]).await?;
        Ok(())
    }

    /// Fold this job's ledger entries into a `JobState` (reserve presence, the
    /// hold amount, and whether any terminal `Release` already exists).
    async fn job_state(
        &self,
        account_id: &AccountId,
        job: &JobId,
    ) -> Result<JobState, BillingError> {
        let entries = self.store.ledger_for_account(account_id).await?;
        let mut state = JobState {
            reserved: false,
            terminal: false,
            reserve_delta: Micros::ZERO,
        };
        for e in entries.iter().filter(|e| e.job_id.as_ref() == Some(job)) {
            match e.kind {
                LedgerKind::Reserve => {
                    state.reserved = true;
                    state.reserve_delta = e.delta_micros;
                }
                LedgerKind::Release => state.terminal = true,
                _ => {}
            }
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{InMemoryStore, LedgerStore, UsageStore};

    fn store() -> InMemoryStore {
        InMemoryStore::new()
    }

    fn ledger(s: &InMemoryStore) -> Ledger {
        let dynamic: Arc<dyn Store> = Arc::new(s.clone());
        Ledger::new(dynamic)
    }

    async fn fund(s: &InMemoryStore, account: &AccountId, micros: i64) {
        s.append_ledger(vec![LedgerEntry::new(
            account.clone(),
            LedgerKind::Deposit,
            Micros::new(micros),
            "seed",
            None,
        )])
        .await
        .unwrap();
    }

    async fn balance(s: &InMemoryStore, account: &AccountId) -> Micros {
        s.balance_micros(account).await.unwrap()
    }

    async fn count_kind(s: &InMemoryStore, account: &AccountId, kind: LedgerKind) -> usize {
        s.ledger_for_account(account)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .count()
    }

    fn usage(cost: i64) -> Usage {
        Usage::new(10, 20, 30, Micros::new(cost))
    }

    fn model() -> ModelId {
        ModelId::namespace("openai/gpt-4o")
    }

    // ---- reserve ---------------------------------------------------------

    #[tokio::test]
    async fn reserve_places_a_hold_and_reduces_available() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 10_000_000).await;
        let l = ledger(&s);

        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();
        assert_eq!(res.estimate_micros, Micros::new(3_000_000));
        assert_eq!(res.account_id, acct);

        assert_eq!(count_kind(&s, &acct, LedgerKind::Reserve).await, 1);
        assert_eq!(
            l.available_micros(&acct).await.unwrap(),
            Micros::new(7_000_000)
        );
        assert_eq!(balance(&s, &acct).await, Micros::new(7_000_000));
    }

    #[tokio::test]
    async fn reserve_rejects_when_estimate_exceeds_available_and_appends_nothing() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 1_000_000).await;
        let l = ledger(&s);

        let err = l
            .reserve(&acct, Micros::new(2_000_000), JobId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, BillingError::InsufficientCredits { .. }));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Reserve).await, 0);
        assert_eq!(balance(&s, &acct).await, Micros::new(1_000_000));
    }

    #[tokio::test]
    async fn reserve_allows_exact_balance_hold() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        assert!(l
            .reserve(&acct, Micros::new(5_000_000), JobId::new())
            .await
            .is_ok());
        assert_eq!(balance(&s, &acct).await, Micros::ZERO);
    }

    #[tokio::test]
    async fn concurrent_holds_cannot_overspend() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        l.reserve(&acct, Micros::new(3_000_000), JobId::new())
            .await
            .unwrap();
        let err = l
            .reserve(&acct, Micros::new(3_000_000), JobId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, BillingError::InsufficientCredits { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_reserves_cannot_both_overspend() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);

        let mut handles = Vec::new();
        for _ in 0..16 {
            let l = l.clone();
            let acct = acct.clone();
            handles.push(tokio::spawn(async move {
                l.reserve(&acct, Micros::new(3_000_000), JobId::new())
                    .await
                    .is_ok()
            }));
        }

        let mut successes = 0usize;
        for h in handles {
            if h.await.unwrap() {
                successes += 1;
            }
        }
        assert_eq!(
            successes, 1,
            "exactly one 3.0 hold may win against a 5.0 balance"
        );
        assert_eq!(balance(&s, &acct).await, Micros::new(2_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Reserve).await, 1);
    }

    #[tokio::test]
    async fn reserve_rejects_negative_estimate() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        let err = l
            .reserve(&acct, Micros::new(-1), JobId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, BillingError::InvalidAmount(_)));
    }

    // ---- settle (success) ------------------------------------------------

    #[tokio::test]
    async fn settle_debits_exactly_actual_not_the_estimate() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 10_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();

        l.settle(&res, usage(1_234_567), model()).await.unwrap();

        assert_eq!(balance(&s, &acct).await, Micros::new(8_765_433));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Release).await, 1);
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 1);
    }

    #[tokio::test]
    async fn settle_writes_matching_usage_record_for_every_debit() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 10_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();

        l.settle(&res, usage(500_000), model()).await.unwrap();

        let records = s.usage_for_job(&job).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.cost_micros, Micros::new(500_000));
        assert_eq!(records[0].model, model());
        assert_eq!(records[0].account_id, acct);
    }

    #[tokio::test]
    async fn settle_with_zero_cost_debits_zero_but_still_records_usage() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 4_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(2_000_000), job).await.unwrap();

        l.settle(&res, usage(0), model()).await.unwrap();
        assert_eq!(balance(&s, &acct).await, Micros::new(4_000_000));
        assert_eq!(s.usage_for_job(&job).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn debit_on_success_is_an_alias_for_settle() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 10_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();
        l.debit_on_success(&res, usage(1_000_000), model())
            .await
            .unwrap();
        assert_eq!(balance(&s, &acct).await, Micros::new(9_000_000));
    }

    // ---- release (failure) ----------------------------------------------

    #[tokio::test]
    async fn release_refunds_fully_and_never_debits() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 4_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(4_000_000), job).await.unwrap();

        l.release(&res).await.unwrap();

        assert_eq!(balance(&s, &acct).await, Micros::new(4_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 0);
        assert!(s.usage_for_job(&job).await.unwrap().is_empty());
    }

    // ---- idempotency (double-submit per request/job id) ------------------

    #[tokio::test]
    async fn double_settle_debits_only_once() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 10_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();

        l.settle(&res, usage(1_000_000), model()).await.unwrap();
        l.settle(&res, usage(1_000_000), model()).await.unwrap();

        assert_eq!(balance(&s, &acct).await, Micros::new(9_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 1);
        assert_eq!(s.usage_for_job(&job).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn double_release_nets_zero_once() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 4_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(4_000_000), job).await.unwrap();

        l.release(&res).await.unwrap();
        l.release(&res).await.unwrap(); // idempotent no-op
        assert_eq!(balance(&s, &acct).await, Micros::new(4_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Release).await, 1);
    }

    #[tokio::test]
    async fn settle_after_release_does_not_debit() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();

        l.release(&res).await.unwrap();
        l.settle(&res, usage(1_000_000), model()).await.unwrap();
        assert_eq!(balance(&s, &acct).await, Micros::new(5_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 0);
        assert!(s.usage_for_job(&job).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn release_after_settle_keeps_the_debit() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        let job = JobId::new();
        let res = l.reserve(&acct, Micros::new(3_000_000), job).await.unwrap();

        l.settle(&res, usage(1_000_000), model()).await.unwrap();
        l.release(&res).await.unwrap(); // no-op; the debit stands
        assert_eq!(balance(&s, &acct).await, Micros::new(4_000_000));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 1);
    }

    // ---- unknown reservation --------------------------------------------

    #[tokio::test]
    async fn settle_unknown_reservation_errors() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 5_000_000).await;
        let l = ledger(&s);
        let phantom = Reservation {
            job_id: JobId::new(),
            account_id: acct.clone(),
            estimate_micros: Micros::new(1_000_000),
        };
        let err = l.settle(&phantom, usage(1), model()).await.unwrap_err();
        assert!(matches!(err, BillingError::UnknownReservation(_)));
    }

    #[tokio::test]
    async fn release_unknown_reservation_errors() {
        let s = store();
        let acct = AccountId::new("a");
        let l = ledger(&s);
        let phantom = Reservation {
            job_id: JobId::new(),
            account_id: acct,
            estimate_micros: Micros::new(1_000_000),
        };
        let err = l.release(&phantom).await.unwrap_err();
        assert!(matches!(err, BillingError::UnknownReservation(_)));
    }

    // ---- exactness / reconciliation -------------------------------------

    #[tokio::test]
    async fn full_cycle_reconciles_to_exact_micros() {
        let s = store();
        let acct = AccountId::new("a");
        fund(&s, &acct, 20_000_000).await;
        let l = ledger(&s);

        let j1 = JobId::new();
        let r1 = l.reserve(&acct, Micros::new(5_000_000), j1).await.unwrap();
        l.settle(&r1, usage(1_111_111), model()).await.unwrap();

        let j2 = JobId::new();
        let r2 = l.reserve(&acct, Micros::new(5_000_000), j2).await.unwrap();
        l.settle(&r2, usage(2_222_222), model()).await.unwrap();

        let j3 = JobId::new();
        let r3 = l.reserve(&acct, Micros::new(5_000_000), j3).await.unwrap();
        l.release(&r3).await.unwrap(); // costs nothing

        assert_eq!(balance(&s, &acct).await, Micros::new(16_666_667));
        assert_eq!(count_kind(&s, &acct, LedgerKind::Debit).await, 2);
        assert_eq!(s.usage_for_job(&j1).await.unwrap().len(), 1);
        assert_eq!(s.usage_for_job(&j2).await.unwrap().len(), 1);
        assert!(s.usage_for_job(&j3).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cost_micros_reads_authoritative_amount() {
        assert_eq!(cost_micros(&usage(4242)), Micros::new(4242));
    }
}
