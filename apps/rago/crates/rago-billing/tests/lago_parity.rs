//! Phase 17 — Lago fixture-parity suite.
//!
//! Every expected value in this file is lifted verbatim from Lago's own RSpec
//! suite (`lago/api/spec/services/...`). Each test names the authoritative Lago
//! spec and reproduces its concrete input table, asserting Rago's engine yields
//! the EXACT same `rust_decimal` value Lago asserts. This locks Rago's money
//! math against Lago's numbers — it proves the rewrite is faithful, not merely
//! plausible.
//!
//! Sources:
//!   * charge_models/{standard,package,volume,graduated,graduated_percentage,
//!     percentage,dynamic,custom,prorated_graduated}_service_spec.rb
//!   * applied_coupons/amount_service_spec.rb
//!   * invoices/compute_amounts_from_fees_spec.rb (coupon-before-tax rollup)

use chrono::Utc;
use rago_billing::aggregation::AggregationResult;
use rago_billing::charge_models::{self, PricingStructure};
use rago_billing::invoices::{assemble_invoice, CouponApplication};
use rago_core::enums::{
    CouponExpiration, CouponFrequency, CouponStatus, CouponType, FeeType, InvoiceStatus,
    InvoiceType, PaymentStatus,
};
use rago_core::models::{AppliedCoupon, Coupon, Fee, Invoice, Tax};
use rago_core::money::Currency;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------- helpers

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

/// Ruby `BigDecimal#round(n)` — ROUND_HALF_UP.
fn round2(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}
fn round5(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(5, RoundingStrategy::MidpointAwayFromZero)
}
/// Ruby `BigDecimal#ceil(n)`.
fn ceil2(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::ToPositiveInfinity)
}

fn ps(model: rago_core::enums::ChargeModel, props: Value) -> PricingStructure {
    PricingStructure {
        charge_model: model,
        properties: props,
        prorated: false,
        accepts_target_wallet: false,
        currency: Currency::new("USD"),
    }
}

/// An aggregation result carrying only a scalar `aggregation`.
fn agg(value: Decimal) -> AggregationResult {
    AggregationResult {
        aggregation: value,
        ..Default::default()
    }
}

fn amount(model: rago_core::enums::ChargeModel, props: Value, a: &AggregationResult) -> Decimal {
    charge_models::compute(&ps(model, props), a).unwrap().amount
}

// ================================================================ STANDARD
// Source: charge_models/standard_service_spec.rb
mod standard {
    use super::*;
    use rago_core::enums::ChargeModel::Standard;

    #[test]
    fn amount_and_unit() {
        // amount "5.12345", aggregation 10 -> 51.2345, unit 5.12345
        let a = agg(dec!(10));
        let r = charge_models::compute(&ps(Standard, json!({"amount": "5.12345"})), &a).unwrap();
        assert_eq!(r.amount, dec!(51.2345));
        assert_eq!(r.unit_amount, dec!(5.12345));
    }

    #[test]
    fn unit_amount_uses_full_units_number() {
        // full_units_number 100 -> unit 0.512345
        let a = AggregationResult {
            aggregation: dec!(10),
            full_units_number: Some(dec!(100)),
            ..Default::default()
        };
        let r = charge_models::compute(&ps(Standard, json!({"amount": "5.12345"})), &a).unwrap();
        assert_eq!(r.unit_amount, dec!(0.512345));
    }
}

// ================================================================ PACKAGE
// Source: charge_models/package_service_spec.rb  (amount 100, package_size 10)
mod package {
    use super::*;
    use rago_core::enums::ChargeModel::Package;

    fn props(free_units: i64) -> Value {
        json!({"amount": "100", "package_size": 10, "free_units": free_units})
    }

    #[test]
    fn free_units_zero() {
        // aggregation 121 -> 1300, unit.round(2) 10.74
        let a = agg(dec!(121));
        let r = charge_models::compute(&ps(Package, props(0)), &a).unwrap();
        assert_eq!(r.amount, dec!(1300));
        assert_eq!(round2(r.unit_amount), dec!(10.74));
    }

