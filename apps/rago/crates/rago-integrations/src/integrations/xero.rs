//! Xero accounting integration — port of
//! `Integrations::Aggregator::{Contacts,Invoices,CreditNotes}::CreateService`
//! (Xero path) plus the item / account catalog fetch.
//!
//! Two transports:
//! * **direct** — Xero's Accounting API (`api.xero.com/api.xro/2.0`) with the
//!   connection's OAuth access token and the `Xero-tenant-id` captured when the
//!   organization authorized the app.
//! * **aggregator** — the legacy `.../v1/xero/*` endpoints with a bearer token.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    parse_catalog, provider_error, AccountingContact, AccountingInvoice, AccountingProvider, CatalogEntry,
    IntegrationError, SyncResult,
};
use crate::transport::HttpTransport;

/// Xero Accounting API root.
pub const XERO_API_BASE: &str = "https://api.xero.com/api.xro/2.0";
/// Xero tenant connections endpoint (lists the tenants a token can address).
pub const XERO_CONNECTIONS_URL: &str = "https://api.xero.com/connections";

enum Mode {
    Direct { tenant_id: String },
    Aggregator { base_url: String },
}

pub struct XeroProvider<T: HttpTransport> {
    mode: Mode,
    api_key: String,
    transport: T,
}

fn amount(cents: i64) -> f64 {
    (cents as f64) / 100.0
}

