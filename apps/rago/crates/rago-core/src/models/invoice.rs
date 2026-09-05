use crate::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use crate::money::Currency;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `invoices.applied_taxes` — a tax applied at invoice level. (Lago
/// `app/models/invoice/applied_tax.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceAppliedTax {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub tax_id: Uuid,
    pub tax_code: String,
    pub tax_name: String,
    pub tax_rate: f64,
    pub tax_description: Option<String>,
    pub amount_cents: i64,
    pub fees_amount_cents: i64,
    pub amount_currency: String,
}

/// `invoices` — the aggregate billing document. (Lago `app/models/invoice.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub billing_entity_id: Uuid,
    pub customer_id: Option<Uuid>,
    pub number: String,
    pub sequential_id: Option<i32>,
    pub invoice_type: InvoiceType,
    pub status: InvoiceStatus,
    pub payment_status: PaymentStatus,
    pub currency: Option<String>,
    pub issuing_date: Option<NaiveDate>,

    // Monetary rollups (all `bigint` cents).
    pub fees_amount_cents: i64,
    pub coupons_amount_cents: i64,
    pub credit_notes_amount_cents: i64,
    pub prepaid_credit_amount_cents: i64,
    pub progressive_billing_credit_amount_cents: i64,
    pub sub_total_excluding_taxes_amount_cents: i64,
    pub sub_total_including_taxes_amount_cents: i64,
    pub taxes_amount_cents: i64,
    pub taxes_rate: f64,
    pub total_amount_cents: i64,

    pub version_number: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    /// In-memory only: applied taxes assembled during computation.
    #[serde(default)]
    pub applied_taxes: Vec<InvoiceAppliedTax>,
}

impl Invoice {
    pub fn currency(&self) -> Currency {
        Currency::new(self.currency.as_deref().unwrap_or("USD"))
    }
}

/// `invoice_subscriptions` — join between an invoice and a subscription,
/// carrying the billing-period boundaries. (Lago
/// `app/models/invoice_subscription.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceSubscription {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub subscription_id: Uuid,
    pub recurring: Option<bool>,
    pub timestamp: Option<DateTime<Utc>>,
    pub from_datetime: Option<DateTime<Utc>>,
    pub to_datetime: Option<DateTime<Utc>>,
    pub charges_from_datetime: Option<DateTime<Utc>>,
    pub charges_to_datetime: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
