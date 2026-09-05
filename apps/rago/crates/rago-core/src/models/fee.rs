use crate::enums::{FeeType, PaymentStatus};
use crate::money::Currency;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// `fees.applied_taxes` — a tax applied to a single fee. (Lago
/// `app/models/fee/applied_tax.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeAppliedTax {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub fee_id: Uuid,
    pub tax_id: Uuid,
    pub tax_code: String,
    pub tax_name: String,
    pub tax_rate: f64,
    pub tax_description: Option<String>,
    pub amount_cents: i64,
    pub precise_amount_cents: Decimal,
    pub amount_currency: String,
}

/// `fees` — a single billable line item. (Lago `app/models/fee.rb`.)
///
/// Only the columns the billing vertical touches are modelled; the rest of the
/// ~40 columns are catalogued in the port map for later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fee {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub billing_entity_id: Uuid,
    pub invoice_id: Option<Uuid>,
    pub subscription_id: Option<Uuid>,
    pub charge_id: Option<Uuid>,
    pub charge_filter_id: Option<Uuid>,
    pub fee_type: Option<FeeType>,
    pub invoiceable_type: Option<String>,
    pub invoiceable_id: Option<Uuid>,

    pub amount_cents: i64,
    pub amount_currency: String,
    pub precise_amount_cents: Decimal,
    pub unit_amount_cents: i64,
    pub precise_unit_amount: Decimal,

    pub taxes_amount_cents: i64,
    pub taxes_precise_amount_cents: Decimal,
    pub taxes_rate: f64,

    pub units: Decimal,
    pub total_aggregated_units: Option<Decimal>,
    pub events_count: Option<i32>,

    /// Running coupon discount allocated to this fee (`numeric(30,5)`).
    pub precise_coupons_amount_cents: Decimal,
    pub precise_credit_notes_amount_cents: Decimal,

    pub payment_status: PaymentStatus,
    pub properties: Value,
    pub amount_details: Value,
    pub grouped_by: Value,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    /// In-memory only: taxes applied to this fee (not a DB column).
    #[serde(default)]
    pub applied_taxes: Vec<FeeAppliedTax>,
}

impl Fee {
    /// `Fee#currency`.
    pub fn currency(&self) -> Currency {
        Currency::new(&self.amount_currency)
    }

    /// `Fee#sub_total_excluding_taxes_amount_cents` = `amount_cents - precise_coupons_amount_cents`.
    pub fn sub_total_excluding_taxes_amount_cents(&self) -> Decimal {
        Decimal::from(self.amount_cents) - self.precise_coupons_amount_cents
    }

    /// `Fee#sub_total_excluding_taxes_precise_amount_cents` = `precise_amount_cents - precise_coupons_amount_cents`.
    pub fn sub_total_excluding_taxes_precise_amount_cents(&self) -> Decimal {
        self.precise_amount_cents - self.precise_coupons_amount_cents
    }

    /// `Fee#compute_precise_credit_amount_cents` — the share of a coupon credit
    /// this fee absorbs, weighted by its remaining (uncouponed) amount.
    pub fn compute_precise_credit_amount_cents(
        &self,
        credit_amount: Decimal,
        base_amount_cents: Decimal,
    ) -> Decimal {
        if base_amount_cents.is_zero() {
            return Decimal::ZERO;
        }
        (credit_amount * (Decimal::from(self.amount_cents) - self.precise_coupons_amount_cents))
            / base_amount_cents
    }

    /// `Fee#charge?`
    pub fn is_charge(&self) -> bool {
        matches!(self.fee_type, Some(FeeType::Charge))
    }
}
