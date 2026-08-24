//! Token + cost usage for a completed job.

use serde::{Deserialize, Serialize};

use crate::money::Micros;

/// Normalized usage for a single completion. `cost_micros` is the authoritative
/// billed amount in integer microdollars (never a float).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default = "Micros::zero")]
    pub cost_micros: Micros,
}

impl Usage {
    /// Construct usage, defaulting `total_tokens` to `prompt + completion` when
    /// zero was reported upstream.
    pub fn new(
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_micros: Micros,
    ) -> Self {
        let total = if total_tokens == 0 {
            prompt_tokens.saturating_add(completion_tokens)
        } else {
            total_tokens
        };
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: total,
            cost_micros,
        }
    }
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_micros: Micros::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_total_from_parts_when_zero() {
        let u = Usage::new(10, 5, 0, Micros::new(1200));
        assert_eq!(u.total_tokens, 15);
        assert_eq!(u.cost_micros, Micros::new(1200));
    }

    #[test]
    fn new_prefers_reported_total() {
        let u = Usage::new(10, 5, 99, Micros::ZERO);
        assert_eq!(u.total_tokens, 99);
    }

    #[test]
    fn usage_roundtrips_json_with_integer_cost() {
        let u = Usage::new(3, 4, 7, Micros::new(500));
        let json = serde_json::to_string(&u).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, u);
        assert!(json.contains("\"cost_micros\":500"));
    }
}
