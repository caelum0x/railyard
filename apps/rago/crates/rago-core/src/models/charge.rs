use crate::enums::ChargeModel;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// `charges` — attaches a billable metric to a plan with a pricing model and
/// its JSON `properties`. (Lago `app/models/charge.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Charge {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub billable_metric_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub code: String,
    pub charge_model: ChargeModel,
    /// Charge-model configuration: `amount`, `graduated_ranges`, `package_size`, …
    pub properties: Value,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoiceable: bool,
    pub min_amount_cents: i64,
    pub amount_currency: Option<String>,
    pub accepts_target_wallet: bool,
    pub invoice_display_name: Option<String>,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Charge {
    /// `Charge#pricing_group_keys` — new name for the deprecated `grouped_by`.
    pub fn pricing_group_keys(&self) -> Option<&Value> {
        self.properties
            .get("pricing_group_keys")
            .or_else(|| self.properties.get("grouped_by"))
    }
}

/// `charge_filters` — a named subset of a charge's events with overriding
/// `properties`. (Lago `app/models/charge_filter.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChargeFilter {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub charge_id: Uuid,
    pub properties: Value,
    pub invoice_display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