    #[test]
    fn with_free_units() {
        // free_units 10 -> 1200, unit.round(2) 10.81
        let a = agg(dec!(121));
        let r = charge_models::compute(&ps(Package, props(10)), &a).unwrap();
        assert_eq!(r.amount, dec!(1200));
        assert_eq!(round2(r.unit_amount), dec!(10.81));
    }

    #[test]
    fn free_units_higher_than_value() {
        // free_units 200 -> 0
        let a = agg(dec!(121));
        let r = charge_models::compute(&ps(Package, props(200)), &a).unwrap();
        assert_eq!(r.amount, dec!(0));
        assert_eq!(r.unit_amount, dec!(0));
    }
}

// ================================================================ VOLUME
// Source: charge_models/volume_service_spec.rb
mod volume {
    use super::*;
    use rago_core::enums::ChargeModel::Volume;

    fn ranges() -> Value {
        json!({"volume_ranges": [
            {"from_value": 0, "to_value": 100, "per_unit_amount": "2", "flat_amount": "10"},
            {"from_value": 101, "to_value": 200, "per_unit_amount": "1", "flat_amount": "0"},
            {"from_value": 201, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "50"}
        ]})
    }

    #[test]
    fn tiered_points() {
        // (aggregation -> expected amount) straight from the spec `it` blocks.
        let cases = [
            (dec!(0), dec!(10)),
            (dec!(1), dec!(12)),
            (dec!(100), dec!(210)),
            (dec!(100.5), dec!(100.5)),
            (dec!(101), dec!(101)),
            (dec!(200), dec!(200)),
            (dec!(300), dec!(200)),
        ];
        for (input, expected) in cases {
            assert_eq!(
                amount(Volume, ranges(), &agg(input)),
                expected,
                "volume agg {input}"
            );
        }
    }

    #[test]
    fn first_tier_flat_zero_returns_zero() {
        let props = json!({"volume_ranges": [
            {"from_value": 0, "to_value": 1, "per_unit_amount": "0", "flat_amount": "0"},
            {"from_value": 2, "to_value": 100, "per_unit_amount": "2", "flat_amount": "10"},
            {"from_value": 101, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "50"}
        ]});
        assert_eq!(amount(Volume, props, &agg(dec!(0))), dec!(0));
    }

    #[test]
    fn prorated_third_tier() {
        // prorated, aggregation 198.6, full_units_number 300 -> 149.3, unit.round(2) 0.50
        let a = AggregationResult {
            aggregation: dec!(198.6),
            full_units_number: Some(dec!(300)),
            ..Default::default()
        };
        let mut structure = ps(Volume, ranges());
        structure.prorated = true;
        let r = charge_models::compute(&structure, &a).unwrap();
        assert_eq!(r.amount, dec!(149.3));
        assert_eq!(round2(r.unit_amount), dec!(0.50));
    }
}

// ================================================================ GRADUATED
// Source: charge_models/graduated_service_spec.rb
mod graduated {
    use super::*;
    use rago_core::enums::ChargeModel::Graduated;

