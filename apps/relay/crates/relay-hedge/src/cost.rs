//! Worst-case per-attempt cost estimation for the hedge cost cap.
//!
//! These are UPSTREAM (vendor-side) worst-case estimates used only to bound how
//! many attempts race concurrently. They are never a relay-ledger charge — the
//! ledger only ever settles the single winning response's actual usage.

/// Worst-case cost (micros) of an attempt priced per-token: `max_tokens *
/// price_per_token_micros`. Saturating so a pathological config can't overflow.
pub fn worst_case_cost_micros(max_tokens: u64, price_per_token_micros: u64) -> u64 {
    max_tokens.saturating_mul(price_per_token_micros)
}

/// Worst-case cost (micros) when the price is quoted per million tokens (the
/// common vendor unit). Rounds up so the cap is never under-counted.
pub fn worst_case_cost_micros_per_million(max_tokens: u64, price_per_million_micros: u64) -> u64 {
    let product = (max_tokens as u128).saturating_mul(price_per_million_micros as u128);
    // Ceiling division by 1_000_000.
    let micros = product.div_ceil(1_000_000);
    micros.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_token_multiplies() {
        assert_eq!(worst_case_cost_micros(1000, 3), 3000);
    }

    #[test]
    fn per_token_saturates_instead_of_overflowing() {
        assert_eq!(worst_case_cost_micros(u64::MAX, 2), u64::MAX);
    }

    #[test]
    fn per_million_rounds_up() {
        // 1500 tokens at 2_000_000 micros / M = 3000 micros exactly.
        assert_eq!(worst_case_cost_micros_per_million(1500, 2_000_000), 3000);
        // 1 token at 3_000_000 / M = 3 micros, rounds up from 3.0.
        assert_eq!(worst_case_cost_micros_per_million(1, 3_000_000), 3);
        // 1 token at 1 micro / M rounds up to 1 (never 0).
        assert_eq!(worst_case_cost_micros_per_million(1, 1), 1);
    }
}
