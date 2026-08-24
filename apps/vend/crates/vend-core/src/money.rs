//! Integer-µ$ money math (ADR-0001) and the markup/fee value types.
//!
//! All arithmetic is checked, integer-only, and rounds markup/fees UP (`ceil`)
//! so Vend is never underpaid by rounding. There are no `f32`/`f64` conversions
//! on the money path. The canonical relationships:
//!
//! ```text
//! markup_micros = max(ceil(provider_cost * markup_bps / 10_000), min_fee_micros)
//! charge        = provider_cost + markup
//! platform_fee  = ceil(markup * platform_fee_bps / 10_000)   // fee on the MARGIN
//! tenant_net    = markup - platform_fee
//! ```

use serde::{Deserialize, Serialize};

use crate::error::VendError;

/// Microdollars per dollar. `1 USD = 1_000_000 µ$`.
pub const MICROS_PER_USD: i64 = 1_000_000;

/// Basis-points denominator. `10_000 bps = 100%`.
pub const BPS_DENOM: i128 = 10_000;

/// A signed integer microdollar amount, serialized as a bare `i64` (never float).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Micros(i64);

impl Micros {
    /// Construct from a raw microdollar count.
    pub const fn new(v: i64) -> Self {
        Micros(v)
    }
    /// The additive identity (`0 µ$`).
    pub const fn zero() -> Self {
        Micros(0)
    }
    /// The raw microdollar count.
    pub const fn get(self) -> i64 {
        self.0
    }
    /// `true` iff strictly greater than zero.
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }
    /// `true` iff strictly less than zero.
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
    /// Checked addition; [`VendError::Overflow`] instead of wrapping.
    pub fn checked_add(self, other: Micros) -> Result<Micros, VendError> {
        self.0
            .checked_add(other.0)
            .map(Micros)
            .ok_or(VendError::Overflow)
    }
    /// Checked subtraction; [`VendError::Overflow`] instead of wrapping.
    pub fn checked_sub(self, other: Micros) -> Result<Micros, VendError> {
        self.0
            .checked_sub(other.0)
            .map(Micros)
            .ok_or(VendError::Overflow)
    }
    /// Human-readable USD rendering (`$22.99`), display-only, truncating.
    pub fn format_usd(self) -> String {
        let neg = self.0 < 0;
        let abs = (self.0 as i128).unsigned_abs();
        let per = MICROS_PER_USD as u128;
        format!(
            "{}${}.{:02}",
            if neg { "-" } else { "" },
            abs / per,
            (abs % per) / 10_000
        )
    }
}

impl std::fmt::Debug for Micros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Micros({})", self.0)
    }
}

/// Ceiling division for a non-negative numerator over a positive denominator,
/// computed in `i128` then narrowed back to `i64`.
fn ceil_div_to_i64(num: i128, denom: i128) -> Result<i64, VendError> {
    debug_assert!(denom > 0);
    if num < 0 {
        return Err(VendError::InvalidAmount);
    }
    let q = (num + (denom - 1)) / denom;
    i64::try_from(q).map_err(|_| VendError::Overflow)
}

/// How a request is funded. Drives which Mint account backs the hold.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundingMode {
    /// Charged against the tenant's platform-held reserve.
    TenantReserve,
    /// Charged against a specific end-user's reserve sub-account.
    EndUserReserve,
    /// Bring-your-own-key: provider billed directly, Vend takes only the markup.
    Byok,
}

/// The reseller's pricing knobs for an app. Basis points over provider cost with
/// an absolute per-call floor, plus the platform's cut of the tenant's margin.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MarkupPolicy {
    /// Markup in basis points applied to raw provider cost.
    pub markup_bps: u32,
    /// Absolute minimum markup per call, in µ$.
    pub min_fee_micros: i64,
    /// Platform's cut of the tenant's *margin*, in basis points.
    pub platform_fee_bps: u32,
    /// How the charge is funded.
    pub funding_mode: FundingMode,
}

impl MarkupPolicy {
    /// A zero-markup, zero-fee, tenant-reserve policy (useful as a test default).
    pub const fn passthrough() -> Self {
        MarkupPolicy {
            markup_bps: 0,
            min_fee_micros: 0,
            platform_fee_bps: 0,
            funding_mode: FundingMode::TenantReserve,
        }
    }
}

