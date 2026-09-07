//! Durable-ish capture reconciliation for the direct-Mint fixed-price path.
//!
//! The fixed-price [`PaidCompletionModel`](crate::paid) serves inference and
//! then `capture`s the operator-owned price. If that `capture` fails *after the
//! provider has already served the request*, the money is owed but unsettled.
//! Relying on a single fire-and-forget retry in a `Drop` impl silently loses
//! that revenue whenever the retry also fails.
//!
//! This module mirrors the pending-payment tracking that
//! [`relay_provider::paid_transport`] uses for its evidence lookups: a shared,
//! in-process pending-capture record plus a bounded reconcile loop that retries
//! until Mint confirms the capture or the record is explicitly **dead-lettered**
//! (retained + logged, never silently dropped).
//!
//! ## Honest durability boundary
//!
//! This queue is **in-process only** — an [`Arc<Mutex<..>>`], exactly like the
//! Relay path's `EvidenceState`. It survives dropped streams, cancelled tasks,
//! and hosts without a Tokio runtime (via [`CaptureReconciler::reconcile_now`]),
//! but it does **not** survive a process crash. Wiring a crash-durable store
//! (a DB row / WAL) would replace the [`Mutex`] state behind this same API; the
//! seam is deliberately small so that swap is local. Nothing here fabricates
//! cross-restart durability.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aitp_client::{AmountMicros, HoldRef, PayError, PaymentGateway};

/// Tunables for the [`CaptureReconciler`] background retry loop.
#[derive(Debug, Clone)]
pub struct ReconcilerConfig {
    /// Maximum capture attempts by the background loop before a record is
    /// dead-lettered. Must be at least 1.
    pub max_attempts: usize,
    /// Base backoff between *failed* background attempts (the first retry runs
    /// immediately, so a flaky capture settles without waiting).
    pub base_delay: Duration,
    /// Upper bound on the exponential backoff.
    pub max_delay: Duration,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl ReconcilerConfig {
    fn backoff(&self, attempt: usize) -> Duration {
        // attempt is 1-based; grow 2^(attempt-1), capped at max_delay.
        let shift = attempt.saturating_sub(1).min(32) as u32;
        let scaled = self
            .base_delay
            .checked_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
            .unwrap_or(self.max_delay);
        scaled.min(self.max_delay)
    }
}

/// A capture the provider already served but Mint has not yet confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCapture {
    pub hold: HoldRef,
    pub amount: AmountMicros,
    /// Failed attempts so far (0 means queued, not yet retried by this queue).
    pub attempts: usize,
}

/// A capture abandoned after exhausting retries. Money is *owed* — this record
/// is retained and logged so an operator can settle it out of band. It is never
/// silently discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetteredCapture {
    pub hold: HoldRef,
    pub amount: AmountMicros,
    pub attempts: usize,
    pub last_error: String,
}

struct PendingRecord {
    hold: HoldRef,
    amount: AmountMicros,
    attempts: usize,
    /// True while a loop (background or explicit) owns this record, so we never
    /// run two captures for the same hold concurrently.
    reconciling: bool,
}

#[derive(Default)]
struct ReconcilerState {
    pending: HashMap<String, PendingRecord>,
    dead_letters: Vec<DeadLetteredCapture>,
}

/// Shared, in-process queue of unsettled captures with a bounded retry loop.
///
/// `P` is left unbounded at the struct level so callers can store an
/// `Arc<CaptureReconciler<P>>` without threading a `PaymentGateway` bound
/// through their own generics; the bound lives on the impl.
pub struct CaptureReconciler<P> {
    gateway: Arc<P>,
    config: ReconcilerConfig,
    state: Mutex<ReconcilerState>,
}

