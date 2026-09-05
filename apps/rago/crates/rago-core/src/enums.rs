//! Domain enums, faithfully mirroring the integer values Lago's ActiveRecord
//! `enum` declarations assign (the position in the declared array, unless an
//! explicit hash is given). These integers are what is persisted in Postgres,
//! so the mapping must match exactly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid enum value {value} for {kind}")]
pub struct EnumError {
    pub kind: &'static str,
    pub value: i32,
}

/// Generate a C-like `#[repr(i32)]` enum with `TryFrom<i32>` / `as_i32`,
/// plus a snake_case string form matching Lago's symbol names.
macro_rules! int_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $val:expr => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $($variant),+
        }
        impl $name {
            pub fn as_i32(self) -> i32 { match self { $(Self::$variant => $val),+ } }
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $s),+ } }
        }
        impl TryFrom<i32> for $name {
            type Error = EnumError;
            fn try_from(v: i32) -> Result<Self, EnumError> {
                match v { $($val => Ok(Self::$variant),)+ _ => Err(EnumError { kind: stringify!($name), value: v }) }
            }
        }
    };
}

int_enum! {
    /// `Charge#charge_model` — index into `Charge::CHARGE_MODELS`.
    ChargeModel {
        Standard = 0 => "standard",
        Graduated = 1 => "graduated",
        Package = 2 => "package",
        Percentage = 3 => "percentage",
        Volume = 4 => "volume",
        GraduatedPercentage = 5 => "graduated_percentage",
        Custom = 6 => "custom",
        Dynamic = 7 => "dynamic",
    }
}

int_enum! {
    /// `BillableMetric#aggregation_type` — explicit hash in Lago.
    AggregationType {
        CountAgg = 0 => "count_agg",
        SumAgg = 1 => "sum_agg",
        MaxAgg = 2 => "max_agg",
        UniqueCountAgg = 3 => "unique_count_agg",
        WeightedSumAgg = 5 => "weighted_sum_agg",
        LatestAgg = 6 => "latest_agg",
        CustomAgg = 7 => "custom_agg",
    }
}

int_enum! {
    /// `Plan#interval`.
    PlanInterval {
        Weekly = 0 => "weekly",
        Monthly = 1 => "monthly",
        Yearly = 2 => "yearly",
        Quarterly = 3 => "quarterly",
        Semiannual = 4 => "semiannual",
    }
}

int_enum! {
    /// `Subscription#status`.
    SubscriptionStatus {
        Pending = 0 => "pending",
        Active = 1 => "active",
        Terminated = 2 => "terminated",
        Canceled = 3 => "canceled",
        Incomplete = 4 => "incomplete",
    }
}

int_enum! {
    /// `Subscription#billing_time`.
    BillingTime {
        Calendar = 0 => "calendar",
        Anniversary = 1 => "anniversary",
    }
}

int_enum! {
    /// `Coupon#coupon_type`.
    CouponType {
        FixedAmount = 0 => "fixed_amount",
        Percentage = 1 => "percentage",
    }
}

int_enum! {
    /// `Coupon#frequency` / `AppliedCoupon#frequency`.
    CouponFrequency {
        Once = 0 => "once",
        Recurring = 1 => "recurring",
        Forever = 2 => "forever",
    }
}

int_enum! {
    /// `Coupon#status` / `AppliedCoupon#status`.
    CouponStatus {
        Active = 0 => "active",
        Terminated = 1 => "terminated",
    }
}

int_enum! {
    /// `Coupon#expiration`.
    CouponExpiration {
        NoExpiration = 0 => "no_expiration",
        TimeLimit = 1 => "time_limit",
    }
}

int_enum! {
    /// `Fee#fee_type`.
    FeeType {
        Charge = 0 => "charge",
        AddOn = 1 => "add_on",
        Subscription = 2 => "subscription",
        Credit = 3 => "credit",
        Commitment = 4 => "commitment",
        FixedCharge = 5 => "fixed_charge",
        Product = 6 => "product",
    }
}

int_enum! {
    /// `Fee#payment_status` / `Invoice#payment_status`.
    PaymentStatus {
        Pending = 0 => "pending",
        Succeeded = 1 => "succeeded",
        Failed = 2 => "failed",
        Refunded = 3 => "refunded",
    }
}

int_enum! {
    /// `Invoice#invoice_type`.
    InvoiceType {
        Subscription = 0 => "subscription",
        AddOn = 1 => "add_on",
        Credit = 2 => "credit",
        OneOff = 3 => "one_off",
        AdvanceCharges = 4 => "advance_charges",
        ProgressiveBilling = 5 => "progressive_billing",
    }
}

int_enum! {
    /// `Invoice#status` — `VISIBLE_STATUS.merge(INVISIBLE_STATUS)`.
    InvoiceStatus {
        Draft = 0 => "draft",
        Finalized = 1 => "finalized",
        Voided = 2 => "voided",
        Generating = 3 => "generating",
        Failed = 4 => "failed",
        Open = 5 => "open",
        Closed = 6 => "closed",
        Pending = 7 => "pending",
        Deleted = 8 => "deleted",
    }
}

int_enum! {
    /// `Wallet#status`.
    WalletStatus {
        Active = 0 => "active",
        Terminated = 1 => "terminated",
    }
}

int_enum! {
    /// `WalletTransaction#transaction_type`.
    WalletTransactionType {
        Inbound = 0 => "inbound",
        Outbound = 1 => "outbound",
    }
}

int_enum! {
    /// `WalletTransaction#status`.
    WalletTransactionStatus {
        Pending = 0 => "pending",
        Settled = 1 => "settled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_model_roundtrips() {
        for v in [0, 1, 2, 3, 4, 5, 6, 7] {
            let m = ChargeModel::try_from(v).unwrap();
            assert_eq!(m.as_i32(), v);
        }
        assert_eq!(ChargeModel::Graduated.as_str(), "graduated");
        assert!(ChargeModel::try_from(99).is_err());
    }

    #[test]
    fn aggregation_skips_four() {
        assert!(AggregationType::try_from(4).is_err());
        assert_eq!(AggregationType::WeightedSumAgg.as_i32(), 5);
    }
}