    fn ranges() -> Value {
        json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 10, "per_unit_amount": "10", "flat_amount": "2"},
            {"from_value": 11, "to_value": 20, "per_unit_amount": "5", "flat_amount": "3"},
            {"from_value": 21, "to_value": null, "per_unit_amount": "5", "flat_amount": "3"}
        ]})
    }

    #[test]
    fn tiered_points() {
        let cases = [
            (dec!(0), dec!(2)),
            (dec!(1), dec!(12)),
            (dec!(10), dec!(102)),
            (dec!(11), dec!(110)),
            (dec!(12), dec!(115)),
            (dec!(21), dec!(163)),
        ];
        for (input, expected) in cases {
            assert_eq!(
                amount(Graduated, ranges(), &agg(input)),
                expected,
                "graduated agg {input}"
            );
        }
    }

    #[test]
    fn unit_amount_12() {
        // aggregation 12 -> unit_amount.round(5) 9.58333
        let r = charge_models::compute(&ps(Graduated, ranges()), &agg(dec!(12))).unwrap();
        assert_eq!(round5(r.unit_amount), dec!(9.58333));
    }

    #[test]
    fn decimal_adjacent_ranges() {
        let props = json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 0.1, "per_unit_amount": "10", "flat_amount": "0"},
            {"from_value": 0.1, "to_value": 1, "per_unit_amount": "5", "flat_amount": "0"},
            {"from_value": 1, "to_value": null, "per_unit_amount": "2", "flat_amount": "0"}
        ]});
        assert_eq!(
            amount(Graduated, props.clone(), &agg(dec!(0.05))),
            dec!(0.5)
        );
        // spans two tiers: 0.1*10 + 0.4*5 = 3.0
        assert_eq!(amount(Graduated, props, &agg(dec!(0.5))), dec!(3.0));
    }

    #[test]
    fn first_tier_flat_zero_returns_zero() {
        let props = json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 0.01, "per_unit_amount": "0", "flat_amount": "0"},
            {"from_value": 0.01, "to_value": 10, "per_unit_amount": "10", "flat_amount": "2"},
            {"from_value": 11, "to_value": null, "per_unit_amount": "5", "flat_amount": "3"}
        ]});
        assert_eq!(amount(Graduated, props, &agg(dec!(0))), dec!(0));
    }
}

// ======================================================= GRADUATED PERCENTAGE
// Source: charge_models/graduated_percentage_service_spec.rb
mod graduated_percentage {
    use super::*;
    use rago_core::enums::ChargeModel::GraduatedPercentage;

    fn ranges() -> Value {
        json!({"graduated_percentage_ranges": [
            {"from_value": 0, "to_value": 10, "flat_amount": "200", "rate": "1"},
            {"from_value": 11, "to_value": 20, "flat_amount": "300", "rate": "2"},
            {"from_value": 21, "to_value": null, "flat_amount": "400", "rate": "3"}
        ]})
    }

    #[test]
    fn tiered_points() {
        let cases = [
            (dec!(0), dec!(200)),
            (dec!(1), dec!(200.01)),
            (dec!(10), dec!(200.1)),
            (dec!(11), dec!(500.12)),
            (dec!(12), dec!(500.14)),
            (dec!(21), dec!(900.33)),
        ];
        for (input, expected) in cases {
            assert_eq!(
                amount(GraduatedPercentage, ranges(), &agg(input)),
                expected,
                "gp agg {input}"
            );
        }
    }

    #[test]
    fn first_tier_flat_zero_returns_zero() {
        let props = json!({"graduated_percentage_ranges": [
            {"from_value": 0, "to_value": 0.01, "flat_amount": "0", "rate": "0"},
            {"from_value": 0.01, "to_value": 10, "flat_amount": "200", "rate": "1"},
            {"from_value": 11, "to_value": null, "flat_amount": "300", "rate": "2"}
        ]});
        assert_eq!(amount(GraduatedPercentage, props, &agg(dec!(0))), dec!(0));
    }
}

// ================================================================ PERCENTAGE
// Source: charge_models/percentage_service_spec.rb
mod percentage {
    use super::*;
    use rago_core::enums::ChargeModel::Percentage;

    /// Build the aggregation used in the main percentage spec block.
    fn agg_with(
        aggregation: Decimal,
        count: i64,
        running_total: Vec<Decimal>,
    ) -> AggregationResult {
        AggregationResult {
            aggregation,
            count,
            running_total,
            ..Default::default()
        }
    }

    fn props(rate: &str, fixed: Option<&str>, fupe: Option<i64>, fupta: Option<&str>) -> Value {
        json!({
            "rate": rate,
            "fixed_amount": fixed,
            "free_units_per_events": fupe,
            "free_units_per_total_aggregation": fupta,
            "per_transaction_max_amount": null,
            "per_transaction_min_amount": null,
        })
    }

