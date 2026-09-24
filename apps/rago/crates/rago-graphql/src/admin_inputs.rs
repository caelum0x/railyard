//! Input objects for the dashboard admin CRUD mutations. Field names mirror the
//! corresponding `Types::*::UpdateInput` / `CreateInput` in Lago's GraphQL
//! schema. Only the mutable fields Rago persists are modelled; Lago inputs carry
//! additional integration/metadata fields that are documented follow-ups.

use async_graphql::{InputObject, Json, ID};

#[derive(InputObject)]
pub struct UpdateCustomerInput {
    pub id: ID,
    pub name: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    pub currency: Option<String>,
    pub timezone: Option<String>,
    pub tax_identification_number: Option<String>,
    pub invoice_grace_period: Option<i32>,
}

#[derive(InputObject)]
pub struct UpdatePlanInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub amount_cents: Option<i64>,
    pub amount_currency: Option<String>,
    pub pay_in_advance: Option<bool>,
    pub interval: Option<String>,
    pub invoice_display_name: Option<String>,
    /// Replaces the plan's progressive-billing thresholds when present (an
    /// empty list clears them).
    pub usage_thresholds: Option<Vec<crate::thresholds::UsageThresholdInput>>,
}

#[derive(InputObject)]
pub struct CreateChargeInput {
    pub plan_id: ID,
    pub billable_metric_id: ID,
    pub charge_model: String,
    pub properties: Option<Json<serde_json::Value>>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoiceable: Option<bool>,
    pub min_amount_cents: Option<i64>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateChargeInput {
    pub id: ID,
    pub charge_model: Option<String>,
    pub properties: Option<Json<serde_json::Value>>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoiceable: Option<bool>,
    pub min_amount_cents: Option<i64>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct CreateChargeFilterInput {
    pub charge_id: ID,
    pub properties: Json<serde_json::Value>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateChargeFilterInput {
    pub id: ID,
    pub properties: Option<Json<serde_json::Value>>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateBillableMetricInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub aggregation_type: Option<String>,
    pub field_name: Option<String>,
    pub recurring: Option<bool>,
    pub expression: Option<String>,
    /// `custom_agg` per-event amount expression.
    pub custom_aggregator: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateAddOnInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub amount_cents: Option<i64>,
}

#[derive(InputObject)]
pub struct UpdateCouponInput {
    pub id: ID,
    pub name: Option<String>,
    pub code: Option<String>,
    pub amount_cents: Option<i64>,
    pub percentage_rate: Option<String>,
    pub frequency: Option<String>,
    pub frequency_duration: Option<i32>,
    pub reusable: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateSubscriptionInput {
    pub id: ID,
    pub name: Option<String>,
    /// RFC3339 timestamp.
    pub subscription_at: Option<String>,
    /// RFC3339 timestamp.
    pub ending_at: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateInvoiceInput {
    pub id: ID,
    pub payment_status: Option<String>,
}

#[derive(InputObject)]
pub struct CreditNoteItemInput {
    pub fee_id: ID,
    pub amount_cents: i64,
}

#[derive(InputObject)]
pub struct CreateCreditNoteInput {
    pub invoice_id: ID,
    pub reason: String,
    pub description: Option<String>,
    pub credit_amount_cents: Option<i64>,
    pub refund_amount_cents: Option<i64>,
    pub items: Vec<CreditNoteItemInput>,
}

#[derive(InputObject)]
pub struct UpdateCreditNoteInput {
    pub id: ID,
    pub refund_status: String,
}

#[derive(InputObject)]
pub struct UpdateCustomerWalletInput {
    pub id: ID,
    pub name: Option<String>,
    /// RFC3339 timestamp.
    pub expiration_at: Option<String>,
    pub priority: Option<i32>,
}

#[derive(InputObject)]
pub struct CreateTaxInput {
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub rate: f64,
}

#[derive(InputObject)]
pub struct UpdateTaxInput {
    pub id: ID,
    pub name: Option<String>,
    pub code: Option<String>,
    pub description: Option<String>,
    pub rate: Option<f64>,
    pub applied_to_organization: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateDunningCampaignInput {
    pub id: ID,
    pub name: Option<String>,
    pub code: Option<String>,
    pub description: Option<String>,
    pub max_attempts: Option<i32>,
    pub days_between_attempts: Option<i32>,
    pub applied_to_organization: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateWebhookEndpointInput {
    pub id: ID,
    pub webhook_url: Option<String>,
    pub signature_algo: Option<String>,
}
