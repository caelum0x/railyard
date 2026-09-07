//! Hedging configuration. Defaults preserve the existing sequential behavior so
//! that adding this layer is a no-op until an operator opts in.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Per-model hedging policy.
///
/// The default (`enabled = false`, `max_concurrency = 1`) is the current
/// behavior: attempts run one at a time, in order, first `Ok` wins. Every field
/// is `#[serde(default)]` so an omitted `[models.*.hedge]` section deserializes
/// to exactly this default.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(default, deny_unknown_fields)]
pub struct HedgeConfig {
    /// Master switch. When `false`, hedging is off regardless of the other
    /// fields and dispatch takes the sequential path.
    pub enabled: bool,
    /// Maximum number of provider attempts to race concurrently. `1` (or `0`,
    /// clamped to `1`) means sequential — the untouched fallback path.
    pub max_concurrency: usize,
    /// Optional interval between provider launches. Secondary attempt `N`
    /// starts at `N * stagger_ms` from the beginning of the race, so backups
    /// are introduced progressively instead of bursting together.
    pub stagger_ms: Option<u64>,
    /// Hard per-request cap on the summed worst-case upstream cost (micros) of
    /// concurrently-launched attempts. Never a ledger charge — see crate docs.
    pub max_cost_micros: Option<u64>,
    /// Worst-case upstream cost for each provider attempt. When a cost cap is
    /// configured, an unpriced secondary is conservatively treated as over the
    /// cap and is not launched concurrently.
    pub provider_cost_micros: BTreeMap<String, u64>,
}

impl Default for HedgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrency: 1,
            stagger_ms: None,
            max_cost_micros: None,
            provider_cost_micros: BTreeMap::new(),
        }
    }
}

impl HedgeConfig {
    /// A convenience constructor for the common "race N" case.
    pub fn hedged(max_concurrency: usize) -> Self {
        Self {
            enabled: true,
            max_concurrency,
            ..Self::default()
        }
    }

    /// The effective concurrency after applying the master switch and clamping.
    /// Returns `1` whenever hedging is disabled, so callers can branch on a
    /// single number.
    pub fn effective_concurrency(&self) -> usize {
        if !self.enabled {
            return 1;
        }
        self.max_concurrency.max(1)
    }

    /// Whether the concurrent (raced) path should be taken at all.
    pub fn is_hedging(&self) -> bool {
        self.effective_concurrency() > 1
    }

    /// Resolve the cost committed by one attempt. Without a request cap, cost
    /// metadata is observational and missing entries safely remain zero. With
    /// a cap, a missing price is returned as `cap + 1`, so `race` admits the
    /// primary but blocks that attempt from the concurrent hedge window.
    pub fn attempt_cost_micros(&self, provider: &str) -> u64 {
        self.provider_cost_micros
            .get(provider)
            .copied()
            .unwrap_or_else(|| {
                self.max_cost_micros
                    .map(|cap| cap.saturating_add(1))
                    .unwrap_or(0)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_sequential_and_disabled() {
        let c = HedgeConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.effective_concurrency(), 1);
        assert!(!c.is_hedging());
    }

    #[test]
    fn disabled_forces_concurrency_one_even_with_high_max() {
        let c = HedgeConfig {
            enabled: false,
            max_concurrency: 8,
            ..HedgeConfig::default()
        };
        assert_eq!(c.effective_concurrency(), 1);
    }

    #[test]
    fn zero_concurrency_clamps_to_one() {
        let c = HedgeConfig {
            enabled: true,
            max_concurrency: 0,
            ..HedgeConfig::default()
        };
        assert_eq!(c.effective_concurrency(), 1);
    }

    #[test]
    fn omitted_section_deserializes_to_default() {
        // An empty table must round-trip to the sequential default.
        let c: HedgeConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, HedgeConfig::default());
    }

    #[test]
    fn capped_unpriced_secondary_is_conservatively_over_cap() {
        let c = HedgeConfig {
            max_cost_micros: Some(100),
            ..HedgeConfig::hedged(2)
        };
        assert_eq!(c.attempt_cost_micros("unknown"), 101);
    }
}