    // running_total [50,150,400]; count 4 across the standard cases.
    fn rt() -> Vec<Decimal> {
        vec![dec!(50), dec!(150), dec!(400)]
    }

    #[test]
    fn aggregation_zero() {
        let a = agg_with(dec!(0), 4, rt());
        assert_eq!(
            amount(
                Percentage,
                props("1.3", Some("2.0"), Some(3), Some("250.0")),
                &a
            ),
            dec!(0)
        );
    }

    #[test]
    fn standard_case() {
        // rate 1.3, fixed 2.0, fupe 3, fupta 250 -> 11.15, unit 0.0139375
        let a = agg_with(dec!(800), 4, rt());
        let r = charge_models::compute(
            &ps(
                Percentage,
                props("1.3", Some("2.0"), Some(3), Some("250.0")),
            ),
            &a,
        )
        .unwrap();
        assert_eq!(r.amount, dec!(11.15));
        assert_eq!(r.unit_amount, dec!(0.0139375));
    }

    #[test]
    fn small_units() {
        // running_total [], fixed nil, aggregation 4, rate 2.9 -> 0.116, unit 0.029
        let a = agg_with(dec!(4), 4, vec![]);
        let r =
            charge_models::compute(&ps(Percentage, props("2.9", None, None, None)), &a).unwrap();
        assert_eq!(r.amount, dec!(0.116));
        assert_eq!(r.unit_amount, dec!(0.029));
    }

    #[test]
    fn rate_zero() {
        // rate 0, running_total [], fupe nil, fupta nil, fixed 2.0 -> 8, unit 0.01
        let a = agg_with(dec!(800), 4, vec![]);
        let r = charge_models::compute(&ps(Percentage, props("0", Some("2.0"), None, None)), &a)
            .unwrap();
        assert_eq!(r.amount, dec!(8));
        assert_eq!(r.unit_amount, dec!(0.01));
    }

    #[test]
    fn free_units_per_events_nil() {
        let a = agg_with(dec!(800), 4, rt());
        assert_eq!(
            amount(
                Percentage,
                props("1.3", Some("2.0"), None, Some("250.0")),
                &a
            ),
            dec!(11.15)
        );
    }

    #[test]
    fn free_units_per_total_aggregation_nil() {
        let a = agg_with(dec!(800), 4, rt());
        assert_eq!(
            amount(Percentage, props("1.3", Some("2.0"), Some(3), None), &a),
            dec!(7.2)
        );
    }

    #[test]
    fn no_free_units() {
        // fupta nil, fupe nil, running_total [] -> 18.4
        let a = agg_with(dec!(800), 4, vec![]);
        assert_eq!(
            amount(Percentage, props("1.3", Some("2.0"), None, None), &a),
            dec!(18.4)
        );
    }

    #[test]
    fn fupta_above_last_running_total() {
        // fupta 500 -> 7.2
        let a = agg_with(dec!(800), 4, rt());
        assert_eq!(
            amount(
                Percentage,
                props("1.3", Some("2.0"), Some(3), Some("500.0")),
                &a
            ),
            dec!(7.2)
        );
    }

    #[test]
    fn free_units_exceed_event_count() {
        // fupe 5, fupta nil, aggregation 400 -> 0
        let a = agg_with(dec!(400), 4, rt());
        assert_eq!(
            amount(Percentage, props("1.3", Some("2.0"), Some(5), None), &a),
            dec!(0)
        );
    }

