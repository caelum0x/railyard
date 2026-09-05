//! Salesforce CRM integration — representative port of
//! `Integrations::Aggregator::Contacts::CreateService` (Salesforce path).
//!
//! Syncs a customer as a Salesforce `Contact` sObject and returns the external
//! contact id.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{CrmCustomer, CrmProvider, IntegrationError, SyncResult};
use crate::transport::HttpTransport;

pub struct SalesforceProvider<T: HttpTransport> {
    base_url: String,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> SalesforceProvider<T> {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            transport,
        }
    }
}

#[async_trait]
impl<T: HttpTransport> CrmProvider for SalesforceProvider<T> {
    fn provider_type(&self) -> &'static str {
        "salesforce"
    }

    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        let url = format!("{}/v1/salesforce/contacts", self.base_url.trim_end_matches('/'));
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        // Salesforce `Contact` sObject fields.
        let body = json!({
            "sobject": "Contact",
            "fields": {
                "Email": customer.email,
                "LastName": customer.name.clone().unwrap_or_else(|| customer.external_id.clone()),
                "Lago_External_Id__c": customer.external_id,
            },
            "lago_id": customer.customer_id,
        })
        .to_string();

        let resp = self.transport.post(&url, &headers, body).await?;
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("code").and_then(Value::as_str).map(str::to_string),
                message: json.get("error").and_then(Value::as_str).unwrap_or("salesforce error").to_string(),
            });
        }
        // Salesforce returns `{ "id": "003...", "success": true }`; aggregator
        // also surfaces `succeededId`.
        let external_id = json
            .get("succeededId")
            .or_else(|| json.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| IntegrationError::InvalidResponse("missing id".into()))?
            .to_string();
        Ok(SyncResult { external_id })
    }
}
