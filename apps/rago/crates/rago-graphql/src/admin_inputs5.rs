//! Input objects for slice-5 admin mutations (quotes). Order/quote-version/AI/
//! data-export/SSO mutations take flat arguments (matching Lago) declared inline
//! on the resolvers.

use async_graphql::{InputObject, Json, ID};

#[derive(InputObject)]
pub struct CreateQuoteInput {
    pub customer_id: ID,
    pub currency: String,
    pub line_items: Option<Json<serde_json::Value>>,
    pub total_amount_cents: Option<i64>,
}

#[derive(InputObject)]
pub struct UpdateQuoteInput {
    pub id: ID,
    pub status: Option<String>,
    pub line_items: Option<Json<serde_json::Value>>,
    pub total_amount_cents: Option<i64>,
}
