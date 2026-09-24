//! Third-party integrations — the `Integrations::Aggregator::*` abstraction.
//!
//! Three provider categories, each a trait over the injectable
//! [`crate::transport::HttpTransport`]:
//! * **tax** ([`TaxProvider`]) — Anrok and Avalara: fetch external tax
//!   amounts/breakdown for an invoice's fees, void, and report credit notes.
//! * **accounting** ([`AccountingProvider`]) — NetSuite (RESTlet over OAuth 1.0
//!   TBA, or through the aggregator) and Xero: contacts, invoices, credit notes
//!   and the item / account / subsidiary catalog for mappings.
//! * **crm** ([`CrmProvider`]) — Salesforce and HubSpot: create or update a
//!   customer as a contact/company; HubSpot also mirrors invoices and
//!   subscriptions as custom objects associated to that contact/company.

pub mod anrok;
pub mod avalara;
pub mod hubspot;
pub mod netsuite;
pub mod oauth1;
pub mod salesforce;
pub mod xero;

use async_trait::async_trait;
use thiserror::Error;

use crate::transport::TransportError;

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("integration error {code:?}: {message}")]
    Provider { code: Option<String>, message: String },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("provider `{0}` is not implemented")]
    NotImplemented(&'static str),
}

// ---- tax -------------------------------------------------------------------

/// A fee to be taxed (`Integrations::Aggregator::Taxes::Invoices` payload item).
#[derive(Debug, Clone)]
pub struct TaxFeeItem {
    /// Fee id (`item_id` in the response).
    pub item_id: String,
    /// Product/charge code used for tax classification (`item_code`).
    pub item_code: String,
    pub amount_cents: i64,
}

/// The request for external invoice taxes.
#[derive(Debug, Clone)]
pub struct TaxRequest {
    pub invoice_id: String,
    pub currency: String,
    pub customer_external_id: String,
    pub fees: Vec<TaxFeeItem>,
}

/// One line of a provider tax breakdown
/// (`TaxResult::TaxBreakdownItem`): `{ name, rate, tax_amount, type }`.
#[derive(Debug, Clone)]
pub struct TaxBreakdownItem {
    pub name: String,
    /// Fractional rate (e.g. `0.085` for 8.5%).
    pub rate: f64,
    pub tax_amount: i64,
    pub type_: String,
}

/// The provider's tax result for one fee (`Integrations::Aggregator::Taxes::TaxResult`).
#[derive(Debug, Clone)]
pub struct FeeTaxResult {
    pub item_id: String,
    pub tax_amount_cents: i64,
    pub tax_breakdown: Vec<TaxBreakdownItem>,
}

/// A tax integration (Anrok, Avalara).
#[async_trait]
pub trait TaxProvider: Send + Sync {
    fn provider_type(&self) -> &'static str;

    /// `Integrations::Aggregator::Taxes::Invoices::CreateService` — fetch the
    /// per-fee tax results for an invoice.
    async fn fetch_invoice_taxes(
        &self,
        req: &TaxRequest,
    ) -> Result<Vec<FeeTaxResult>, IntegrationError>;
}

// ---- accounting ------------------------------------------------------------

/// A finalized invoice (or credit note) to sync to an accounting system.
#[derive(Debug, Clone, Default)]
pub struct AccountingInvoice {
    pub invoice_id: String,
    pub number: String,
    pub currency: String,
    pub total_amount_cents: i64,
    /// Rago's customer `external_id` (used when no provider contact is linked).
    pub customer_external_id: String,
    /// The provider contact id the customer is linked to
    /// (`integration_customers.external_customer_id`), when known.
    pub contact_external_id: Option<String>,
    /// `YYYY-MM-DD`.
    pub issuing_date: Option<String>,
    /// `YYYY-MM-DD`.
    pub due_date: Option<String>,
    pub taxes_amount_cents: i64,
    /// Fee lines with their mapped provider item / account.
    pub lines: Vec<AccountingLine>,
}

/// One invoice line, already resolved against the integration's mappings.
#[derive(Debug, Clone, Default)]
pub struct AccountingLine {
    /// The Lago fee id.
    pub lago_id: String,
    pub description: String,
    /// Mapped provider item id (NetSuite internal id / Xero item code).
    pub item_external_id: Option<String>,
    /// Mapped ledger account code.
    pub account_code: Option<String>,
    /// Units as a decimal string.
    pub units: String,
    pub unit_amount_cents: i64,
    pub amount_cents: i64,
    pub taxes_amount_cents: i64,
    pub taxes_rate: f64,
}

/// A customer to create as an accounting contact.
#[derive(Debug, Clone, Default)]
pub struct AccountingContact {
    pub customer_id: String,
    pub external_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub currency: Option<String>,
    pub country: Option<String>,
    pub tax_identification_number: Option<String>,
    /// NetSuite subsidiary the contact belongs to.
    pub subsidiary_id: Option<String>,
}

/// One entry of a provider catalog (items, ledger accounts, subsidiaries,
/// tax codes) fetched for the mapping pickers.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub external_id: String,
    pub name: Option<String>,
    pub account_code: Option<String>,
}

