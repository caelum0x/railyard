//! Per-endpoint EWMA scoring plus a circuit-breaker state machine.
//!
//! Lower score == healthier == ranked earlier. A tripped (`Open`) circuit is
//! pushed to the tail of the ranking but never removed, so a fully-degraded
//! fleet still attempts every provider as a last resort.
//!
//! Time is read via [`tokio::time::Instant`] so tests can drive the circuit
//! breaker deterministically under a paused Tokio clock.

use std::time::Duration;

use tokio::time::Instant;

use crate::slo::{Outcome, ResultClass};

/// Relative weights of the three health signals in the composite score.
#[derive(Clone, Copy, Debug)]
pub struct ScoreWeights {
    /// Weight on normalized tail latency (p99-ish).
    pub latency: f64,
    /// Weight on the recent (EWMA) error rate.
    pub error: f64,
    /// Weight on long-run unavailability (`1 - uptime`).
    pub uptime: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            latency: 0.5,
            error: 0.35,
            uptime: 0.15,
        }
    }
}

/// Tunables for EWMA smoothing and the circuit breaker.
#[derive(Clone, Copy, Debug)]
pub struct SloConfig {
    /// Composite-score weights.
    pub weights: ScoreWeights,
    /// EWMA smoothing factor in `(0, 1]`. Higher reacts faster.
    pub alpha: f64,
    /// Latency scale (ms) used to normalize tail latency into `[0, 1)` via
    /// `lat / (lat + scale)`.
    pub latency_scale_ms: f64,
    /// Error-rate threshold above which the breaker trips `Open`.
    pub trip_error_rate: f64,
    /// Minimum samples before the breaker may trip (avoids tripping on one bad
    /// request during warmup).
    pub min_samples: u64,
    /// How long the breaker stays `Open` before admitting a `HalfOpen` probe.
    pub open_cooldown: Duration,
    /// Neutral score assigned to an endpoint with no recorded history, so
    /// unknown providers slot between known-good and known-bad while preserving
    /// configured order among themselves.
    pub neutral_score: f64,
}

impl Default for SloConfig {
    fn default() -> Self {
        Self {
            weights: ScoreWeights::default(),
            alpha: 0.3,
            latency_scale_ms: 500.0,
            trip_error_rate: 0.5,
            min_samples: 3,
            open_cooldown: Duration::from_secs(30),
            neutral_score: 0.25,
        }
    }
}

/// Circuit-breaker state for one endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
    /// Healthy: scored normally.
    Closed,
    /// Tripped: shed to the tail until `until`.
    Open { until: Instant },
    /// Cooldown elapsed: exactly one probe is admitted. `probing` is `true`
    /// once that probe has been handed out (so a second `rank` sheds again
    /// until the probe resolves via `record`).
    HalfOpen { probing: bool },
}

/// Rolling health of one endpoint.
#[derive(Clone, Debug)]
pub struct EwmaScore {
    /// EWMA of response latency (ms).
    ewma_latency_ms: f64,
    /// EWMA of TTFT (ms).
    ewma_ttft_ms: f64,
    /// Decaying max of EWMA latency — a cheap p99-ish tail signal.
    decaying_max_ms: f64,
    /// EWMA over `{0.0 success, 1.0 failure}` — the recent error rate.
    error_rate: f64,
    /// Lifetime attempt count.
    total: u64,
    /// Lifetime failure count (for long-run uptime).
    failures: u64,
    /// Circuit-breaker state.
    circuit: CircuitState,
    /// Admission deadline for an in-flight `HalfOpen` probe. Set when a probe
    /// is handed out (`rank_score`); if the probe is never dispatched (hence
    /// never resolved via `record`) the deadline lets `maybe_half_open` re-arm
    /// a fresh probe instead of leaving the endpoint stuck `probing` forever.
    half_open_deadline: Option<Instant>,
}

impl Default for EwmaScore {
    fn default() -> Self {
        Self {
            ewma_latency_ms: 0.0,
            ewma_ttft_ms: 0.0,
            decaying_max_ms: 0.0,
            error_rate: 0.0,
            total: 0,
            failures: 0,
            circuit: CircuitState::Closed,
            half_open_deadline: None,
        }
    }
}