/// Per-key daily spend caps enforced by the atomic check-and-reserve gate.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpendCap {
    /// Cap on total `charge_micros` for the key over the UTC day.
    pub daily_cap_micros: Option<i64>,
    /// Cap on total `charge_micros` per end-user over the UTC day.
    pub per_end_user_daily_cap_micros: Option<i64>,
}

impl SpendCap {
    /// An uncapped policy.
    pub const fn unlimited() -> Self {
        SpendCap {
            daily_cap_micros: None,
            per_end_user_daily_cap_micros: None,
        }
    }
}

/// The full, immutable breakdown of one priced call. Computed once via
/// [`price`]; every field is derived by checked integer math.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Charge {
    /// Raw upstream provider cost.
    pub provider_cost_micros: i64,
    /// Reseller markup after applying `min_fee_micros`.
    pub markup_micros: i64,
    /// What the end-user is charged: `provider_cost + markup`.
    pub charge_micros: i64,
    /// Platform's fee on the tenant's margin.
    pub platform_fee_micros: i64,
    /// What the tenant keeps: `markup - platform_fee`.
    pub tenant_net_micros: i64,
}

/// Price a single call from raw provider cost + policy, returning the full,
/// conservation-checked [`Charge`] breakdown.
pub fn price(provider_cost: Micros, policy: &MarkupPolicy) -> Result<Charge, VendError> {
    if provider_cost.is_negative() {
        return Err(VendError::InvalidAmount);
    }
    let cost = provider_cost.get();

    // markup = max(ceil(cost * markup_bps / 10_000), min_fee_micros)
    let raw_markup = ceil_div_to_i64(cost as i128 * policy.markup_bps as i128, BPS_DENOM)?;
    let markup = raw_markup.max(policy.min_fee_micros.max(0));

    // charge = cost + markup
    let charge = Micros::new(cost).checked_add(Micros::new(markup))?.get();

    // platform_fee = ceil(markup * platform_fee_bps / 10_000)  (fee on the margin)
    let platform_fee =
        ceil_div_to_i64(markup as i128 * policy.platform_fee_bps as i128, BPS_DENOM)?;

    // tenant_net = markup - platform_fee
    let tenant_net = Micros::new(markup)
        .checked_sub(Micros::new(platform_fee))?
        .get();

    Ok(Charge {
        provider_cost_micros: cost,
        markup_micros: markup,
        charge_micros: charge,
        platform_fee_micros: platform_fee,
        tenant_net_micros: tenant_net,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(markup_bps: u32, min_fee: i64, platform_bps: u32) -> MarkupPolicy {
        MarkupPolicy {
            markup_bps,
            min_fee_micros: min_fee,
            platform_fee_bps: platform_bps,
            funding_mode: FundingMode::TenantReserve,
        }
    }

    #[test]
    fn markup_rounds_up_and_respects_min_fee() {
        // cost $1.00, 30% markup => 300_000; min fee 500_000 wins.
        let c = price(Micros::new(1_000_000), &policy(3_000, 500_000, 0)).unwrap();
        assert_eq!(c.markup_micros, 500_000);
        assert_eq!(c.charge_micros, 1_500_000);
    }

    #[test]
    fn markup_ceils_fractional_micro() {
        // cost 1 µ$, 1 bps => 0.0001 µ$ -> ceil => 1 µ$.
        let c = price(Micros::new(1), &policy(1, 0, 0)).unwrap();
        assert_eq!(c.markup_micros, 1);
    }

    #[test]
    fn platform_fee_is_on_margin_and_conserves() {
        // cost $2.00, 50% markup => 1_000_000 markup; 20% platform fee => 200_000.
        let c = price(Micros::new(2_000_000), &policy(5_000, 0, 2_000)).unwrap();
        assert_eq!(c.markup_micros, 1_000_000);
        assert_eq!(c.platform_fee_micros, 200_000);
        assert_eq!(c.tenant_net_micros, 800_000);
        // conservation: markup == platform_fee + tenant_net
        assert_eq!(c.markup_micros, c.platform_fee_micros + c.tenant_net_micros);
        // charge == cost + markup
        assert_eq!(c.charge_micros, c.provider_cost_micros + c.markup_micros);
    }

    #[test]
    fn negative_cost_is_rejected() {
        assert_eq!(
            price(Micros::new(-1), &policy(0, 0, 0)),
            Err(VendError::InvalidAmount)
        );
    }

    #[test]
    fn serializes_as_bare_i64() {
        assert_eq!(
            serde_json::to_string(&Micros::new(22_990_000)).unwrap(),
            "22990000"
        );
    }
}
