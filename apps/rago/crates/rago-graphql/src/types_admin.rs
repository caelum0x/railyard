//! Additional GraphQL object types for the dashboard admin CRUD surface —
//! faithful ports of `Types::Taxes::Object`, `Types::AppliedCoupons::Object`,
//! `Types::ChargeFilters::Object` and `Types::Webhooks::Object`. Each wraps a
//! `rago-core` model (or a `rago-db` row) and exposes Lago's camelCase fields.

use async_graphql::{Object, SimpleObject, ID};
use rago_core::models as m;

use crate::types::CollectionMetadata;

// ---- Tax -------------------------------------------------------------------

pub struct Tax(pub m::Tax);

#[Object]
impl Tax {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn code(&self) -> &str {
        &self.0.code
    }
    async fn description(&self) -> Option<&str> {
        self.0.description.as_deref()
    }
    async fn rate(&self) -> f64 {
        self.0.rate
    }
    async fn applied_to_organization(&self) -> bool {
        self.0.applied_to_organization
    }
    async fn created_at(&self) -> String {
        self.0.created_at.to_rfc3339()
    }
}

#[derive(SimpleObject)]
pub struct TaxCollection {
    pub collection: Vec<Tax>,
    pub metadata: CollectionMetadata,
}

// ---- AppliedCoupon ---------------------------------------------------------

pub struct AppliedCoupon(pub m::AppliedCoupon);

#[Object]
impl AppliedCoupon {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn lago_coupon_id(&self) -> ID {
        self.0.coupon_id.into()
    }
    async fn lago_customer_id(&self) -> ID {
        self.0.customer_id.into()
    }
    async fn status(&self) -> &str {
        self.0.status.as_str()
    }
    async fn amount_cents(&self) -> Option<i64> {
        self.0.amount_cents
    }
    async fn amount_currency(&self) -> Option<&str> {
        self.0.amount_currency.as_deref()
    }
    async fn percentage_rate(&self) -> Option<String> {
        self.0.percentage_rate.map(|d| d.to_string())
    }
    async fn frequency(&self) -> &str {
        self.0.frequency.as_str()
    }
    async fn frequency_duration(&self) -> Option<i32> {
        self.0.frequency_duration
    }
    async fn frequency_duration_remaining(&self) -> Option<i32> {
        self.0.frequency_duration_remaining
    }
    async fn terminated_at(&self) -> Option<String> {
        self.0.terminated_at.map(|d| d.to_rfc3339())
    }
    async fn created_at(&self) -> String {
        self.0.created_at.to_rfc3339()
    }
}

// ---- ChargeFilter ----------------------------------------------------------

pub struct ChargeFilter(pub m::ChargeFilter);

#[Object]
impl ChargeFilter {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn lago_charge_id(&self) -> ID {
        self.0.charge_id.into()
    }
    async fn invoice_display_name(&self) -> Option<&str> {
        self.0.invoice_display_name.as_deref()
    }
    async fn properties(&self) -> async_graphql::Json<serde_json::Value> {
        async_graphql::Json(self.0.properties.clone())
    }
    async fn created_at(&self) -> String {
        self.0.created_at.to_rfc3339()
    }
}

// ---- AddOn collection ------------------------------------------------------

use crate::types::AddOn;

#[derive(SimpleObject)]
pub struct AddOnCollection {
    pub collection: Vec<AddOn>,
    pub metadata: CollectionMetadata,
}

// ---- Webhook ---------------------------------------------------------------

/// `Types::Webhooks::Object` — an outbound webhook delivery record.
#[derive(SimpleObject)]
pub struct Webhook {
    pub id: ID,
    pub webhook_type: String,
    pub object_type: String,
    pub status: String,
    pub http_status: Option<i32>,
    pub endpoint: String,
    pub retries: i32,
}

impl From<rago_db::WebhookRow> for Webhook {
    fn from(w: rago_db::WebhookRow) -> Self {
        Self {
            id: w.id.into(),
            webhook_type: w.webhook_type,
            object_type: w.object_type,
            status: w.status,
            http_status: w.http_status,
            endpoint: w.endpoint,
            retries: w.retries,
        }
    }
}
