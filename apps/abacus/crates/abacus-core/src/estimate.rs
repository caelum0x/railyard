//! Pre-flight cost estimation types.
//!
//! Before a request is forwarded, Abacus must price the MAXIMUM possible cost it
//! could incur ([`CostEstimate::est_max`]) and gate the budget against THAT, not
//! against a hopeful average. `est_max` is the number reserved on the Mint hold,
//! so a looping agent can never sneak past the ceiling by under-declaring.

use serde::{Deserialize, Serialize};

use crate::money::Micros;

/// A priced pre-flight estimate for one upstream request.
///
/// `est_prompt` is the priced cost of the (already-known) prompt tokens.
/// `est_max` is the priced cost of the WORST case — prompt plus the maximum
/// completion the request could generate (`max_tokens`, provider ceiling). The
/// reserve gate always uses `est_max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEstimate {
    /// Priced cost of the known prompt/input tokens, in µ$.
    pub est_prompt: Micros,
    /// Priced MAX possible cost (prompt + max completion), in µ$. This is what
    /// the hold reserves.
    pub est_max: Micros,
    /// Input tokens the estimate was priced from.
    pub prompt_tokens: i64,
    /// Maximum completion tokens the estimate assumed.
    pub max_completion_tokens: i64,
}

impl CostEstimate {
    /// Build an estimate, guaranteeing `est_max >= est_prompt` (the max case can
    /// never be cheaper than the prompt alone).
    pub fn new(
        est_prompt: Micros,
        est_max: Micros,
        prompt_tokens: i64,
        max_completion_tokens: i64,
    ) -> Self {
        let est_max = if est_max.get() < est_prompt.get() {
            est_prompt
        } else {
            est_max
        };
        CostEstimate {
            est_prompt,
            est_max,
            prompt_tokens,
            max_completion_tokens,
        }
    }

    /// The amount the reserve gate must hold: the priced MAX possible cost.
    pub fn reserve_amount(&self) -> Micros {
        self.est_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn est_max_is_the_reserve_amount() {
        let e = CostEstimate::new(Micros::new(100), Micros::new(5_000), 10, 500);
        assert_eq!(e.reserve_amount(), Micros::new(5_000));
    }

    #[test]
    fn est_max_never_below_prompt() {
        // A degenerate max below prompt is clamped up to the prompt cost.
        let e = CostEstimate::new(Micros::new(9_000), Micros::new(1), 900, 0);
        assert_eq!(e.est_max, Micros::new(9_000));
    }
}
