use crate::enums::{CouponExpiration, CouponFrequency, CouponStatus, CouponType};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `coupons` — a discount definition. (Lago `app/models/coupon.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coupon {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub code: String,
    pub status: CouponStatus,
    pub coupon_type: CouponType,
    pub amount_cents: Option<i64>,
    pub amount_currency: Option<String>,
    pub percentage_rate: Option<Decimal>,
    pub frequency: CouponFrequency,
    pub frequency_duration: Option<i32>,
    pub expiration: CouponExpiration,
    pub expiration_at: Option<DateTime<Utc>>,
    pub reusable: bool,
    pub limited_plans: bool,
    pub limited_billable_metrics: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Coupon {
    pub fn is_percentage(&self) -> bool {
        matches!(self.coupon_type, CouponType::Percentage)
    }
    pub fn is_fixed_amount(&self) -> bool {
        matches!(self.coupon_type, CouponType::FixedAmount)
    }
}

/// `applied_coupons` — a coupon attached to a customer. (Lago
/// `app/models/applied_coupon.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedCoupon {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub coupon_id: Uuid,
    pub customer_id: Uuid,
    pub status: CouponStatus,
    pub amount_cents: Option<i64>,
    pub amount_currency: Option<String>,
    pub percentage_rate: Option<Decimal>,
    pub frequency: CouponFrequency,
    pub frequency_duration: Option<i32>,
    pub frequency_duration_remaining: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminated_at: Option<DateTime<Utc>>,
}

impl AppliedCoupon {
    pub fn is_once(&self) -> bool {
        matches!(self.frequency, CouponFrequency::Once)
    }
    pub fn is_recurring(&self) -> bool {
        matches!(self.frequency, CouponFrequency::Recurring)
    }
    pub fn is_forever(&self) -> bool {
        matches!(self.frequency, CouponFrequency::Forever)
    }

    /// `AppliedCoupon#remaining_amount` = `amount_cents - already-credited`.
    /// The consumed amount (sum of prior credits) is passed explicitly since
    /// it is derived from the `credits` association in Lago.
    pub fn remaining_amount(&self, already_credited_cents: Decimal) -> Decimal {
        Decimal::from(self.amount_cents.unwrap_or(0)) - already_credited_cents
    }
}

/// `credits` — a discount/credit line applied to an invoice, from a coupon,
/// a credit note, or a progressive-billing invoice. (Lago
/// `app/models/credit.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credit {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Option<Uuid>,
    pub applied_coupon_id: Option<Uuid>,
    pub credit_note_id: Option<Uuid>,
    pub progressive_billing_invoice_id: Option<Uuid>,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub before_taxes: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