impl<P> CaptureReconciler<P>
where
    P: PaymentGateway + 'static,
{
    /// A reconciler over `gateway` with the given retry policy.
    pub fn new(gateway: Arc<P>, config: ReconcilerConfig) -> Self {
        Self {
            gateway,
            config,
            state: Mutex::new(ReconcilerState::default()),
        }
    }

    /// A reconciler with [`ReconcilerConfig::default`].
    pub fn with_defaults(gateway: Arc<P>) -> Self {
        Self::new(gateway, ReconcilerConfig::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReconcilerState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record an unsettled capture and, if a Tokio runtime is present, start a
    /// bounded background loop that retries it until Mint confirms or it is
    /// dead-lettered. Idempotent per hold: a second enqueue for a hold already
    /// being reconciled is a no-op, so the drop-guard safety net and the
    /// explicit failure path can both call this without double-charging.
    ///
    /// Without a Tokio runtime the record is retained for a later
    /// [`reconcile_now`](Self::reconcile_now).
    pub fn enqueue(self: &Arc<Self>, hold: HoldRef, amount: AmountMicros) {
        let key = hold.0.clone();
        {
            let mut state = self.lock();
            state.pending.entry(key.clone()).or_insert(PendingRecord {
                hold,
                amount,
                attempts: 0,
                reconciling: false,
            });
        }
        self.spawn_background(key);
    }

    fn spawn_background(self: &Arc<Self>, key: String) {
        // Claim the record; if another loop already owns it, do nothing.
        let claimed = {
            let mut state = self.lock();
            match state.pending.get_mut(&key) {
                Some(record) if !record.reconciling => {
                    record.reconciling = true;
                    Some((record.hold.clone(), record.amount))
                }
                _ => None,
            }
        };
        let Some((hold, amount)) = claimed else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No async host: release the claim so reconcile_now() can take it.
            self.mark_retryable(&key);
            tracing::warn!(
                hold = ?hold,
                amount = ?amount,
                "AITP-402 capture queued for reconciliation but no Tokio runtime is present; \
                 call CaptureReconciler::reconcile_now() to settle"
            );
            return;
        };
        let this = self.clone();
        runtime.spawn(async move {
            this.reconcile_loop(key, hold, amount).await;
        });
    }

    async fn reconcile_loop(&self, key: String, hold: HoldRef, amount: AmountMicros) {
        let mut attempt = 0usize;
        loop {
            match self.gateway.capture(&hold, amount).await {
                Ok(_) => {
                    self.mark_complete(&key);
                    tracing::info!(
                        hold = ?hold,
                        amount = ?amount,
                        attempts = attempt,
                        "AITP-402 capture reconciled after inference was served"
                    );
                    return;
                }
                Err(error) => {
                    attempt += 1;
                    self.bump_attempt(&key, attempt);
                    if attempt >= self.config.max_attempts {
                        self.dead_letter(&key, error.to_string());
                        return;
                    }
                    tokio::time::sleep(self.config.backoff(attempt)).await;
                }
            }
        }
    }

    /// Explicitly retry every currently unclaimed pending capture exactly once.
    ///
    /// This is the deterministic recovery hook for shutdown and non-Tokio hosts;
    /// background loops (when a runtime exists) use the same primitives. Returns
    /// the number of captures settled by this pass, or the first capture error
    /// encountered (records that still failed remain pending or are
    /// dead-lettered once they hit `max_attempts`).
    pub async fn reconcile_now(&self) -> Result<usize, PayError> {
        let claimed = {
            let mut state = self.lock();
            state
                .pending
                .iter_mut()
                .filter(|(_, record)| !record.reconciling)
                .map(|(key, record)| {
                    record.reconciling = true;
                    (key.clone(), record.hold.clone(), record.amount, record.attempts)
                })
                .collect::<Vec<_>>()
        };

        let mut settled = 0usize;
        let mut first_error = None;
        for (key, hold, amount, prior_attempts) in claimed {
            match self.gateway.capture(&hold, amount).await {
                Ok(_) => {
                    self.mark_complete(&key);
                    settled += 1;
                }
                Err(error) => {
                    let attempt = prior_attempts + 1;
                    self.bump_attempt(&key, attempt);
                    if attempt >= self.config.max_attempts {
                        self.dead_letter(&key, error.to_string());
                    } else {
                        self.mark_retryable(&key);
                    }
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(settled),
        }
    }

    fn mark_complete(&self, key: &str) {
        self.lock().pending.remove(key);
    }

    fn mark_retryable(&self, key: &str) {
        if let Some(record) = self.lock().pending.get_mut(key) {
            record.reconciling = false;
        }
    }

    fn bump_attempt(&self, key: &str, attempt: usize) {
        if let Some(record) = self.lock().pending.get_mut(key) {
            record.attempts = attempt;
        }
    }

    fn dead_letter(&self, key: &str, last_error: String) {
        let mut state = self.lock();
        if let Some(record) = state.pending.remove(key) {
            tracing::error!(
                hold = ?record.hold,
                amount = ?record.amount,
                attempts = record.attempts,
                error = %last_error,
                "AITP-402 capture dead-lettered after exhausting retries; \
                 revenue owed and retained for out-of-band settlement"
            );
            state.dead_letters.push(DeadLetteredCapture {
                hold: record.hold,
                amount: record.amount,
                attempts: record.attempts,
                last_error,
            });
        }
    }

    /// Snapshot of captures still awaiting confirmation.
    pub fn pending(&self) -> Vec<PendingCapture> {
        let state = self.lock();
        let mut out = state
            .pending
            .values()
            .map(|record| PendingCapture {
                hold: record.hold.clone(),
                amount: record.amount,
                attempts: record.attempts,
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.hold.0.cmp(&b.hold.0));
        out
    }

    /// Number of captures still awaiting confirmation.
    pub fn pending_len(&self) -> usize {
        self.lock().pending.len()
    }

    /// Snapshot of captures abandoned after exhausting retries. Non-empty means
    /// revenue is owed and must be settled out of band.
    pub fn dead_letters(&self) -> Vec<DeadLetteredCapture> {
        self.lock().dead_letters.clone()
    }

    /// Number of dead-lettered captures.
    pub fn dead_letter_len(&self) -> usize {
        self.lock().dead_letters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aitp_client::{Authorized, PaymentChallenge, Receipt, StubPaymentGateway};

    /// A gateway whose `capture` fails its first `fail_first` calls, then
    /// succeeds. `release`/`authorize` delegate to a real stub.
    struct FlakyCaptureGateway {
        inner: StubPaymentGateway,
        fail_first: usize,
        capture_calls: AtomicUsize,
    }

    impl FlakyCaptureGateway {
        fn new(fail_first: usize) -> Self {
            Self {
                inner: StubPaymentGateway::default(),
                fail_first,
                capture_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl PaymentGateway for FlakyCaptureGateway {
        async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError> {
            self.inner.authorize(challenge).await
        }
        async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError> {
            let n = self.capture_calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err(PayError::Gateway(format!("capture attempt {n} lost")));
            }
            self.inner.capture(hold, actual).await
        }
        async fn release(&self, hold: &HoldRef) -> Result<(), PayError> {
            self.inner.release(hold).await
        }
    }

    fn hold() -> HoldRef {
        HoldRef("hold_test_1".into())
    }

    #[tokio::test]
    async fn background_loop_retries_until_capture_settles() {
        let gateway = Arc::new(FlakyCaptureGateway::new(2));
        let reconciler = Arc::new(CaptureReconciler::new(
            gateway.clone(),
            ReconcilerConfig {
                max_attempts: 8,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
            },
        ));

        reconciler.enqueue(hold(), AmountMicros(4200));

        for _ in 0..2_000 {
            if reconciler.pending_len() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        assert_eq!(reconciler.pending_len(), 0, "capture must eventually settle");
        assert_eq!(reconciler.dead_letter_len(), 0, "settled capture is not dead-lettered");
        // 2 failures + 1 success == 3 capture calls; the money was captured.
        assert_eq!(gateway.capture_calls.load(Ordering::SeqCst), 3);
        gateway.inner.with_calls(|c| assert_eq!(c.captured.len(), 1));
    }

    #[tokio::test]
    async fn exhausted_retries_dead_letter_instead_of_losing_money() {
        // Always fails: capture can never settle.
        let gateway = Arc::new(FlakyCaptureGateway::new(usize::MAX));
        let reconciler = Arc::new(CaptureReconciler::new(
            gateway.clone(),
            ReconcilerConfig {
                max_attempts: 3,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
            },
        ));

        reconciler.enqueue(hold(), AmountMicros(4200));

        for _ in 0..2_000 {
            if reconciler.dead_letter_len() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        assert_eq!(reconciler.pending_len(), 0, "record leaves the pending queue");
        let dead = reconciler.dead_letters();
        assert_eq!(dead.len(), 1, "unsettled money is retained, not silently dropped");
        assert_eq!(dead[0].amount, AmountMicros(4200));
        assert_eq!(dead[0].hold, hold());
        assert_eq!(dead[0].attempts, 3);
    }

    #[tokio::test]
    async fn reconcile_now_settles_pending_without_background_runtime() {
        // Fails twice then succeeds; drive it purely through the explicit hook.
        let gateway = Arc::new(FlakyCaptureGateway::new(2));
        let reconciler = Arc::new(CaptureReconciler::new(
            gateway.clone(),
            ReconcilerConfig {
                max_attempts: 8,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
            },
        ));

        // Register without letting the background loop claim it first.
        {
            let mut state = reconciler.lock();
            state.pending.insert(
                hold().0,
                PendingRecord {
                    hold: hold(),
                    amount: AmountMicros(4200),
                    attempts: 0,
                    reconciling: false,
                },
            );
        }

        // Each pass retries once; three passes: fail, fail, settle.
        let _ = reconciler.reconcile_now().await;
        let _ = reconciler.reconcile_now().await;
        let settled = reconciler.reconcile_now().await.expect("third pass settles");

        assert_eq!(settled, 1);
        assert_eq!(reconciler.pending_len(), 0);
        assert_eq!(reconciler.dead_letter_len(), 0);
    }

    #[tokio::test]
    async fn duplicate_enqueue_does_not_double_capture() {
        let gateway = Arc::new(FlakyCaptureGateway::new(0)); // always succeeds
        let reconciler = Arc::new(CaptureReconciler::with_defaults(gateway.clone()));

        // Explicit-failure path and drop-guard both enqueue the same hold.
        reconciler.enqueue(hold(), AmountMicros(4200));
        reconciler.enqueue(hold(), AmountMicros(4200));

        for _ in 0..2_000 {
            if reconciler.pending_len() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(reconciler.pending_len(), 0);
        // Exactly one capture despite two enqueues.
        assert_eq!(gateway.capture_calls.load(Ordering::SeqCst), 1);
    }
}