    // --- per-transaction min/max (Lago gates on premium; Rago always applies).
    // These reproduce the PREMIUM fixtures — the numbers Rago is built to match.
    fn minmax_props(rate: &str, fixed: &str, fupe: Option<i64>, fupta: &str) -> Value {
        json!({
            "rate": rate,
            "fixed_amount": fixed,
            "free_units_per_events": fupe,
            "free_units_per_total_aggregation": fupta,
            "per_transaction_max_amount": "12",
            "per_transaction_min_amount": "1.75",
        })
    }
    fn minmax_agg(fupta_free: Option<Decimal>) -> AggregationResult {
        let _ = fupta_free;
        AggregationResult {
            aggregation: dec!(10090),
            count: 3,
            per_event_values: vec![dec!(10), dec!(80), dec!(10000)],
            running_total: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn min_max_premium_base() {
        // rate 2.99, fixed 0, no free -> 1.75 + 2.392 + 12 = 16.142
        let a = minmax_agg(None);
        assert_eq!(
            amount(Percentage, minmax_props("2.99", "0", None, "0"), &a),
            dec!(16.142)
        );
    }

    #[test]
    fn min_max_premium_with_fixed() {
        // fixed 2.0 -> 2.299 + 4.392 + 12 = 18.691
        let a = minmax_agg(None);
        assert_eq!(
            amount(Percentage, minmax_props("2.99", "2.0", None, "0"), &a),
            dec!(18.691)
        );
    }

    #[test]
    fn min_max_premium_free_units_per_events() {
        // fupe 2 -> only the 10000 event pays: 12
        let a = minmax_agg(None);
        assert_eq!(
            amount(Percentage, minmax_props("2.99", "0", Some(2), "0"), &a),
            dec!(12)
        );
    }

    #[test]
    fn min_max_premium_free_units_per_total_aggregation() {
        // fupta 30 -> 1.794 + 12 = 13.794
        let a = minmax_agg(None);
        assert_eq!(
            amount(Percentage, minmax_props("2.99", "0", None, "30"), &a),
            dec!(13.794)
        );
    }

    #[test]
    fn min_max_premium_both_free() {
        // fupe 3, fupta 10000 -> (90) * 0.0299 = 2.691
        let a = minmax_agg(None);
        assert_eq!(
            amount(Percentage, minmax_props("2.99", "0", Some(3), "10000"), &a),
            dec!(2.691)
        );
    }
}

// ================================================================ DYNAMIC
// Source: charge_models/dynamic_service_spec.rb
mod dynamic {
    use super::*;
    use rago_core::enums::ChargeModel::Dynamic;

    #[test]
    fn amount_and_unit() {
        // aggregation 20, precise_total_amount_cents 40.2 -> 0.402, unit 0.0201
        let a = AggregationResult {
            aggregation: dec!(20),
            precise_total_amount_cents: Some(dec!(40.2)),
            ..Default::default()
        };
        let r = charge_models::compute(&ps(Dynamic, json!({})), &a).unwrap();
        assert_eq!(r.amount, dec!(0.402));
        assert_eq!(r.unit_amount, dec!(0.0201));
    }

    #[test]
    fn zero_aggregation() {
        let a = AggregationResult {
            aggregation: dec!(0),
            precise_total_amount_cents: Some(dec!(40.2)),
            ..Default::default()
        };
        let r = charge_models::compute(&ps(Dynamic, json!({})), &a).unwrap();
        assert_eq!(r.amount, dec!(0));
        assert_eq!(r.unit_amount, dec!(0));
    }
}

// ================================================================ CUSTOM
// Source: charge_models/custom_service_spec.rb
mod custom {
    use super::*;
    use rago_core::enums::ChargeModel::Custom;

    #[test]
    fn amount_and_unit() {
        // custom_aggregation amount 20, full_units_number 10 -> amount 20, unit 2
        let a = AggregationResult {
            aggregation: dec!(10),
            full_units_number: Some(dec!(10)),
            custom_amount: Some(dec!(20)),
            ..Default::default()
        };
        let r = charge_models::compute(&ps(Custom, json!({})), &a).unwrap();
        assert_eq!(r.amount, dec!(20));
        assert_eq!(r.unit_amount, dec!(2));
    }
}

// ========================================================= PRORATED GRADUATED
// Source: charge_models/prorated_graduated_service_spec.rb
mod prorated_graduated {
    use super::*;
    use rago_core::enums::ChargeModel::Graduated;

    fn ranges_two() -> Value {
        json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 5, "per_unit_amount": "10", "flat_amount": "100"},
            {"from_value": 6, "to_value": null, "per_unit_amount": "5", "flat_amount": "50"}
        ]})
    }

    fn prorated_agg(
        aggregation: Decimal,
        full_units_number: Decimal,
        full: Vec<Decimal>,
        prorated: Vec<Decimal>,
    ) -> AggregationResult {
        AggregationResult {
            aggregation,
            full_units_number: Some(full_units_number),
            current_usage_units: Some(full_units_number),
            event_full_values: full,
            event_prorated_values: prorated,
            ..Default::default()
        }
    }

    fn compute(props: Value, a: &AggregationResult) -> Decimal {
        let mut structure = ps(Graduated, props);
        structure.prorated = true;
        charge_models::compute(&structure, a).unwrap().amount
    }

    #[test]
    fn main_case() {
        // full [5,5,10,-6] prorated [3.5,2.66667,2,-2.2], agg 5.96667, full 14 -> 197.33
        let a = prorated_agg(
            dec!(5.96667),
            dec!(14),
            vec![dec!(5), dec!(5), dec!(10), dec!(-6)],
            vec![dec!(3.5), dec!(2.66667), dec!(2), dec!(-2.2)],
        );
        assert_eq!(round2(compute(ranges_two(), &a)), dec!(197.33));
    }

    #[test]
    fn zero_usage_flat_from_first_tier() {
        let a = prorated_agg(dec!(0), dec!(0), vec![], vec![]);
        assert_eq!(compute(ranges_two(), &a), dec!(100));
    }

    #[test]
    fn event_cannot_be_fully_placed() {
        // full [2,5,10,-6] prorated [1.4,2.66667,2,-2.2], agg 3.86667, full 11 -> 184.33
        let a = prorated_agg(
            dec!(3.86667),
            dec!(11),
            vec![dec!(2), dec!(5), dec!(10), dec!(-6)],
            vec![dec!(1.4), dec!(2.66667), dec!(2), dec!(-2.2)],
        );
        assert_eq!(round2(compute(ranges_two(), &a)), dec!(184.33));
    }

    #[test]
    fn one_range_used() {
        // full [1] prorated [0.7], agg 0.7, full 1 -> 107
        let a = prorated_agg(dec!(0.7), dec!(1), vec![dec!(1)], vec![dec!(0.7)]);
        assert_eq!(round2(compute(ranges_two(), &a)), dec!(107));
    }

    #[test]
    fn one_range_first_unit_fully_covers_first_range() {
        // ranges [0,1]@10 flat100 ; [2,nil]@5 flat50, full [1] prorated [0.7] -> 107
        let props = json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 1, "per_unit_amount": "10", "flat_amount": "100"},
            {"from_value": 2, "to_value": null, "per_unit_amount": "5", "flat_amount": "50"}
        ]});
        let a = prorated_agg(dec!(0.7), dec!(1), vec![dec!(1)], vec![dec!(0.7)]);
        assert_eq!(round2(compute(props, &a)), dec!(107));
    }

    #[test]
    fn three_ranges_one_overflow() {
        // full [2,5,10,-6,4,60] prorated [1.4,2.5,2,-2.2,0.667,2], agg 6.36, full 75
        // -> amount.ceil(2) 191.34
        let props = json!({"graduated_ranges": [
            {"from_value": 0, "to_value": 5, "per_unit_amount": "10", "flat_amount": "100"},
            {"from_value": 6, "to_value": 15, "per_unit_amount": "5", "flat_amount": "50"},
            {"from_value": 16, "to_value": null, "per_unit_amount": "2", "flat_amount": "0"}
        ]});
        let a = prorated_agg(
            dec!(6.36),
            dec!(75),
            vec![dec!(2), dec!(5), dec!(10), dec!(-6), dec!(4), dec!(60)],
            vec![
                dec!(1.4),
                dec!(2.5),
                dec!(2),
                dec!(-2.2),
                dec!(0.667),
                dec!(2),
            ],
        );
        assert_eq!(ceil2(compute(props, &a)), dec!(191.34));
    }
}

