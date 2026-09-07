//! [`reap_expired`] — the orphaned-hold reaper.
//!
//! `settle.rs` (ADR-0002) drives every hold to a terminal explicitly; a
//! [`crate::settle::HoldGuard`] dropped still-armed means the request task
//! PANICKED (or the process was killed) before settling, leaving the reserved
//! budget locked on Mint. The guard can only *log* that — it cannot run async
//! release in `Drop`. This reaper closes that gap out of band: it sweeps the
//! [`HoldStore`], and for every hold past its TTL / deadline it calls
//! `release_hold` (returning the reserved budget to the agent) and drops the
//! tracking row.
//!
//! Determinism: time enters ONLY through the injected [`Clock`], so tests advance
//! a [`abacus_core::hold_store::ManualClock`] instead of sleeping.
//!
//! Production registers each hold before forwarding, removes it after terminal
//! settlement, and runs this sweep immediately at startup and on an interval.

use abacus_core::error::AbacusError;
use abacus_core::hold_store::{Clock, HoldStore};
use abacus_core::receipt::{MintReceipt, ReceiptOutcome, ReceiptStore};
use abacus_core::transport::{MintTransport, Release};

/// The outcome of one reap sweep — enough to log/meter what the reaper did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReapSummary {
    /// Hold ids that were expired, released on Mint, and dropped from the store.
    pub reaped_hold_ids: Vec<String>,
    /// Total reserved budget released back to agents this sweep, in µ$.
    pub released_micros: i64,
    /// Live (non-expired) holds left untouched.
    pub survived: usize,
    /// Expired rows whose hold was ALREADY terminal on Mint (`hold_not_found`);
    /// nothing to release, tracking row dropped. Keeps the reaper self-healing.
    pub already_terminal: usize,
}

impl ReapSummary {
    /// How many orphans were actively released this sweep.
    pub fn reaped_count(&self) -> usize {
        self.reaped_hold_ids.len()
    }
}

/// Sweep `store` once, releasing every hold expired as of `clock.now_ms()` under
/// `ttl_ms` (or its explicit deadline), and return a [`ReapSummary`].
///
/// Invariants this upholds:
/// - **Orphan → released:** an expired hold's `release_hold` is invoked (returning
///   `reserved_micros` to the agent) and its row removed.
/// - **Live → survives:** a non-expired hold is never touched.
/// - **Idempotent:** because a released row is removed, a second sweep re-scans a
///   smaller store and re-does nothing; and each release reuses the hold's
///   `idem_key`, so even a racing double-release is a Mint no-op.
///
/// Error policy: a hold already terminal on Mint (`HoldNotFound`) is treated as
/// success (drop the stale row). Any OTHER error is transient (e.g. Mint
/// unreachable); the sweep aborts and leaves the remaining rows in place so the
/// next sweep retries them — budget is never lost, only released late.
pub async fn reap_expired(
    store: &dyn HoldStore,
    mint: &dyn MintTransport,
    receipts: &dyn ReceiptStore,
    clock: &dyn Clock,
    ttl_ms: i64,
) -> Result<ReapSummary, AbacusError> {
    let now_ms = clock.now_ms();
    reap_expired_claimed(
        store,
        mint,
        receipts,
        clock,
        ttl_ms,
        "single-replica",
        now_ms.saturating_add(ttl_ms.max(1)),
        usize::MAX,
    )
    .await
}

