//! Charge models — the pricing math applied to aggregated usage.
//!
//! Faithful port of `app/services/charge_models/*`. Given a
//! [`PricingStructure`] (built from a [`Charge`]) and an [`AggregationResult`],
//! a calculator produces a [`ChargeModelResult`] carrying the amount (in the
//! currency's *main* unit as an exact [`Decimal`]), the per-unit amount, and
//! the model-specific `amount_details`.

mod custom;
mod dynamic;
mod graduated;
mod graduated_percentage;
mod package;
mod percentage;
mod prorated_graduated;
mod range_graduated;
mod standard;
mod volume;

pub use custom::CustomService;
pub use dynamic::DynamicService;
pub use graduated::GraduatedService;
pub use graduated_percentage::GraduatedPercentageService;
pub use package::PackageService;
pub use percentage::PercentageService;
pub use prorated_graduated::ProratedGraduatedService;
pub use standard::StandardService;
pub use volume::VolumeService;

use rago_core::enums::ChargeModel;
use rago_core::models::Charge;
use rago_core::money::Currency;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::aggregation::AggregationResult;
use crate::error::Result;

/// `ChargeModels::PricingStructure` — the immutable pricing configuration a
/// charge model operates on.
#[derive(Debug, Clone)]
pub struct PricingStructure {
    pub charge_model: ChargeModel,
    pub properties: Value,
    pub prorated: bool,
    pub accepts_target_wallet: bool,
    pub currency: Currency,
}

impl PricingStructure {
    /// `PricingStructure.from_charge` — currency comes from the plan.
    pub fn from_charge(charge: &Charge, plan_currency: Currency) -> Self {
        Self {
            charge_model: charge.charge_model,
            properties: charge.properties.clone(),
            prorated: charge.prorated,
            accepts_target_wallet: charge.accepts_target_wallet,
            currency: plan_currency,
        }
    }

    /// `PricingStructure#with(properties:)` — override properties (used for
    /// per-filter fees).
    pub fn with_properties(&self, properties: Value) -> Self {
        Self {
            properties,
            ..self.clone()
        }
    }
}

/// `ChargeModels::BaseService::Result` — the subset of fields the fee builder
/// consumes. `amount` and `unit_amount` are in the currency's main unit.
#[derive(Debug, Clone)]
pub struct ChargeModelResult {
    pub units: Decimal,
    pub current_usage_units: Option<Decimal>,
    pub full_units_number: Option<Decimal>,
    pub count: i64,
    pub amount: Decimal,
    pub unit_amount: Decimal,
    pub amount_details: Value,
    pub total_aggregated_units: Option<Decimal>,
}

/// A charge-model calculator (`ChargeModels::BaseService` subclasses).
pub trait ChargeModelCalculator {
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal;
    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal;
    fn amount_details(&self, _ps: &PricingStructure, _agg: &AggregationResult) -> Value {
        json!({})
    }
}

/// `ChargeModels::BaseService#apply` — assemble the result from the calculator.
pub fn apply<C: ChargeModelCalculator>(
    calc: &C,
    ps: &PricingStructure,
    agg: &AggregationResult,
) -> ChargeModelResult {
    ChargeModelResult {
        units: agg.aggregation,
        current_usage_units: agg.current_usage_units,
        full_units_number: agg.full_units_number,
        count: agg.count,
        amount: calc.compute_amount(ps, agg),
        unit_amount: calc.unit_amount(ps, agg),
        amount_details: calc.amount_details(ps, agg),
        total_aggregated_units: agg.total_aggregated_units,
    }
}

/// Whether prorated per-event aggregation data is available (Lago's
/// `has_aggregator` — the prorated graduated model needs it, else it falls back
/// to the plain graduated model).
fn has_prorated_aggregator(agg: &AggregationResult) -> bool {
    !agg.event_full_values.is_empty()
}

/// `ChargeModels::Factory.charge_model_class` — apply a single (non-grouped)
/// aggregation result with the calculator for `ps.charge_model`. All eight
/// Lago charge models are implemented.
fn apply_single(ps: &PricingStructure, agg: &AggregationResult) -> ChargeModelResult {
    match ps.charge_model {
        ChargeModel::Standard => apply(&StandardService, ps, agg),
        ChargeModel::Graduated => {
            if ps.prorated && has_prorated_aggregator(agg) {
                apply(&ProratedGraduatedService, ps, agg)
            } else {
                apply(&GraduatedService, ps, agg)
            }
        }
        ChargeModel::GraduatedPercentage => apply(&GraduatedPercentageService, ps, agg),
        ChargeModel::Package => apply(&PackageService, ps, agg),
        ChargeModel::Percentage => apply(&PercentageService, ps, agg),
        ChargeModel::Volume => apply(&VolumeService, ps, agg),
        ChargeModel::Custom => apply(&CustomService, ps, agg),
        ChargeModel::Dynamic => apply(&DynamicService, ps, agg),
    }
}

/// `ChargeModels::Factory.new_instance` — dispatch on the charge model, running
/// the grouped wrapper (`ChargeModels::GroupedService`) when pricing group keys
/// are configured and the aggregation is grouped.
///
/// All eight charge models are implemented (standard, graduated,
/// graduated_percentage, package, percentage, volume, custom, dynamic), plus
/// the prorated graduated variant and the grouped wrapper.
pub fn compute(ps: &PricingStructure, agg: &AggregationResult) -> Result<ChargeModelResult> {
    let pricing_group_keys = ps
        .properties
        .get("pricing_group_keys")
        .or_else(|| ps.properties.get("grouped_by"));
    let use_grouped = pricing_group_keys.is_some_and(|v| !v.is_null()) || ps.accepts_target_wallet;

    if use_grouped && !agg.aggregations.is_empty() {
        Ok(apply_grouped(ps, agg))
    } else {
        Ok(apply_single(ps, agg))
    }
}

/// `ChargeModels::Factory.in_advance_charge_model_class` — apply the in-advance
/// calculator (graduated is always non-prorated in the pay-in-advance path).
pub fn compute_in_advance(ps: &PricingStructure, agg: &AggregationResult) -> ChargeModelResult {
    let non_prorated = PricingStructure {
        prorated: false,
        ..ps.clone()
    };
    apply_single(&non_prorated, agg)
}

/// `ChargeModels::GroupedService#apply` — price each group independently, then
/// sum amounts and units. The per-group results are exposed in `amount_details`.
fn apply_grouped(ps: &PricingStructure, agg: &AggregationResult) -> ChargeModelResult {
    let grouped: Vec<ChargeModelResult> = agg
        .aggregations
        .iter()
        .map(|g| apply_single(ps, g))
        .collect();

    let amount: Decimal = grouped.iter().map(|r| r.amount).sum();
    let units: Decimal = grouped.iter().map(|r| r.units).sum();
    let count: i64 = grouped.iter().map(|r| r.count).sum();

    let group_details: Vec<Value> = agg
        .aggregations
        .iter()
        .zip(grouped.iter())
        .map(|(g, r)| {
            json!({
                "grouped_by": g.grouped_by,
                "units": r.units.to_string(),
                "amount": r.amount.to_string(),
            })
        })
        .collect();

    let unit_amount = if units.is_zero() {
        Decimal::ZERO
    } else {
        amount / units
    };

    ChargeModelResult {
        units,
        current_usage_units: Some(units),
        full_units_number: None,
        count,
        amount,
        unit_amount,
        amount_details: json!({ "grouped_results": group_details }),
        total_aggregated_units: Some(units),
    }
}