/// Parse an aggregator/provider error body into an [`IntegrationError`].
pub(crate) fn provider_error(body: &str, fallback: &str) -> IntegrationError {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let message = json
        .get("error")
        .and_then(|e| e.as_str().map(str::to_string).or_else(|| e.get("message").and_then(|m| m.as_str()).map(str::to_string)))
        .or_else(|| json.get("message").and_then(serde_json::Value::as_str).map(str::to_string))
        .or_else(|| json.pointer("/Elements/0/ValidationErrors/0/Message").and_then(serde_json::Value::as_str).map(str::to_string))
        .or_else(|| json.pointer("/0/message").and_then(serde_json::Value::as_str).map(str::to_string))
        .unwrap_or_else(|| fallback.to_string());
    IntegrationError::Provider {
        code: json
            .get("code")
            .and_then(serde_json::Value::as_str)
            .or_else(|| json.pointer("/0/errorCode").and_then(serde_json::Value::as_str))
            .map(str::to_string),
        message,
    }
}

/// Parse a catalog list from a provider response: an array at one of `keys`
/// (or the body itself), each entry's id / name / account code read from the
/// first present field of the given candidates.
pub(crate) fn parse_catalog(
    body: &str,
    keys: &[&str],
    id_fields: &[&str],
    name_fields: &[&str],
    code_fields: &[&str],
) -> Result<Vec<CatalogEntry>, IntegrationError> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
    let list = keys
        .iter()
        .find_map(|k| json.get(*k).and_then(serde_json::Value::as_array))
        .or_else(|| json.as_array())
        .cloned()
        .unwrap_or_default();
    let pick = |v: &serde_json::Value, fields: &[&str]| {
        fields.iter().find_map(|f| {
            let field = if f.contains('.') { v.pointer(&format!("/{}", f.replace('.', "/"))) } else { v.get(*f) };
            match field {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
            }
        })
    };
    Ok(list
        .iter()
        .filter_map(|v| {
            Some(CatalogEntry {
                external_id: pick(v, id_fields)?,
                name: pick(v, name_fields),
                account_code: pick(v, code_fields),
            })
        })
        .collect())
}

/// The result of an accounting sync: the external record id.
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub external_id: String,
}

/// A succeeded payment to record against an already-synced invoice
/// (`Integrations::Aggregator::Payments::CreateService`).
#[derive(Debug, Clone, Default)]
pub struct AccountingPayment {
    /// The Lago payment id.
    pub payment_id: String,
    /// The provider id of the invoice the payment settles (from the invoice's
    /// sync mapping).
    pub invoice_external_id: String,
    pub invoice_number: String,
    /// The provider contact / customer id.
    pub contact_external_id: Option<String>,
    pub amount_cents: i64,
    pub currency: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Free-text reference (the payment provider's id when known).
    pub reference: Option<String>,
    /// The ledger (bank) account the payment is deposited to, from the
    /// integration's `account` collection mapping.
    pub account_code: Option<String>,
}

/// An accounting integration (NetSuite, Xero).
#[async_trait]
pub trait AccountingProvider: Send + Sync {
    fn provider_type(&self) -> &'static str;

    /// `Integrations::Aggregator::Invoices::CreateService` — sync a finalized
    /// invoice, returning the external accounting id.
    async fn sync_invoice(&self, invoice: &AccountingInvoice) -> Result<SyncResult, IntegrationError>;
}

// ---- crm -------------------------------------------------------------------

/// A customer to sync as a CRM contact/company.
#[derive(Debug, Clone)]
pub struct CrmCustomer {
    pub customer_id: String,
    pub external_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

/// A finalized invoice to mirror as a CRM record.
#[derive(Debug, Clone, Default)]
pub struct CrmInvoice {
    pub invoice_id: String,
    pub number: String,
    pub invoice_type: String,
    pub status: String,
    pub payment_status: String,
    pub currency: String,
    /// `YYYY-MM-DD`.
    pub issuing_date: Option<String>,
    /// `YYYY-MM-DD`.
    pub payment_due_date: Option<String>,
    pub sub_total_excluding_taxes_amount_cents: i64,
    pub taxes_amount_cents: i64,
    pub total_amount_cents: i64,
}

/// A subscription to mirror as a CRM record.
#[derive(Debug, Clone, Default)]
pub struct CrmSubscription {
    pub subscription_id: String,
    pub external_id: String,
    pub name: Option<String>,
    pub plan_code: String,
    pub status: String,
    pub billing_time: String,
    /// `YYYY-MM-DD`.
    pub subscription_at: Option<String>,
    /// `YYYY-MM-DD`.
    pub started_at: Option<String>,
    /// `YYYY-MM-DD`.
    pub ending_at: Option<String>,
}

/// The outcome of a CRM record upsert: the record id, plus the error of the
/// association to the customer's contact/company when that step failed after
/// the record itself was written (the id must still be stored so a retry
/// updates instead of duplicating).
#[derive(Debug)]
pub struct CrmRecordSync {
    pub external_id: String,
    pub association_error: Option<IntegrationError>,
}

/// A CRM integration (Hubspot, Salesforce).
#[async_trait]
pub trait CrmProvider: Send + Sync {
    fn provider_type(&self) -> &'static str;

    /// `Integrations::Aggregator::Contacts::CreateService` — sync a customer as
    /// a CRM contact, returning the external contact id.
    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError>;

    /// Update the CRM record previously created for `customer`
    /// (`Integrations::Aggregator::Contacts::UpdateService`). When the record
    /// no longer exists at the provider it is created again; the returned id
    /// is the record that now mirrors the customer.
    async fn update_customer(
        &self,
        external_id: &str,
        customer: &CrmCustomer,
    ) -> Result<SyncResult, IntegrationError>;
}
