use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `payments` — a provider charge against an invoice. (Lago
/// `app/models/payment.rb`, invoice payable.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub provider: String,
    pub provider_payment_id: Option<String>,
    pub provider_status: Option<String>,
    /// Payable-facing status: `pending` / `succeeded` / `failed`.
    pub status: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
