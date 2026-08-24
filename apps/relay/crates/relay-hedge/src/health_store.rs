//! In-memory [`ProviderHealth`] backed by per-`(model, provider)` EWMA scores
//! and circuit breakers.
//!
//! There is no cross-request health store in TensorZero today, so this is the
//! new state. It is intentionally dependency-light (a `Mutex<HashMap>`), created
//! once at gateway startup and shared behind an `Arc`. A Postgres warm-start
//! (seeding EWMAs from `model_provider_statistics` / `model_inferences`) plugs
//! in via [`InMemoryProviderHealth::seed`] without changing the trait surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::score::{CircuitState, EwmaScore, SloConfig};
use crate::slo::{Outcome, ProviderHealth};

type Key = (Arc<str>, Arc<str>);

/// A concurrent, in-memory provider-health scorer.
#[derive(Clone)]
pub struct InMemoryProviderHealth {
    scores: Arc<Mutex<HashMap<Key, EwmaScore>>>,
    cfg: SloConfig,
}

impl InMemoryProviderHealth {
    /// Create an empty store with the given SLO tunables.
    pub fn new(cfg: SloConfig) -> Self {
        Self {
            scores: Arc::new(Mutex::new(HashMap::new())),
            cfg,
        }
    }

    /// Create an empty store with default tunables.
    pub fn with_defaults() -> Self {
        Self::new(SloConfig::default())
    }

    /// Warm-start seed for one `(model, provider)` pair. Replays `sample_count`
    /// synthetic outcomes reconstructed from aggregate stats (mean latency and
    /// error fraction), so the store isn't cold at startup. Intended to be fed
    /// from Postgres rollups; kept transport-free here for testability.
    pub fn seed(
        &self,
        model_name: &str,
        provider: &str,
        mean_latency: Duration,
        error_fraction: f64,
        sample_count: u64,
    ) {
        let now = Instant::now();
        let key: Key = (Arc::from(model_name), Arc::from(provider));
        let mut guard = self.lock();
        let entry = guard.entry(key).or_default();
        let errors = (error_fraction.clamp(0.0, 1.0) * sample_count as f64).round() as u64;
        for i in 0..sample_count {
            let outcome = if i < errors {
                Outcome::transient_error()
            } else {
                Outcome::success(mean_latency)
            };
            entry.record(outcome, &self.cfg, now);
        }
    }

    /// Read the circuit state for a pair (for observability/tests).
    pub fn circuit_state(&self, model_name: &str, provider: &str) -> Option<CircuitState> {
        let key: Key = (Arc::from(model_name), Arc::from(provider));
        self.lock().get(&key).map(EwmaScore::circuit)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, EwmaScore>> {
        // A poisoned lock means another thread panicked mid-update; recover the
        // guard rather than cascading the panic — a stale score is harmless.
        self.scores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ProviderHealth for InMemoryProviderHealth {
    fn rank(&self, model_name: &str, routing: &[Arc<str>]) -> Vec<Arc<str>> {
        let now = Instant::now();
        let mut guard = self.lock();

        // Score each endpoint, preserving the input index for a stable tiebreak.
        let mut scored: Vec<(usize, bool, f64, Arc<str>)> = routing
            .iter()
            .enumerate()
            .map(|(idx, provider)| {
                let key: Key = (Arc::from(model_name), provider.clone());
                match guard.get_mut(&key) {
                    None => (idx, false, self.cfg.neutral_score, provider.clone()),
                    Some(entry) => {
                        entry.maybe_half_open(now);
                        let out = entry.rank_score(&self.cfg, now);
                        (idx, out.shed, out.score, provider.clone())
                    }
                }
            })
            .collect();

        // Order: non-shed before shed; then ascending score; then configured
        // order (stable). `sort_by` is stable, so the final `.then` on the
        // original index makes equal-key ordering fully deterministic.
        scored.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.0.cmp(&b.0))
        });

        scored.into_iter().map(|(_, _, _, p)| p).collect()
    }

    fn record(&self, model_name: &str, provider: &str, outcome: Outcome) {
        let now = Instant::now();
        let key: Key = (Arc::from(model_name), Arc::from(provider));
        let mut guard = self.lock();
        guard
            .entry(key)
            .or_default()
            .record(outcome, &self.cfg, now);
    }
}
