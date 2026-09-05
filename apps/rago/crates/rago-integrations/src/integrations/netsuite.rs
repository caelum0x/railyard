//! NetSuite accounting integration — representative port of
//! `Integrations::Aggregator::Invoices::CreateService` (NetSuite path).
//!
//! Syncs a finalized invoice through the aggregator (`.../v1/netsuite/invoices`)
//! and returns the external record id.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{AccountingInvoice, AccountingProvider, IntegrationError, SyncResult};
use crate::transport::HttpTransport;

pub struct NetsuiteProvider<T: HttpTransport> {
    base_url: String,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> NetsuiteProvider<T> {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            transport,
        }
    }
}

#[async_trait]
impl<T: HttpTransport> AccountingProvider for NetsuiteProvider<T> {
    fn provider_type(&self) -> &'static str {
        "netsuite"
    }

    async fn sync_invoice(&self, invoice: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        let url = format!("{}/v1/netsuite/invoices", self.base_url.trim_end_matches('/'));
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let body = json!({
            "type": "invoice",
            "isDynamic": true,
            "columns": {
                "tranid": invoice.number,
                "entity": invoice.customer_external_id,
                "currency": invoice.currency,
                "total": invoice.total_amount_cents,
            },
            "lago_id": invoice.invoice_id,
        })
        .to_string();

        let resp = self.transport.post(&url, &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("code").and_then(Value::as_str).map(str::to_string),
                message: json.get("error").and_then(Value::as_str).unwrap_or("netsuite error").to_string(),
            });
        }
        let external_id = json
            .get("succeededId")
            .or_else(|| json.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| IntegrationError::InvalidResponse("missing succeededId".into()))?
            .to_string();
        Ok(SyncResult { external_id })
    }
}