// ============================================ COUPON AMOUNT (before tax)
// Source: applied_coupons/amount_service_spec.rb — reproduced through Rago's
// real coupon pipeline (`assemble_invoice`), where the coupon base is the
// invoice sub-total (= the single fee's amount).
mod coupon_amount {
    use super::*;

    fn coupon(coupon_type: CouponType) -> Coupon {
        Coupon {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            name: "c".into(),
            code: "c".into(),
            status: CouponStatus::Active,
            coupon_type,
            amount_cents: Some(12),
            amount_currency: Some("USD".into()),
            percentage_rate: None,
            frequency: CouponFrequency::Once,
            frequency_duration: None,
            expiration: CouponExpiration::NoExpiration,
            expiration_at: None,
            reusable: false,
            limited_plans: false,
            limited_billable_metrics: false,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        }
    }

    fn applied(
        frequency: CouponFrequency,
        amount_cents: Option<i64>,
        pct: Option<Decimal>,
    ) -> AppliedCoupon {
        AppliedCoupon {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            coupon_id: Uuid::nil(),
            customer_id: Uuid::nil(),
            status: CouponStatus::Active,
            amount_cents,
            amount_currency: Some("USD".into()),
            percentage_rate: pct,
            frequency,
            frequency_duration: match frequency {
                CouponFrequency::Recurring => Some(3),
                _ => Some(0),
            },
            frequency_duration_remaining: match frequency {
                CouponFrequency::Recurring => Some(3),
                _ => Some(0),
            },
            created_at: now(),
            updated_at: now(),
            terminated_at: None,
        }
    }

