use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `credit_note_items` — a fee amount being credited. (Lago
/// `app/models/credit_note_item.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditNoteItem {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub credit_note_id: Option<Uuid>,
    pub fee_id: Uuid,
    pub amount_cents: i64,
    pub precise_amount_cents: Decimal,
    pub amount_currency: String,
}

/// `credit_notes.applied_taxes` — a tax on a credit note. (Lago
/// `app/models/credit_note/applied_tax.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditNoteAppliedTax {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub credit_note_id: Option<Uuid>,
    pub tax_id: Uuid,
    pub tax_code: String,
    pub tax_name: String,
    pub tax_rate: f64,
    pub tax_description: Option<String>,
    pub amount_cents: i64,
    pub base_amount_cents: i64,
    pub amount_currency: String,
}

/// `credit_notes` — a credit issued against an invoice. (Lago
/// `app/models/credit_note.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditNote {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub invoice_id: Uuid,
    pub sequential_id: i32,
    pub number: String,
    pub issuing_date: NaiveDate,
    pub reason: i32,
    pub credit_status: Option<i32>,
    pub refund_status: Option<i32>,
    pub status: i32,
    pub credit_amount_cents: i64,
    pub credit_amount_currency: String,
    pub balance_amount_cents: i64,
    pub refund_amount_cents: i64,
    pub total_amount_cents: i64,
    pub total_amount_currency: String,
    pub taxes_amount_cents: i64,
    pub taxes_rate: f64,
    pub coupons_adjustment_amount_cents: i64,
    pub precise_coupons_adjustment_amount_cents: Decimal,
    pub precise_taxes_amount_cents: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub voided_at: Option<DateTime<Utc>>,
    /// In-memory only: the items and taxes assembled during computation.
    #[serde(default)]
    pub items: Vec<CreditNoteItem>,
    #[serde(default)]
    pub applied_taxes: Vec<CreditNoteAppliedTax>,
}
