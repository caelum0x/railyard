use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `organizations` — the top-level tenant. (Lago `app/models/organization.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub default_currency: String,
    pub email: Option<String>,
    pub country: Option<String>,
    pub tax_identification_number: Option<String>,
    pub timezone: String,
    pub document_locale: String,
    pub finalize_zero_amount_invoice: bool,
    /// Secret key used to sign outbound webhooks (Lago `hmac_key`).
    #[serde(default)]
    pub hmac_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
