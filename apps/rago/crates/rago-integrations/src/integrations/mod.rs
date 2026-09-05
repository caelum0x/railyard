//! Third-party integrations — the `Integrations::Aggregator::*` abstraction.
//!
//! Three provider categories, each a trait over the injectable
//! [`crate::transport::HttpTransport`], with one faithful representative
//! implementation:
//! * **tax** ([`TaxProvider`] → [`anrok::AnrokTaxProvider`]) — fetch external
//!   tax amounts/breakdown for an invoice's fees (wires into the invoice tax
//!   path). Avalara is the same trait (follow-up).
//! * **accounting** ([`AccountingProvider`] → [`netsuite::NetsuiteProvider`]) —
//!   sync a finalized invoice. Xero is the same trait (follow-up).
//! * **crm** ([`CrmProvider`] → [`hubspot::HubspotProvider`]) — sync a customer
//!   as a contact/company. Salesforce is the same trait (follow-up).

pub mod anrok;
pub mod avalara;
pub mod hubspot;
pub mod netsuite;
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

/// A finalized invoice to sync to an accounting system.
#[derive(Debug, Clone)]
pub struct AccountingInvoice {
    pub invoice_id: String,
    pub number: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub customer_external_id: String,
}

/// The result of an accounting sync: the external record id.
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub external_id: String,
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

/// A CRM integration (Hubspot, Salesforce).
#[async_trait]
pub trait CrmProvider: Send + Sync {
    fn provider_type(&self) -> &'static str;

    /// `Integrations::Aggregator::Contacts::CreateService` — sync a customer as
    /// a CRM contact, returning the external contact id.
    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError>;
}
