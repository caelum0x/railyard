//! Input objects for the slice-2 admin mutations. Field names mirror the
//! corresponding Lago `Types::*` inputs; only the fields Rago persists are
//! modelled.

use async_graphql::{InputObject, Json, ID};

// ---- one-off invoices ------------------------------------------------------

#[derive(InputObject)]
pub struct OneOffFeeInput {
    pub add_on_id: Option<ID>,
    pub unit_amount_cents: i64,
    pub units: Option<String>,
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct CreateInvoiceInput {
    pub customer_id: ID,
    pub currency: Option<String>,
    pub fees: Vec<OneOffFeeInput>,
}

// ---- usage-monitoring alerts ----------------------------------------------

#[derive(InputObject, Clone)]
pub struct AlertThresholdInput {
    pub code: Option<String>,
    pub value: String,
    pub recurring: Option<bool>,
}

#[derive(InputObject)]
pub struct CreateSubscriptionAlertInput {
    pub subscription_id: ID,
    pub alert_type: String,
    pub code: String,
    pub name: Option<String>,
    pub billable_metric_id: Option<ID>,
    pub thresholds: Vec<AlertThresholdInput>,
}

#[derive(InputObject)]
pub struct CreateWalletAlertInput {
    pub wallet_id: ID,
    pub alert_type: String,
    pub code: String,
    pub name: Option<String>,
    pub billable_metric_id: Option<ID>,
    pub thresholds: Vec<AlertThresholdInput>,
}

#[derive(InputObject)]
pub struct UpdateAlertInput {
    pub id: ID,
    pub name: Option<String>,
    pub code: Option<String>,
    pub thresholds: Option<Vec<AlertThresholdInput>>,
}

// ---- billing entities ------------------------------------------------------

#[derive(InputObject)]
pub struct CreateBillingEntityInput {
    pub code: String,
    pub name: String,
    pub email: Option<String>,
    pub country: Option<String>,
    pub default_currency: Option<String>,
    pub timezone: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateBillingEntityInput {
    pub id: ID,
    pub name: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    pub default_currency: Option<String>,
    pub timezone: Option<String>,
}

// ---- invoice custom sections ----------------------------------------------

#[derive(InputObject)]
pub struct CreateInvoiceCustomSectionInput {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub details: Option<String>,
    pub display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateInvoiceCustomSectionInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub details: Option<String>,
    pub display_name: Option<String>,
}

// ---- adjusted fees ---------------------------------------------------------

#[derive(InputObject)]
pub struct CreateAdjustedFeeInput {
    pub invoice_id: ID,
    pub fee_id: ID,
    pub units: Option<String>,
    pub unit_amount_cents: Option<i64>,
    pub invoice_display_name: Option<String>,
}

// ---- fixed charges ---------------------------------------------------------

#[derive(InputObject)]
pub struct CreateFixedChargeInput {
    pub plan_id: ID,
    pub add_on_id: Option<ID>,
    pub charge_model: String,
    pub properties: Option<Json<serde_json::Value>>,
    pub units: Option<String>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateFixedChargeInput {
    pub id: ID,
    pub charge_model: Option<String>,
    pub properties: Option<Json<serde_json::Value>>,
    pub units: Option<String>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoice_display_name: Option<String>,
}
