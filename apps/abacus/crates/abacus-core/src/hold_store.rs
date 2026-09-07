//! Pluggable hold-tracking PORT + an injectable [`Clock`] for the orphaned-hold
//! reaper.
//!
//! ## Why this seam exists
//!
//! Abacus opens a Mint reservation (a *hold*) as its reserve gate, forwards the
//! request, then captures the exact actual on success or releases on any other
//! terminal (see `abacus-proxy`'s `settle.rs`, ADR-0002). Today the
//! open → terminal window is tracked only by the in-process `HoldGuard` RAII
//! value. If the request task PANICS — or the whole process is killed — between
//! opening the hold and settling it, the guard's `Drop` merely *logs*: the
//! reserved budget stays locked on Mint indefinitely. That is an **orphaned
//! hold**, and at horizontal scale it silently starves an agent's ceiling.
//!
//! [`HoldStore`] is the durable side-table that records every open hold's
//! `hold_id` + `idem_key` + reserved amount + age, so an out-of-band reaper can
//! find the orphans and release their budget. It is a PORT, exactly like
//! [`crate::receipt::ReceiptStore`], [`crate::identity::BudgetRepo`] and
//! [`crate::transport::MintTransport`]: THIS crate declares the trait; an
//! in-memory implementation (see `abacus-proxy::mem_hold_store`) backs a single
//! instance today.
//!
//! ## The future shared datastore (why the trait, not a concrete type)
//!
//! Abacus, turnstile and vend each keep holds/nonces/receipts in-process. When
//! any of them scales past one instance, a hold opened on instance A must be
//! reapable by instance B, which requires a *shared, transactional* store. Because
//! callers depend on the [`HoldStore`] trait (never a concrete map), that shared
//! backend — a Mint-side `holds` table, Neon, or Redis with a compare-and-remove
//! primitive — can be swapped in behind the SAME four methods with ZERO change to
//! the reaper or the request hot path. `create` is defined to be idempotent on an
//! identical record so a transactional backend can implement it as an upsert, and
//! `remove` returns the removed row so a shared reaper can claim-then-release
//! exactly once. That swappability across the three services is the entire point
//! of this seam; this commit scopes the concrete work to abacus.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};

use crate::error::AbacusError;
use crate::ids::{AgentId, BudgetId, RunId};
use crate::receipt::{MintReceipt, ReceiptStore};

/// Stable request/principal context required to turn a recovered Mint release
/// into the same signed receipt projection as the normal terminal path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldReceiptContext {
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub budget_id: BudgetId,
    pub payer_account: String,
    pub request_id: String,
}

/// A wall-clock source, injected so the reaper is deterministic and testable
/// WITHOUT real time. Production wires [`SystemClock`]; tests wire
/// [`ManualClock`] and advance it explicitly.
pub trait Clock: Send + Sync {
    /// Current time, in unix milliseconds.
    fn now_ms(&self) -> i64;
}

/// The real, monotonically-advancing system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Pre-epoch clocks (or a backwards step) collapse to 0 rather than panic;
        // a 0 "now" only makes the reaper *more* conservative (nothing looks old).
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }
}

/// A test/simulation clock whose time is set explicitly. `Send + Sync` via an
/// atomic so it can be shared behind `&dyn Clock` across tasks.
pub struct ManualClock {
    now_ms: AtomicI64,
}

impl ManualClock {
    /// Start the clock at `now_ms`.
    pub fn new(now_ms: i64) -> Self {
        ManualClock {
            now_ms: AtomicI64::new(now_ms),
        }
    }

    /// Jump the clock to an absolute `now_ms`.
    pub fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Advance the clock by `delta_ms` (may be negative to simulate skew).
    pub fn advance(&self, delta_ms: i64) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// The tracked metadata for one open Mint hold — the minimum the reaper needs to
/// recognise an orphan and release it.
///
/// A row is inserted when a reservation opens (right after [`crate::transport::MintTransport::create_hold`]
/// / `spend_allowance` succeeds) and removed when the request settles. Any row
/// that outlives its TTL / `deadline_ms` without being removed is, by definition,
/// orphaned by a panicked or killed caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRecord {
    /// The Mint hold id (`hold_…`) this row tracks.
    pub hold_id: String,
    /// The deterministic idempotency base reused across the hold triple; the
    /// reaper passes it to `release_hold` so the release is itself idempotent.
    pub idem_key: String,
    /// Amount reserved on the hold (`est_max`), in µ$ — the budget the reaper
    /// returns to the agent when it releases an orphan.
    pub reserved_micros: i64,
    /// When the hold was opened, in unix milliseconds ([`Clock::now_ms`]).
    pub created_at_ms: i64,
    /// An explicit expiry instant, in unix milliseconds. When `Some`, it OVERRIDES
    /// the TTL: the hold is orphaned once `now >= deadline_ms` regardless of TTL.
    /// Use it for requests with a known upstream timeout budget.
    pub deadline_ms: Option<i64>,
    /// Receipt identity captured before the upstream call. `None` is accepted
    /// only for legacy snapshots created before receipted recovery existed.
    #[serde(default)]
    pub receipt_context: Option<HoldReceiptContext>,
}