impl EwmaScore {
    /// Fold one attempt outcome into the rolling statistics and advance the
    /// circuit breaker.
    pub fn record(&mut self, outcome: Outcome, cfg: &SloConfig, now: Instant) {
        let alpha = cfg.alpha;
        self.total = self.total.saturating_add(1);

        // Latency / TTFT EWMAs (only on measured samples).
        if let Some(latency) = outcome.latency {
            let ms = latency.as_secs_f64() * 1000.0;
            self.ewma_latency_ms = ewma(self.ewma_latency_ms, ms, alpha, self.total == 1);
            // Decaying max: jump up to a new peak, otherwise decay toward the
            // current EWMA so a one-off spike fades but is not instantly lost.
            self.decaying_max_ms = self.decaying_max_ms.max(ms);
            self.decaying_max_ms = ewma(
                self.decaying_max_ms,
                self.ewma_latency_ms,
                alpha * 0.5,
                false,
            );
        }
        if let Some(ttft) = outcome.ttft {
            let ms = ttft.as_secs_f64() * 1000.0;
            self.ewma_ttft_ms = ewma(self.ewma_ttft_ms, ms, alpha, self.ewma_ttft_ms == 0.0);
        }

        // Error-rate EWMA.
        let is_error = outcome.result.is_error();
        let sample = if is_error { 1.0 } else { 0.0 };
        self.error_rate = ewma(self.error_rate, sample, alpha, false);
        if is_error {
            self.failures = self.failures.saturating_add(1);
        }

        // Circuit-breaker transitions.
        match (self.circuit, outcome.result) {
            // A probe resolved: success closes, failure re-opens. Either way the
            // in-flight probe is done, so clear its admission deadline.
            (CircuitState::HalfOpen { .. }, ResultClass::Ok) => {
                self.circuit = CircuitState::Closed;
                self.half_open_deadline = None;
                // Reset the recent error signal so the freshly-closed endpoint
                // isn't immediately re-tripped by stale error history.
                self.error_rate = 0.0;
            }
            (CircuitState::HalfOpen { .. }, _) => {
                self.circuit = CircuitState::Open {
                    until: now + cfg.open_cooldown,
                };
                self.half_open_deadline = None;
            }
            // Closed and now unhealthy enough: trip.
            (CircuitState::Closed, _) => {
                if self.total >= cfg.min_samples && self.error_rate > cfg.trip_error_rate {
                    self.circuit = CircuitState::Open {
                        until: now + cfg.open_cooldown,
                    };
                }
            }
            // Open: outcomes recorded while open don't change the deadline.
            (CircuitState::Open { .. }, _) => {}
        }
    }

