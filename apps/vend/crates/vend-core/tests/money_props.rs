//! Property-based invariants for the integer-µ$ markup math ([`vend_core::price`]).
//!
//! The money path is checked, integer-only, and rounds markup/fees UP so Vend is
//! never underpaid. These properties pin the conservation + rounding laws across a
//! wide random input space (no floats anywhere).

use proptest::prelude::*;

use vend_core::money::{price, FundingMode, MarkupPolicy, Micros};

/// Independent integer ceiling division, mirrored in the test so the property
/// checks `price` against a second implementation rather than itself.
fn ceil_div(num: i128, denom: i128) -> i64 {
    ((num + (denom - 1)) / denom) as i64
}

fn policy(markup_bps: u32, min_fee_micros: i64, platform_fee_bps: u32) -> MarkupPolicy {
    MarkupPolicy {
        markup_bps,
        min_fee_micros,
        platform_fee_bps,
        funding_mode: FundingMode::TenantReserve,
    }
}

proptest! {
    // Bounded so all intermediate i128 math stays well within i64 on narrowing:
    // cost <= 1e12 µ$ ($1M), markup_bps <= 1e6 (100x), fees/min bounded.
    #[test]
    fn markup_conserves_and_rounds_up(
        cost in 0i64..1_000_000_000_000,
        markup_bps in 0u32..1_000_000,
        min_fee in 0i64..100_000_000,
        platform_fee_bps in 0u32..10_000,
    ) {
        let c = price(Micros::new(cost), &policy(markup_bps, min_fee, platform_fee_bps))
            .expect("bounded inputs never overflow");

        // charge = cost + markup, exactly.
        prop_assert_eq!(c.charge_micros, c.provider_cost_micros + c.markup_micros);

        // markup splits EXACTLY into platform fee (on the margin) + tenant net.
        prop_assert_eq!(c.markup_micros, c.platform_fee_micros + c.tenant_net_micros);

        // end-user paid == cost + tenant take + platform take, exactly.
        prop_assert_eq!(
            c.charge_micros,
            c.provider_cost_micros + c.tenant_net_micros + c.platform_fee_micros
        );

        // The platform fee is on the MARGIN — never more than the whole markup.
        prop_assert!(c.platform_fee_micros <= c.markup_micros);
        prop_assert!(c.platform_fee_micros >= 0);
        prop_assert!(c.tenant_net_micros >= 0);

        // markup = max(ceil(cost * bps / 10_000), min_fee).
        let raw = ceil_div(cost as i128 * markup_bps as i128, 10_000);
        prop_assert_eq!(c.markup_micros, raw.max(min_fee));
        prop_assert!(c.markup_micros >= min_fee);
        prop_assert!(c.markup_micros >= raw);
    }

    // A zero provider cost still yields exactly the min-fee markup.
    #[test]
    fn zero_cost_charges_only_the_min_fee(
        markup_bps in 0u32..1_000_000,
        min_fee in 0i64..100_000_000,
    ) {
        let c = price(Micros::new(0), &policy(markup_bps, min_fee, 0)).unwrap();
        prop_assert_eq!(c.markup_micros, min_fee);
        prop_assert_eq!(c.charge_micros, min_fee);
    }

    // Negative provider cost is always rejected (never priced).
    #[test]
    fn negative_cost_is_always_rejected(cost in i64::MIN..0) {
        prop_assert!(price(Micros::new(cost), &policy(1_000, 0, 0)).is_err());
    }
}
