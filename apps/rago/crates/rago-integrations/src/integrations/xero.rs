//! Xero accounting integration — representative port of
//! `Integrations::Aggregator::Invoices::CreateService` (Xero path).
//!
//! Syncs a finalized invoice through the aggregator (`.../v1/xero/invoices`)
//! using Xero's `ACCREC` invoice shape and returns the external record id.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{AccountingInvoice, AccountingProvider, IntegrationError, SyncResult};
use crate::transport::HttpTransport;

pub struct XeroProvider<T: HttpTransport> {
    base_url: String,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> XeroProvider<T> {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            transport,
        }
    }
}

#[async_trait]
impl<T: HttpTransport> AccountingProvider for XeroProvider<T> {
    fn provider_type(&self) -> &'static str {
        "xero"
    }

    async fn sync_invoice(&self, invoice: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        let url = format!("{}/v1/xero/invoices", self.base_url.trim_end_matches('/'));
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        // Xero `ACCREC` (accounts-receivable) invoice.
        let body = json!({
            "Type": "ACCREC",
            "InvoiceNumber": invoice.number,
            "CurrencyCode": invoice.currency,
            "Contact": { "ContactNumber": invoice.customer_external_id },
            "Total": invoice.total_amount_cents,
            "Status": "AUTHORISED",
            "lago_id": invoice.invoice_id,
        })
        .to_string();

        let resp = self.transport.post(&url, &headers, body).await?;
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("code").and_then(Value::as_str).map(str::to_string),
                message: json.get("error").and_then(Value::as_str).unwrap_or("xero error").to_string(),
            });
        }
        // Xero returns `{ "Invoices": [ { "InvoiceID": "..." } ] }`; the
        // aggregator also surfaces `succeededId`.
        let external_id = json
            .get("succeededId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                json.get("Invoices")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|i| i.get("InvoiceID"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| IntegrationError::InvalidResponse("missing InvoiceID".into()))?;
        Ok(SyncResult { external_id })
    }
}