impl<T: HttpTransport> XeroProvider<T> {
    /// Aggregator mode (`{base_url}/v1/xero/*`).
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Aggregator { base_url: base_url.into() }, api_key: api_key.into(), transport }
    }

    /// Direct mode against Xero's Accounting API for one tenant.
    pub fn direct(access_token: impl Into<String>, tenant_id: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Direct { tenant_id: tenant_id.into() }, api_key: access_token.into(), transport }
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut h = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ];
        if let Mode::Direct { tenant_id } = &self.mode {
            h.push(("Xero-tenant-id".to_string(), tenant_id.clone()));
        }
        h
    }

    fn url(&self, direct_path: &str, aggregator_path: &str) -> String {
        match &self.mode {
            Mode::Direct { .. } => format!("{XERO_API_BASE}/{direct_path}"),
            Mode::Aggregator { base_url } => format!("{}/v1/xero/{aggregator_path}", base_url.trim_end_matches('/')),
        }
    }

    fn contact(doc: &AccountingInvoice) -> Value {
        match &doc.contact_external_id {
            Some(id) => json!({ "ContactID": id }),
            None => json!({ "ContactNumber": doc.customer_external_id }),
        }
    }

    fn line_items(doc: &AccountingInvoice) -> Vec<Value> {
        doc.lines
            .iter()
            .map(|l| {
                let units: f64 = l.units.parse().unwrap_or(1.0);
                json!({
                    "Description": l.description,
                    "Quantity": units,
                    "UnitAmount": amount(l.unit_amount_cents),
                    "LineAmount": amount(l.amount_cents),
                    "ItemCode": l.item_external_id,
                    "AccountCode": l.account_code,
                    "TaxAmount": amount(l.taxes_amount_cents),
                })
            })
            .collect()
    }

    fn document(doc: &AccountingInvoice, doc_type: &str, number_key: &str) -> Value {
        json!({
            "Type": doc_type,
            number_key: doc.number,
            "Reference": doc.invoice_id,
            "Contact": Self::contact(doc),
            "Date": doc.issuing_date,
            "DueDate": doc.due_date,
            "CurrencyCode": doc.currency,
            "Status": "AUTHORISED",
            "LineAmountTypes": "Exclusive",
            "LineItems": Self::line_items(doc),
        })
    }

    async fn post_for_id(&self, url: &str, body: Value, list_key: &str, id_key: &str) -> Result<SyncResult, IntegrationError> {
        let resp = self.transport.post(url, &self.headers(), body.to_string()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "xero error"));
        }
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        json.get("succeededId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| json.pointer(&format!("/{list_key}/0/{id_key}")).and_then(Value::as_str).map(str::to_string))
            .map(|external_id| SyncResult { external_id })
            .ok_or_else(|| IntegrationError::InvalidResponse(format!("missing {id_key}")))
    }

    /// Sync a credit note as an `ACCRECCREDIT` document.
    pub async fn sync_credit_note(&self, note: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        let url = self.url("CreditNotes", "creditnotes");
        let mut doc = Self::document(note, "ACCRECCREDIT", "CreditNoteNumber");
        let body = match self.mode {
            Mode::Direct { .. } => json!({ "CreditNotes": [doc] }),
            Mode::Aggregator { .. } => {
                doc["Total"] = json!(note.total_amount_cents);
                doc["lago_id"] = json!(note.invoice_id);
                doc
            }
        };
        self.post_for_id(&url, body, "CreditNotes", "CreditNoteID").await
    }

    /// Create the customer as a Xero contact.
    pub async fn create_contact(&self, contact: &AccountingContact) -> Result<SyncResult, IntegrationError> {
        let url = self.url("Contacts", "contacts");
        let c = json!({
            "Name": contact.name.clone().unwrap_or_else(|| contact.external_id.clone()),
            "ContactNumber": contact.external_id,
            "AccountNumber": contact.external_id,
            "EmailAddress": contact.email,
            "TaxNumber": contact.tax_identification_number,
            "DefaultCurrency": contact.currency,
        });
        let body = match self.mode {
            Mode::Direct { .. } => json!({ "Contacts": [c] }),
            Mode::Aggregator { .. } => c,
        };
        self.post_for_id(&url, body, "Contacts", "ContactID").await
    }

    /// Fetch the `item` or `account` catalog.
    pub async fn fetch_catalog(&self, kind: &str) -> Result<Vec<CatalogEntry>, IntegrationError> {
        let (direct, agg, key) = match kind {
            "item" => ("Items", "items", "Items"),
            "account" => ("Accounts?where=Status%3D%3D%22ACTIVE%22", "accounts", "Accounts"),
            other => return Err(IntegrationError::InvalidResponse(format!("unsupported catalog: {other}"))),
        };
        let url = self.url(direct, agg);
        let resp = self.transport.get(&url, &self.headers()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "xero catalog error"));
        }
        match kind {
            // Xero line items reference items by `Code`.
            "item" => parse_catalog(&resp.body, &[key, "records"], &["Code", "ItemID", "id"], &["Name", "name"], &["SalesDetails.AccountCode"]),
            _ => parse_catalog(&resp.body, &[key, "records"], &["AccountID", "id"], &["Name", "name"], &["Code", "code"]),
        }
    }

    /// The tenants an access token can address (`GET /connections`):
    /// `(tenant_id, tenant_name)`.
    pub async fn connections(&self) -> Result<Vec<(String, Option<String>)>, IntegrationError> {
        let headers = vec![("Authorization".to_string(), format!("Bearer {}", self.api_key))];
        let resp = self.transport.get(XERO_CONNECTIONS_URL, &headers).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "xero connections error"));
        }
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        Ok(json
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|c| {
                Some((
                    c.get("tenantId")?.as_str()?.to_string(),
                    c.get("tenantName").and_then(Value::as_str).map(str::to_string),
                ))
            })
            .collect())
    }
}

#[async_trait]
impl<T: HttpTransport> AccountingProvider for XeroProvider<T> {
    fn provider_type(&self) -> &'static str {
        "xero"
    }

    async fn sync_invoice(&self, invoice: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        let url = self.url("Invoices", "invoices");
        let mut doc = Self::document(invoice, "ACCREC", "InvoiceNumber");
        let body = match self.mode {
            Mode::Direct { .. } => json!({ "Invoices": [doc] }),
            Mode::Aggregator { .. } => {
                doc["Total"] = json!(invoice.total_amount_cents);
                doc["lago_id"] = json!(invoice.invoice_id);
                doc
            }
        };
        self.post_for_id(&url, body, "Invoices", "InvoiceID").await
    }
}