    /// Assemble a single-fee invoice (no tax) and return the coupon credit cents.
    fn credit(base_cents: i64, coupon: &Coupon, ac: &AppliedCoupon) -> i64 {
        let mut invoice = blank_invoice();
        let mut fees = vec![mk_fee(base_cents, Decimal::ZERO)];
        let apps = [CouponApplication::new(ac, coupon)];
        assemble_invoice(&mut invoice, &mut fees, &[], &apps);
        invoice.coupons_amount_cents
    }

    #[test]
    fn fixed_amount() {
        let c = coupon(CouponType::FixedAmount);
        let ac = applied(CouponFrequency::Once, Some(12), None);
        assert_eq!(credit(300, &c, &ac), 12); // base 300, fixed 12
        assert_eq!(credit(6, &c, &ac), 6); // base 6 < 12 -> capped at 6
    }

    #[test]
    fn base_zero() {
        let c = coupon(CouponType::FixedAmount);
        let ac = applied(CouponFrequency::Once, Some(12), None);
        assert_eq!(credit(0, &c, &ac), 0);
    }

    #[test]
    fn percentage() {
        // coupon percentage, applied percentage_rate 20% on base 300 -> 60
        let c = coupon(CouponType::Percentage);
        let ac = applied(CouponFrequency::Once, None, Some(dec!(20)));
        assert_eq!(credit(300, &c, &ac), 60);
    }

    #[test]
    fn recurring_fixed() {
        let c = coupon(CouponType::FixedAmount);
        let ac = applied(CouponFrequency::Recurring, Some(12), None);
        assert_eq!(credit(300, &c, &ac), 12);
        assert_eq!(credit(6, &c, &ac), 6);
    }

    #[test]
    fn forever_fixed() {
        let c = coupon(CouponType::FixedAmount);
        let ac = applied(CouponFrequency::Forever, Some(12), None);
        assert_eq!(credit(300, &c, &ac), 12);
        assert_eq!(credit(6, &c, &ac), 6);
    }

    #[test]
    fn recurring_percentage() {
        let c = coupon(CouponType::Percentage);
        let ac = applied(CouponFrequency::Recurring, None, Some(dec!(20)));
        assert_eq!(credit(300, &c, &ac), 60);
    }
}

