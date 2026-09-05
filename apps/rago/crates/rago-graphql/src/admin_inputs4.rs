//! Input objects for slice-4 admin mutations (catalog v2 + RBAC). Auth
//! mutations mostly take flat arguments (matching Lago) and are declared inline
//! on the resolvers; the input objects here cover the catalog + role shapes.

use async_graphql::{InputObject, Json, ID};

// ---- RBAC ------------------------------------------------------------------

#[derive(InputObject)]
pub struct CreateRoleInput {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
}

#[derive(InputObject)]
pub struct UpdateRoleInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub permissions: Option<Vec<String>>,
}

// ---- catalog: products -----------------------------------------------------

#[derive(InputObject)]
pub struct CreateProductInput {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
    pub product_type: String,
    pub billable_metric_id: Option<ID>,
    pub product_category_id: Option<ID>,
}

#[derive(InputObject)]
pub struct UpdateProductInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
    pub product_category_id: Option<ID>,
}

#[derive(InputObject)]
pub struct CreateProductCategoryInput {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateProductCategoryInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct CreateProductFilterInput {
    pub product_id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
    pub values: Json<serde_json::Value>,
}

#[derive(InputObject)]
pub struct UpdateProductFilterInput {
    pub id: ID,
    pub name: Option<String>,
    pub invoice_display_name: Option<String>,
    pub values: Option<Json<serde_json::Value>>,
}

// ---- catalog: rate cards ---------------------------------------------------

#[derive(InputObject)]
pub struct CreateRateCardInput {
    pub product_id: ID,
    pub code: String,
    pub name: String,
    pub currency: String,
    pub description: Option<String>,
    pub billing_timing: Option<String>,
    pub proration: Option<bool>,
    pub display_on_invoice: Option<bool>,
    pub wallet_targetable: Option<bool>,
}

#[derive(InputObject)]
pub struct UpdateRateCardInput {
    pub id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub proration: Option<bool>,
    pub display_on_invoice: Option<bool>,
    pub wallet_targetable: Option<bool>,
}

#[derive(InputObject)]
pub struct CreateRateCardRateInput {
    pub rate_card_id: ID,
    pub charge_model: String,
    pub properties: Option<Json<serde_json::Value>>,
}

#[derive(InputObject)]
pub struct UpdateRateCardRateInput {
    pub id: ID,
    pub charge_model: Option<String>,
    pub properties: Option<Json<serde_json::Value>>,
}

#[derive(InputObject)]
pub struct CreatePlanAppliedRateCardInput {
    pub plan_id: ID,
    pub rate_card_id: ID,
}

#[derive(InputObject)]
pub struct CreateRatePhaseInput {
    pub plan_applied_rate_card_id: ID,
    pub code: String,
    pub name: Option<String>,
    pub position: Option<i32>,
    pub billing_interval_cycle_count: Option<i32>,
}

#[derive(InputObject)]
pub struct UpdateRatePhaseInput {
    pub id: ID,
    pub name: Option<String>,
    pub position: Option<i32>,
    pub billing_interval_cycle_count: Option<i32>,
}

// ---- catalog: pricing units + commitments ---------------------------------

#[derive(InputObject)]
pub struct CreatePricingUnitInput {
    pub code: String,
    pub name: String,
    pub short_name: String,
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct UpdatePricingUnitInput {
    pub id: ID,
    pub name: Option<String>,
    pub short_name: Option<String>,
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct CreateCommitmentInput {
    pub plan_id: Option<ID>,
    pub commitment_type: Option<String>,
    pub amount_cents: i64,
    pub invoice_display_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateCommitmentInput {
    pub id: ID,
    pub amount_cents: Option<i64>,
    pub invoice_display_name: Option<String>,
}
