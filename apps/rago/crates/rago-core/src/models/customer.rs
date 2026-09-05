use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `customers` — the billed entity. (Lago `app/models/customer.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub billing_entity_id: Uuid,
    pub external_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    /// Customer override currency; falls back to the organization default.
    pub currency: Option<String>,
    pub timezone: Option<String>,
    pub tax_identification_number: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
