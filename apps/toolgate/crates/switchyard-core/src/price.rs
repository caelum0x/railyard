//! Integer micro-USD pricing. A [`Price`] produces two numbers:
//!
//! - a **quote**: the upper bound held on Mint before the call, computed from
//!   the caller's input alone;
//! - a **settlement**: the exact amount captured after the call, computed from
//!   the outcome (status class, units the provider reported, or the provider's
//!   own reported cost). Failures always settle to zero.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceBasis {
    /// Billed whenever the provider accepts the request (2xx; 4xx only if the
    /// tool is marked `bill_on_4xx`).
    PerCall,
    /// Billed only on a successful (2xx) answer.
    PerSuccess,
    /// `micros` per result returned; an empty answer is free.
    PerResult,
    /// `micros` per unit consumed (recipient, rendered second, 1k characters).
    PerUnit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub basis: PriceBasis,
    /// Micro-USD per call, per result, or per unit.
    pub micros: i64,
    /// Human unit label (`result`, `recipient`, `second`, `1k_chars`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// JSON Pointer into the input used to estimate units for the quote. A
    /// number is used as is, an array counts its items, a string counts chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units_from: Option<String>,
    /// Divide the estimated unit count (rounding up), e.g. 1000 for per-1k-chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_divisor: Option<u64>,
    /// Units assumed when the input does not say (e.g. default page size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_units: Option<u64>,
    /// Hard upper bound on units for one call (bounds the hold).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_units: Option<u64>,
    /// Price of a cache hit (default 0).
    #[serde(default)]
    pub cache_micros: i64,
}

impl Price {
    pub fn free() -> Self {
        Price {
            basis: PriceBasis::PerCall,
            micros: 0,
            unit: None,
            units_from: None,
            unit_divisor: None,
            default_units: None,
            max_units: None,
            cache_micros: 0,
        }
    }

    pub fn is_free(&self) -> bool {
        self.micros == 0
    }

    fn divide(&self, n: u64) -> u64 {
        match self.unit_divisor {
            Some(d) if d > 1 => n.div_ceil(d),
            _ => n,
        }
    }

    fn cap(&self, n: u64) -> u64 {
        match self.max_units {
            Some(m) => n.min(m),
            None => n,
        }
    }

    /// Units this input is expected to consume (upper bound).
    pub fn estimated_units(&self, input: &Value) -> u64 {
        if matches!(self.basis, PriceBasis::PerCall | PriceBasis::PerSuccess) {
            return 1;
        }
        let from_input = self.units_from.as_deref().and_then(|ptr| input.pointer(ptr)).and_then(|v| match v {
            Value::Number(n) => n.as_u64(),
            Value::Array(a) => Some(a.len() as u64),
            Value::String(s) => Some(s.chars().count() as u64),
            _ => None,
        });
        let raw = from_input.or(self.default_units).or(self.max_units).unwrap_or(1);
        self.cap(self.divide(raw)).max(1)
    }

    /// The hold amount for this input, in micros.
    pub fn quote(&self, input: &Value) -> i64 {
        self.micros.saturating_mul(self.estimated_units(input) as i64)
    }

    /// The exact amount to capture.
    ///
    /// - `status`: provider HTTP status class of the final attempt (0 = network/timeout).
    /// - `units`: units the adapter observed (results returned, seconds rendered).
    /// - `reported_cost`: the provider's own reported charge, when it has one.
    /// - `bill_on_4xx`: tool flag; only applies to `per_call` pricing.
    /// - `hold`: the reserved amount; a capture never exceeds it.
    pub fn settle(&self, status: u16, units: Option<u64>, reported_cost: Option<i64>, bill_on_4xx: bool, hold: i64) -> i64 {
        let success = (200..300).contains(&status);
        let billable_4xx = (400..500).contains(&status) && bill_on_4xx && self.basis == PriceBasis::PerCall;
        if !success && !billable_4xx {
            return 0;
        }
        let amount = if let Some(cost) = reported_cost {
            cost.max(0)
        } else {
            match self.basis {
                PriceBasis::PerCall | PriceBasis::PerSuccess => self.micros,
                PriceBasis::PerResult | PriceBasis::PerUnit => {
                    let u = self.cap(units.unwrap_or(0));
                    self.micros.saturating_mul(u as i64)
                }
            }
        };
        amount.min(hold.max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn price(basis: PriceBasis, micros: i64) -> Price {
        Price { basis, micros, ..Price::free() }
    }

    #[test]
    fn per_call_quote_and_settle() {
        let p = price(PriceBasis::PerCall, 2_000);
        assert_eq!(p.quote(&json!({})), 2_000);
        assert_eq!(p.settle(200, None, None, false, 2_000), 2_000);
        assert_eq!(p.settle(404, None, None, false, 2_000), 0);
        assert_eq!(p.settle(404, None, None, true, 2_000), 2_000);
        assert_eq!(p.settle(503, None, None, true, 2_000), 0);
        assert_eq!(p.settle(0, None, None, true, 2_000), 0, "timeouts are never billed");
    }

    #[test]
    fn per_success_never_bills_4xx() {
        let p = price(PriceBasis::PerSuccess, 1_500);
        assert_eq!(p.settle(422, None, None, true, 1_500), 0);
        assert_eq!(p.settle(201, None, None, false, 1_500), 1_500);
    }

    #[test]
    fn per_result_uses_input_for_quote_and_observed_for_settle() {
        let p = Price { units_from: Some("/limit".into()), default_units: Some(10), max_units: Some(100), ..price(PriceBasis::PerResult, 300) };
        assert_eq!(p.quote(&json!({"limit": 25})), 7_500);
        assert_eq!(p.quote(&json!({})), 3_000);
        assert_eq!(p.quote(&json!({"limit": 5000})), 30_000, "max_units bounds the hold");
        assert_eq!(p.settle(200, Some(3), None, false, 7_500), 900);
        assert_eq!(p.settle(200, Some(0), None, false, 7_500), 0, "an empty lookup is free");
    }

    #[test]
    fn per_unit_divisor_rounds_up() {
        let p = Price { units_from: Some("/text".into()), unit_divisor: Some(1000), ..price(PriceBasis::PerUnit, 50) };
        assert_eq!(p.estimated_units(&json!({"text": "a".repeat(2001)})), 3);
        assert_eq!(p.quote(&json!({"text": "hi"})), 50);
        let recipients = Price { units_from: Some("/to".into()), ..price(PriceBasis::PerUnit, 400) };
        assert_eq!(recipients.quote(&json!({"to": ["a@x.io", "b@x.io"]})), 800);
    }

    #[test]
    fn reported_cost_wins_but_never_exceeds_hold() {
        let p = price(PriceBasis::PerSuccess, 1_000);
        assert_eq!(p.settle(200, None, Some(640), false, 1_000), 640);
        assert_eq!(p.settle(200, None, Some(5_000), false, 1_000), 1_000);
        assert_eq!(p.settle(500, None, Some(640), false, 1_000), 0);
    }
}
