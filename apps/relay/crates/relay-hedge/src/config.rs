//! Hedging configuration. Defaults preserve the existing sequential behavior so
//! that adding this layer is a no-op until an operator opts in.

use serde::{Deserialize, Serialize};

/// Per-model hedging policy.
///
/// The default (`enabled = false`, `max_concurrency = 1`) is the current
/// behavior: attempts run one at a time, in order, first `Ok` wins. Every field
/// is `#[serde(default)]` so an omitted `[models.*.hedge]` section deserializes
/// to exactly this default.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HedgeConfig {
    /// Master switch. When `false`, hedging is off regardless of the other
    /// fields and dispatch takes the sequential path.
    pub enabled: bool,
    /// Maximum number of provider attempts to race concurrently. `1` (or `0`,
    /// clamped to `1`) means sequential — the untouched fallback path.
    pub max_concurrency: usize,
    /// Optional delay before launching each attempt after the first, so a fast
    /// primary usually wins alone before any hedge is even sent upstream.
    pub stagger_ms: Option<u64>,
    /// Hard per-request cap on the summed worst-case upstream cost (micros) of
    /// concurrently-launched attempts. Never a ledger charge — see crate docs.
    pub max_cost_micros: Option<u64>,
}

impl Default for HedgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrency: 1,
            stagger_ms: None,
            max_cost_micros: None,
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
}