impl HoldRecord {
    /// `true` iff this hold is past its deadline, or older than `ttl_ms`.
    ///
    /// A non-positive `ttl_ms` with no deadline expires everything immediately —
    /// callers should pass a sane positive TTL (comfortably larger than the
    /// longest legitimate request) so live holds are never reaped mid-flight.
    pub fn is_expired(&self, now_ms: i64, ttl_ms: i64) -> bool {
        if let Some(deadline) = self.deadline_ms {
            return now_ms >= deadline;
        }
        now_ms.saturating_sub(self.created_at_ms) >= ttl_ms
    }
}

/// The pluggable hold-tracking port. Object-safe (async_trait boxes the futures)
/// so it can be held as `Arc<dyn HoldStore>` in server state, and swapped from the
/// in-memory single-instance impl to a shared transactional datastore with no
/// caller changes.
#[async_trait]
pub trait HoldStore: Send + Sync {
    /// Record a freshly-opened hold. Idempotent on an IDENTICAL record (a retried
    /// registration is a no-op); a differing record for an existing `hold_id` is a
    /// conflict ([`AbacusError::IdempotencyConflict`]).
    async fn create(&self, record: HoldRecord) -> Result<(), AbacusError>;

    /// Fetch a tracked hold by id, if present.
    async fn get(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError>;

    /// Snapshot every currently-tracked hold. Implementations SHOULD return a
    /// stable order so a reap sweep is deterministic.
    async fn list(&self) -> Result<Vec<HoldRecord>, AbacusError>;

    /// Atomically lease expired holds to one recovery worker. A row with an
    /// unexpired lease is invisible to other workers; an expired lease is
    /// reclaimable after a crashed worker.
    async fn claim_expired(
        &self,
        worker_id: &str,
        now_ms: i64,
        ttl_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<HoldRecord>, AbacusError>;

    /// Delete a row only when it is still leased by `worker_id`.
    async fn complete_claim(
        &self,
        hold_id: &str,
        worker_id: &str,
    ) -> Result<Option<HoldRecord>, AbacusError>;

    /// Persist a recovered terminal receipt and delete the owned claim as one
    /// shared-store transaction where supported. Single-replica stores use the
    /// safe default ordering: receipt first, then claim deletion.
    async fn complete_claim_with_receipt(
        &self,
        hold_id: &str,
        worker_id: &str,
        receipt: &MintReceipt,
        receipts: &dyn ReceiptStore,
    ) -> Result<Option<HoldRecord>, AbacusError> {
        let removed = self.complete_claim(hold_id, worker_id).await?;
        if removed.is_some() {
            receipts.record(receipt).await?;
        }
        Ok(removed)
    }

    /// Remove a tracked hold, returning the row that was removed (or `None` if it
    /// was already gone). A shared backend implements this as an atomic
    /// claim-and-delete so exactly one reaper releases each orphan.
    async fn remove(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hold_id: &str, created_at_ms: i64, deadline_ms: Option<i64>) -> HoldRecord {
        HoldRecord {
            hold_id: hold_id.to_string(),
            idem_key: format!("idem_{hold_id}"),
            reserved_micros: 1_000,
            created_at_ms,
            deadline_ms,
            receipt_context: None,
        }
    }

    #[test]
    fn ttl_expiry_is_inclusive_of_the_boundary() {
        let r = rec("hold_a", 1_000, None);
        // exactly TTL old -> expired.
        assert!(r.is_expired(1_000 + 30_000, 30_000));
        // one ms short of TTL -> live.
        assert!(!r.is_expired(1_000 + 29_999, 30_000));
    }

    #[test]
    fn deadline_overrides_ttl() {
        let r = rec("hold_b", 1_000, Some(2_000));
        // Well within TTL, but past the explicit deadline -> expired.
        assert!(r.is_expired(2_500, 1_000_000));
        // Before the deadline -> live, even with a tiny TTL that would otherwise fire.
        assert!(!r.is_expired(1_999, 1));
    }

    #[test]
    fn manual_clock_advances() {
        let clock = ManualClock::new(100);
        assert_eq!(clock.now_ms(), 100);
        clock.advance(50);
        assert_eq!(clock.now_ms(), 150);
        clock.set(10);
        assert_eq!(clock.now_ms(), 10);
    }
}
