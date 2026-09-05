use crate::enums::PlanInterval;
use crate::money::Currency;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `plans` — a subscription plan. (Lago `app/models/plan.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub code: String,
    pub interval: Option<PlanInterval>,
    pub description: Option<String>,
    pub amount_cents: Option<i64>,
    pub amount_currency: String,
    pub trial_period: Option<f64>,
    pub pay_in_advance: bool,
    pub bill_charges_monthly: Option<bool>,
    pub parent_id: Option<Uuid>,
    pub invoice_display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Plan {
    /// `Plan#amount.currency` — the plan's billing currency.
    pub fn currency(&self) -> Currency {
        Currency::new(&self.amount_currency)
    }
}
