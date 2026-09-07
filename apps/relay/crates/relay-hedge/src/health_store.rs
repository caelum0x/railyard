//! In-memory [`ProviderHealth`] backed by per-`(model, provider)` EWMA scores
//! and circuit breakers.
//!
//! TensorZero did not have a cross-request provider-health store, so this is an
//! additive Relay state layer. It is intentionally dependency-light (a
//! `Mutex<HashMap>`), created once and shared for the gateway process. Relay's
//! gateway periodically persists [`ProviderHealthSnapshot`] records to Postgres
//! and restores them at startup; [`InMemoryProviderHealth::seed`] remains useful
//! to reconstruct a cold start from other aggregate data sources.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::score::{CircuitSnapshot, CircuitState, EwmaScore, EwmaScoreSnapshot, SloConfig};
use crate::slo::{Outcome, ProviderHealth};

type Key = (Arc<str>, Arc<str>);

/// One restart-durable `(model, provider)` health record.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ProviderHealthSnapshot {
    pub model_name: String,
    pub provider_name: String,
    /// Wall-clock capture time used only to subtract process downtime from an
    /// open circuit's remaining cooldown. Zero supports old snapshots.
    #[serde(default)]
    pub captured_at_unix_ms: u64,
    pub score: EwmaScoreSnapshot,
}

/// A concurrent, in-memory provider-health scorer.
#[derive(Clone, Debug)]
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

    /// Export a consistent restart snapshot without holding the lock during I/O.
    pub fn snapshot(&self) -> Vec<ProviderHealthSnapshot> {
        let now = Instant::now();
        let captured_at_unix_ms = unix_time_ms();
        self.lock()
            .iter()
            .map(|((model, provider), score)| ProviderHealthSnapshot {
                model_name: model.to_string(),
                provider_name: provider.to_string(),
                captured_at_unix_ms,
                score: score.snapshot(now),
            })
            .collect()
    }

    /// Merge validated restart records into the live scorer. Invalid rows are
    /// ignored rather than poisoning routing; returns the accepted row count.
    pub fn restore(&self, snapshots: Vec<ProviderHealthSnapshot>) -> usize {
        let now = Instant::now();
        let mut restored = 0;
        let mut guard = self.lock();
        let restored_at_unix_ms = unix_time_ms();
        for mut snapshot in snapshots {
            if snapshot.model_name.is_empty() || snapshot.provider_name.is_empty() {
                continue;
            }
            if snapshot.captured_at_unix_ms != 0 {
                let downtime_ms = restored_at_unix_ms.saturating_sub(snapshot.captured_at_unix_ms);
                if let CircuitSnapshot::Open {
                    remaining_cooldown_ms,
                } = &mut snapshot.score.circuit
                {
                    *remaining_cooldown_ms = remaining_cooldown_ms.saturating_sub(downtime_ms);
                }
            }
            let Some(score) = EwmaScore::restore(snapshot.score, now) else {
                continue;
            };
            guard.insert(
                (
                    Arc::from(snapshot.model_name),
                    Arc::from(snapshot.provider_name),
                ),
                score,
            );
            restored += 1;
        }
        restored
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, EwmaScore>> {
        // A poisoned lock means another thread panicked mid-update; recover the
        // guard rather than cascading the panic — a stale score is harmless.
        self.scores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
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

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[gtest]
    #[tokio::test(start_paused = true)]
    async fn snapshot_restore_preserves_ranking_and_circuit_state() {
        let original = InMemoryProviderHealth::with_defaults();
        for _ in 0..5 {
            original.record("chat", "bad", Outcome::transient_error());
            original.record("chat", "good", Outcome::success(Duration::from_millis(50)));
        }
        let snapshots = original.snapshot();

        let restored = InMemoryProviderHealth::with_defaults();
        expect_that!(restored.restore(snapshots), eq(2));
        assert_eq!(
            restored.rank("chat", &[Arc::from("bad"), Arc::from("good")]),
            vec![Arc::from("good"), Arc::from("bad")]
        );
        expect_that!(
            restored.circuit_state("chat", "bad"),
            some(matches_pattern!(CircuitState::Open { .. }))
        );
    }

    #[gtest]
    #[tokio::test(start_paused = true)]
    async fn restore_skips_invalid_or_unidentified_rows() {
        let store = InMemoryProviderHealth::with_defaults();
        let valid_score = EwmaScore::default().snapshot(Instant::now());
        let mut invalid_score = valid_score.clone();
        invalid_score.error_rate = 2.0;

        let restored = store.restore(vec![
            ProviderHealthSnapshot {
                model_name: String::new(),
                provider_name: "provider".into(),
                captured_at_unix_ms: 0,
                score: valid_score,
            },
            ProviderHealthSnapshot {
                model_name: "model".into(),
                provider_name: "provider".into(),
                captured_at_unix_ms: 0,
                score: invalid_score,
            },
        ]);
        expect_that!(restored, eq(0));
    }
}