    /// Advance the circuit toward probe-eligibility. Called by the ranker before
    /// scoring. Two transitions:
    ///
    /// - `Open -> HalfOpen { probing: false }` once the cooldown has elapsed, so
    ///   a cooled-down endpoint becomes probe-eligible.
    /// - Re-arm a stuck probe: if a `HalfOpen` probe was admitted but never
    ///   dispatched (never resolved via `record`) and its admission deadline has
    ///   passed, drop back to `probing: false` so a fresh probe is admitted.
    ///   This bounds the stuck window to one cooldown instead of forever.
    pub fn maybe_half_open(&mut self, now: Instant) {
        match self.circuit {
            CircuitState::Open { until } if now >= until => {
                self.circuit = CircuitState::HalfOpen { probing: false };
                self.half_open_deadline = None;
            }
            CircuitState::HalfOpen { probing: true } => {
                if let Some(deadline) = self.half_open_deadline {
                    if now >= deadline {
                        self.circuit = CircuitState::HalfOpen { probing: false };
                        self.half_open_deadline = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// The composite score (lower is better) plus whether this endpoint should
    /// be shed to the tail. Marks a `HalfOpen` endpoint as `probing` so only a
    /// single probe is admitted per cooldown.
    pub fn rank_score(&mut self, cfg: &SloConfig, now: Instant) -> ScoreOutput {
        match self.circuit {
            CircuitState::Open { .. } => ScoreOutput {
                score: self.composite(cfg),
                shed: true,
            },
            CircuitState::HalfOpen { probing: true } => ScoreOutput {
                score: self.composite(cfg),
                shed: true,
            },
            CircuitState::HalfOpen { probing: false } => {
                // Admit exactly one probe: hand this one out (not shed), then
                // flip `probing` so the next rank sheds until `record` resolves.
                // Stamp an admission deadline so an undispatched probe re-arms
                // (see `maybe_half_open`) rather than sticking forever.
                self.circuit = CircuitState::HalfOpen { probing: true };
                self.half_open_deadline = Some(now + cfg.open_cooldown);
                ScoreOutput {
                    score: self.composite(cfg),
                    shed: false,
                }
            }
            CircuitState::Closed => ScoreOutput {
                score: self.composite(cfg),
                shed: false,
            },
        }
    }

    /// Weighted sum of the three normalized signals.
    fn composite(&self, cfg: &SloConfig) -> f64 {
        let w = &cfg.weights;
        let norm_latency = self.decaying_max_ms / (self.decaying_max_ms + cfg.latency_scale_ms);
        let uptime = if self.total == 0 {
            1.0
        } else {
            1.0 - (self.failures as f64 / self.total as f64)
        };
        w.latency * norm_latency + w.error * self.error_rate + w.uptime * (1.0 - uptime)
    }

    /// Current circuit state (for observability/tests).
    pub fn circuit(&self) -> CircuitState {
        self.circuit
    }

    /// Current recent error rate (for observability/tests).
    pub fn error_rate(&self) -> f64 {
        self.error_rate
    }
}

/// Output of scoring one endpoint.
#[derive(Clone, Copy, Debug)]
pub struct ScoreOutput {
    /// Composite score, lower is better.
    pub score: f64,
    /// If `true`, sort this endpoint to the tail (still present as fallback).
    pub shed: bool,
}

/// One EWMA update step. On the very first sample (`seed`), adopt the value
/// directly instead of blending from zero.
fn ewma(prev: f64, sample: f64, alpha: f64, seed: bool) -> f64 {
    if seed {
        sample
    } else {
        alpha * sample + (1.0 - alpha) * prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[gtest]
    #[tokio::test(start_paused = true)]
    async fn errors_raise_error_rate_and_trip_breaker() {
        let cfg = SloConfig::default();
        let mut s = EwmaScore::default();
        let now = Instant::now();
        for _ in 0..5 {
            s.record(Outcome::transient_error(), &cfg, now);
        }
        expect_that!(s.error_rate(), gt(cfg.trip_error_rate));
        expect_that!(s.circuit(), matches_pattern!(CircuitState::Open { .. }));
    }

    #[gtest]
    #[tokio::test(start_paused = true)]
    async fn healthy_endpoint_scores_below_erroring_one() {
        let cfg = SloConfig::default();
        let now = Instant::now();
        let mut good = EwmaScore::default();
        let mut bad = EwmaScore::default();
        for _ in 0..5 {
            good.record(Outcome::success(Duration::from_millis(100)), &cfg, now);
            bad.record(Outcome::transient_error(), &cfg, now);
        }
        let good_score = good.rank_score(&cfg, now).score;
        let bad_score = bad.rank_score(&cfg, now).score;
        expect_that!(good_score, lt(bad_score));
    }

    #[gtest]
    #[tokio::test(start_paused = true)]
    async fn undispatched_probe_rearms_after_cooldown_instead_of_sticking() {
        let cfg = SloConfig::default();
        let mut s = EwmaScore::default();
        let now = Instant::now();

        // Trip the breaker Open.
        for _ in 0..5 {
            s.record(Outcome::transient_error(), &cfg, now);
        }
        expect_that!(s.circuit(), matches_pattern!(CircuitState::Open { .. }));

        // Cooldown elapses: a probe is admitted (not shed) and marked in-flight.
        let t1 = now + cfg.open_cooldown + Duration::from_secs(1);
        s.maybe_half_open(t1);
        expect_that!(
            s.rank_score(&cfg, t1).shed,
            eq(false),
            "first probe should be admitted"
        );
        expect_that!(s.circuit(), eq(CircuitState::HalfOpen { probing: true }));

        // The probe is never dispatched, so `record` is never called. Before the
        // re-arm deadline the endpoint stays shed (only one probe in flight).
        let t_mid = t1 + Duration::from_secs(1);
        s.maybe_half_open(t_mid);
        expect_that!(s.rank_score(&cfg, t_mid).shed, eq(true));

        // Once the admission deadline passes, a fresh probe must be admitted
        // again — the endpoint must NOT be stuck shed forever.
        let t2 = t1 + cfg.open_cooldown + Duration::from_secs(1);
        s.maybe_half_open(t2);
        expect_that!(
            s.rank_score(&cfg, t2).shed,
            eq(false),
            "recovered endpoint must get a fresh probe, not be shed forever"
        );
    }
}
