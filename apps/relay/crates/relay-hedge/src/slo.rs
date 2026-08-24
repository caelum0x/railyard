//! The provider-health seam: a trait that reorders the routing slice before
//! dispatch and records the outcome of each attempt afterwards.
//!
//! The default [`NoOpProviderHealth`] returns routing unchanged and records
//! nothing, so injecting the seam is behavior-preserving until an operator opts
//! into a real implementation like [`crate::health_store::InMemoryProviderHealth`].

use std::sync::Arc;
use std::time::Duration;

/// How an attempt's result should be counted by the scorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultClass {
    /// A successful response.
    Ok,
    /// A retryable/transient failure (timeout, 5xx, connection reset). Counts
    /// toward the error rate and the circuit breaker.
    Transient,
    /// A fatal, non-retryable failure (auth, 4xx). Counts toward the error rate.
    Fatal,
}

impl ResultClass {
    /// `true` for any failure class.
    pub fn is_error(self) -> bool {
        !matches!(self, ResultClass::Ok)
    }
}

/// The observed outcome of a single provider attempt.
#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    /// Total wall-clock latency of the attempt, if measured.
    pub latency: Option<Duration>,
    /// Time to first token (streaming), if measured.
    pub ttft: Option<Duration>,
    /// Success / transient / fatal.
    pub result: ResultClass,
}

impl Outcome {
    /// A successful outcome with a measured latency.
    pub fn success(latency: Duration) -> Self {
        Self {
            latency: Some(latency),
            ttft: None,
            result: ResultClass::Ok,
        }
    }

    /// A transient failure with no useful latency.
    pub fn transient_error() -> Self {
        Self {
            latency: None,
            ttft: None,
            result: ResultClass::Transient,
        }
    }

    /// A fatal failure.
    pub fn fatal_error() -> Self {
        Self {
            latency: None,
            ttft: None,
            result: ResultClass::Fatal,
        }
    }
}

/// Per-endpoint health used to rank and shed providers.
///
/// Implementations MUST be cheap, non-blocking, and side-effect-free with
/// respect to the actual provider call — they only influence *order*, never
/// *how* a provider is invoked.
pub trait ProviderHealth: Send + Sync {
    /// Return `routing` reordered best-first for `model_name`. Every input
    /// endpoint MUST appear exactly once in the output (nothing is dropped): a
    /// fully-degraded fleet still attempts every provider as a last resort.
    fn rank(&self, model_name: &str, routing: &[Arc<str>]) -> Vec<Arc<str>>;

    /// Record the outcome of one attempt so future rankings reflect it.
    fn record(&self, model_name: &str, provider: &str, outcome: Outcome);
}

/// The identity implementation: routing is returned unchanged and outcomes are
/// ignored. This is the default injected when SLO routing is disabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpProviderHealth;

impl ProviderHealth for NoOpProviderHealth {
    fn rank(&self, _model_name: &str, routing: &[Arc<str>]) -> Vec<Arc<str>> {
        routing.to_vec()
    }

    fn record(&self, _model_name: &str, _provider: &str, _outcome: Outcome) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_preserves_routing_order() {
        let routing: Vec<Arc<str>> = vec!["a".into(), "b".into(), "c".into()];
        let health = NoOpProviderHealth;
        assert_eq!(health.rank("m", &routing), routing);
        // record is a no-op and must not panic.
        health.record("m", "a", Outcome::transient_error());
    }
}