/// Replica-safe sweep using an explicit worker lease. The worker owns each
/// returned row until `lease_until_ms`; a crash leaves it reclaimable later.
pub async fn reap_expired_claimed(
    store: &dyn HoldStore,
    mint: &dyn MintTransport,
    receipts: &dyn ReceiptStore,
    clock: &dyn Clock,
    ttl_ms: i64,
    worker_id: &str,
    lease_until_ms: i64,
    limit: usize,
) -> Result<ReapSummary, AbacusError> {
    let now_ms = clock.now_ms();
    let mut summary = ReapSummary::default();
    summary.survived = store
        .list()
        .await?
        .into_iter()
        .filter(|record| !record.is_expired(now_ms, ttl_ms))
        .count();

    for record in store
        .claim_expired(worker_id, now_ms, ttl_ms, lease_until_ms, limit)
        .await?
    {
        match mint
            .release_hold(
                &record.hold_id,
                Release {
                    idem_key: record.idem_key.clone(),
                },
            )
            .await
        {
            Ok(release) => {
                // Release succeeded (or was an idempotent no-op on Mint). Drop the
                // tracking row so the next sweep does not revisit it.
                if let Some(context) = &record.receipt_context {
                    let receipt = MintReceipt {
                        agent_id: context.agent_id.clone(),
                        run_id: context.run_id.clone(),
                        budget_id: context.budget_id.clone(),
                        payer_account: context.payer_account.clone(),
                        request_id: context.request_id.clone(),
                        hold_id: record.hold_id.clone(),
                        reserved_micros: record.reserved_micros,
                        captured_micros: 0,
                        balance_after_micros: 0,
                        settlement_seq: release.proof.sequence,
                        outcome: ReceiptOutcome::Released,
                        settlement_proof: release.proof,
                    };
                    store
                        .complete_claim_with_receipt(&record.hold_id, worker_id, &receipt, receipts)
                        .await?;
                } else {
                    store.complete_claim(&record.hold_id, worker_id).await?;
                }
                summary.released_micros = summary
                    .released_micros
                    .saturating_add(record.reserved_micros);
                summary.reaped_hold_ids.push(record.hold_id);
            }
            Err(AbacusError::HoldNotFound) => {
                // The hold was already captured/released (e.g. the handler settled
                // it after we snapshotted the list). Nothing to return; just clear
                // the stale tracking row.
                store.complete_claim(&record.hold_id, worker_id).await?;
                summary.already_terminal += 1;
            }
            Err(other) => return Err(other),
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    use abacus_core::hold_store::{HoldReceiptContext, HoldRecord, ManualClock};
    use abacus_core::ids::{AgentId, BudgetId, RunId};
    use abacus_core::receipt::{ReceiptOutcome, ReceiptStore};
    use abacus_core::transport::Hold;
    use abacus_testkit::fake_mint::LedgerFakeMint;
    use abacus_testkit::FakeReceiptStore;

    use crate::mem_hold_store::InMemoryHoldStore;

    const TTL_MS: i64 = 30_000;
    const TREASURY: &str = "acct_treasury";

    /// Open a real hold on the fake Mint AND register it in the store, returning
    /// the record so tests can assert on it. Mirrors the future enforce-time wiring.
    async fn open_and_track(
        mint: &LedgerFakeMint,
        store: &InMemoryHoldStore,
        idem_key: &str,
        amount: i64,
        created_at_ms: i64,
        deadline_ms: Option<i64>,
    ) -> HoldRecord {
        let receipt = mint
            .create_hold(Hold {
                account_id: TREASURY.to_string(),
                amount_micros: amount,
                idem_key: idem_key.to_string(),
            })
            .await
            .expect("hold opens");
        let record = HoldRecord {
            hold_id: receipt.hold_id,
            idem_key: idem_key.to_string(),
            reserved_micros: amount,
            created_at_ms,
            deadline_ms,
            receipt_context: Some(HoldReceiptContext {
                agent_id: AgentId::generate(),
                run_id: RunId::generate(),
                budget_id: BudgetId::generate(),
                payer_account: TREASURY.to_string(),
                request_id: idem_key.to_string(),
            }),
        };
        store.create(record.clone()).await.expect("track hold");
        record
    }

    #[tokio::test]
    async fn expired_hold_is_reaped_and_budget_released() {
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 10_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(1_000);

        let orphan = open_and_track(&mint, &store, "req_orphan", 4_000, 1_000, None).await;
        // The reserve locked 4_000 of the 10_000 balance.
        assert_eq!(mint.balance(TREASURY), 6_000);

        // Time moves past the TTL: the caller panicked and never settled.
        clock.advance(TTL_MS + 1);
        let receipts = FakeReceiptStore::new();
        let summary = reap_expired(&store, &mint, &receipts, &clock, TTL_MS)
            .await
            .unwrap();

        assert_eq!(summary.reaped_hold_ids, vec![orphan.hold_id.clone()]);
        assert_eq!(summary.released_micros, 4_000);
        assert_eq!(summary.already_terminal, 0);
        let receipt = receipts.get(&orphan.hold_id).await.unwrap().unwrap();
        assert_eq!(receipt.request_id, "req_orphan");
        assert_eq!(receipt.outcome, ReceiptOutcome::Released);
        assert_eq!(receipt.captured_micros, 0);
        assert_eq!(receipt.settlement_seq, receipt.settlement_proof.sequence);
        assert!(!receipt.settlement_proof.signature.is_empty());
        // Budget is fully returned to the agent, and the row is gone.
        assert_eq!(mint.balance(TREASURY), 10_000);
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn live_hold_survives_the_sweep() {
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 10_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(5_000);

        open_and_track(&mint, &store, "req_live", 3_000, 5_000, None).await;
        assert_eq!(mint.balance(TREASURY), 7_000);

        // Only a little time passes — well within TTL.
        clock.advance(TTL_MS - 1);
        let summary = reap_expired(&store, &mint, &FakeReceiptStore::new(), &clock, TTL_MS)
            .await
            .unwrap();

        assert!(summary.reaped_hold_ids.is_empty());
        assert_eq!(summary.survived, 1);
        // Nothing released; the hold and its reservation stand.
        assert_eq!(mint.balance(TREASURY), 7_000);
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn reaping_is_idempotent() {
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 10_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(1_000);

        open_and_track(&mint, &store, "req_orphan", 4_000, 1_000, None).await;
        clock.advance(TTL_MS + 1);

        let receipts = FakeReceiptStore::new();
        let first = reap_expired(&store, &mint, &receipts, &clock, TTL_MS)
            .await
            .unwrap();
        assert_eq!(first.reaped_count(), 1);
        assert_eq!(first.released_micros, 4_000);
        assert_eq!(mint.balance(TREASURY), 10_000);

        // A second sweep must be a total no-op: budget unchanged, nothing reaped.
        let second = reap_expired(&store, &mint, &receipts, &clock, TTL_MS)
            .await
            .unwrap();
        assert_eq!(second, ReapSummary::default());
        assert_eq!(mint.balance(TREASURY), 10_000);
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn mixed_holds_reap_orphans_and_keep_live() {
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 20_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(10_000);

        // Two orphans (created long ago) and one fresh live hold. At the sweep
        // instant (now = 50_000, TTL = 30_000) the expiry cutoff is 20_000, so the
        // two olds (created <= 1_000) are orphaned while `fresh` (created 40_000)
        // is still comfortably live.
        let a = open_and_track(&mint, &store, "old_a", 2_000, 0, None).await;
        let b = open_and_track(&mint, &store, "old_b", 3_000, 1_000, None).await;
        let _live = open_and_track(&mint, &store, "fresh", 5_000, 40_000, None).await;
        assert_eq!(mint.balance(TREASURY), 10_000); // 20k - 2k - 3k - 5k

        clock.set(50_000);
        let summary = reap_expired(&store, &mint, &FakeReceiptStore::new(), &clock, TTL_MS)
            .await
            .unwrap();

        let mut reaped = summary.reaped_hold_ids.clone();
        reaped.sort();
        let mut expected = vec![a.hold_id.clone(), b.hold_id.clone()];
        expected.sort();
        assert_eq!(reaped, expected);
        assert_eq!(summary.released_micros, 5_000); // 2k + 3k
        assert_eq!(summary.survived, 1);
        // The two orphans' budget is back; the live hold's 5_000 is still locked.
        assert_eq!(mint.balance(TREASURY), 15_000);
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn already_terminal_hold_is_swept_without_error() {
        // Track a hold id the Mint has never seen -> release_hold => HoldNotFound.
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 10_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(1_000);

        store
            .create(HoldRecord {
                hold_id: "hold_ghost".to_string(),
                idem_key: "req_ghost".to_string(),
                reserved_micros: 1_000,
                created_at_ms: 1_000,
                deadline_ms: None,
                receipt_context: None,
            })
            .await
            .unwrap();

        clock.advance(TTL_MS + 1);
        let summary = reap_expired(&store, &mint, &FakeReceiptStore::new(), &clock, TTL_MS)
            .await
            .unwrap();

        assert!(summary.reaped_hold_ids.is_empty());
        assert_eq!(summary.released_micros, 0);
        assert_eq!(summary.already_terminal, 1);
        // The stale tracking row is cleared so it is not revisited.
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn deadline_expired_hold_is_reaped_before_ttl() {
        let mint = LedgerFakeMint::new().with_balance(TREASURY, 10_000);
        let store = InMemoryHoldStore::new();
        let clock = ManualClock::new(1_000);

        // Created just now, but with an explicit deadline 500ms out and a huge TTL.
        let rec = open_and_track(&mint, &store, "req_dl", 4_000, 1_000, Some(1_500)).await;
        clock.set(1_600); // past the deadline, nowhere near the TTL.

        let summary = reap_expired(&store, &mint, &FakeReceiptStore::new(), &clock, 1_000_000)
            .await
            .unwrap();
        assert_eq!(summary.reaped_hold_ids, vec![rec.hold_id]);
        assert_eq!(mint.balance(TREASURY), 10_000);
    }
}