// ==================================== INVOICE: coupon-before-tax rollup
// Source: invoices/compute_amounts_from_fees_spec.rb
// fee1 amount 151 (untouched); fee2 amount 379 with a 100-cent coupon applied
// only to it; tax1 10% + tax2 20% on both fees; one 100-cent coupon credit.
mod invoice_rollup {
    use super::*;

    fn tax(rate: f64) -> Tax {
        Tax {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            name: format!("t{rate}"),
            code: format!("t{rate}"),
            description: None,
            rate,
            applied_to_organization: true,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn coupon_fixed_100() -> (Coupon, AppliedCoupon) {
        let coupon = Coupon {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            name: "c".into(),
            code: "c".into(),
            status: CouponStatus::Active,
            coupon_type: CouponType::FixedAmount,
            amount_cents: Some(100),
            amount_currency: Some("USD".into()),
            percentage_rate: None,
            frequency: CouponFrequency::Once,
            frequency_duration: Some(0),
            expiration: CouponExpiration::NoExpiration,
            expiration_at: None,
            reusable: false,
            limited_plans: false,
            limited_billable_metrics: false,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
        };
        let applied = AppliedCoupon {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            coupon_id: coupon.id,
            customer_id: Uuid::nil(),
            status: CouponStatus::Active,
            amount_cents: Some(100),
            amount_currency: Some("USD".into()),
            percentage_rate: None,
            frequency: CouponFrequency::Once,
            frequency_duration: Some(0),
            frequency_duration_remaining: Some(0),
            created_at: now(),
            updated_at: now(),
            terminated_at: None,
        };
        (coupon, applied)
    }

    #[test]
    fn coupon_before_tax_rollup() {
        let mut invoice = blank_invoice();
        let fee1 = mk_fee(151, Decimal::ZERO);
        let fee2 = mk_fee(379, Decimal::ZERO);
        let fee2_id = fee2.id;
        let mut fees = vec![fee1, fee2];

        let (coupon, applied) = coupon_fixed_100();
        // Coupon limited to fee2 only (reproduces the 100-cent coupon on fee2).
        let app = CouponApplication {
            applied_coupon: &applied,
            coupon: &coupon,
            limited_to_fee_ids: Some(vec![fee2_id]),
        };
        let taxes = [tax(10.0), tax(20.0)];
        assemble_invoice(&mut invoice, &mut fees, &taxes, &[app]);

        // fee-level taxes on the coupon-reduced base
        let f1 = fees.iter().find(|f| f.id != fee2_id).unwrap();
        let f2 = fees.iter().find(|f| f.id == fee2_id).unwrap();
        assert_eq!(f1.taxes_amount_cents, 45, "fee1 151 * 30%");
        assert_eq!(f2.taxes_amount_cents, 84, "fee2 (379-100) * 30%");

        // invoice rollup
        assert_eq!(invoice.fees_amount_cents, 530);
        assert_eq!(invoice.coupons_amount_cents, 100);
        assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 430);
        assert_eq!(invoice.taxes_amount_cents, 129);
        assert_eq!(invoice.sub_total_including_taxes_amount_cents, 559);
        assert_eq!(invoice.total_amount_cents, 559);
    }
}

// ---------------------------------------------------------------- fee/invoice builders

fn mk_fee(amount_cents: i64, precise_coupons: Decimal) -> Fee {
    Fee {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        invoice_id: None,
        subscription_id: None,
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(FeeType::Charge),
        invoiceable_type: Some("Charge".to_string()),
        invoiceable_id: None,
        amount_cents,
        amount_currency: "USD".into(),
        precise_amount_cents: Decimal::from(amount_cents),
        unit_amount_cents: amount_cents,
        precise_unit_amount: Decimal::from(amount_cents),
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: Decimal::ZERO,
        taxes_rate: 0.0,
        units: dec!(1),
        total_aggregated_units: Some(dec!(1)),
        events_count: Some(1),
        precise_coupons_amount_cents: precise_coupons,
        precise_credit_notes_amount_cents: Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({}),
        grouped_by: json!({}),
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

fn blank_invoice() -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        customer_id: None,
        number: "INV-1".into(),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Draft,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: None,
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}
