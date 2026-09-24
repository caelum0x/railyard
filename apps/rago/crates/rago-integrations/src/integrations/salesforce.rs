//! Salesforce CRM integration — port of
//! `Integrations::Aggregator::Contacts::CreateService` (Salesforce path).
//!
//! Syncs a customer as a Salesforce `Contact` sObject and returns the external
//! contact id; a re-sync updates the linked contact (`PATCH .../Contact/{id}`)
//! instead of creating a duplicate. Two transports: **direct** (the org's `instance_url` from the
//! OAuth token response + the REST sObjects API) and **aggregator**
//! (`.../v1/salesforce/contacts`).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{provider_error, CrmCustomer, CrmProvider, IntegrationError, SyncResult};
use crate::transport::HttpTransport;

/// Salesforce REST API version used for sObject calls.
pub const SALESFORCE_API_VERSION: &str = "v59.0";

enum Mode {
    Direct { instance_url: String },
    Aggregator { base_url: String },
}

pub struct SalesforceProvider<T: HttpTransport> {
    mode: Mode,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> SalesforceProvider<T> {
    /// Aggregator mode.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Aggregator { base_url: base_url.into() }, api_key: api_key.into(), transport }
    }

    /// Direct mode against the org's Salesforce instance.
    pub fn direct(access_token: impl Into<String>, instance_url: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Direct { instance_url: instance_url.into() }, api_key: access_token.into(), transport }
    }
}

#[async_trait]
impl<T: HttpTransport> CrmProvider for SalesforceProvider<T> {
    fn provider_type(&self) -> &'static str {
        "salesforce"
    }

    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let last_name = customer.name.clone().unwrap_or_else(|| customer.external_id.clone());
        let fields = json!({
            "Email": customer.email,
            "LastName": last_name,
            "Lago_External_Id__c": customer.external_id,
        });
        let (url, body) = match &self.mode {
            // Standard fields only: a custom field would fail with
            // INVALID_FIELD on an org without the Lago package.
            Mode::Direct { instance_url } => (
                format!(
                    "{}/services/data/{SALESFORCE_API_VERSION}/sobjects/Contact",
                    instance_url.trim_end_matches('/')
                ),
                json!({
                    "Email": customer.email,
                    "LastName": last_name,
                    "Description": format!("Rago customer {}", customer.external_id),
                }),
            ),
            Mode::Aggregator { base_url } => (
                format!("{}/v1/salesforce/contacts", base_url.trim_end_matches('/')),
                json!({ "sobject": "Contact", "fields": fields, "lago_id": customer.customer_id }),
            ),
        };

        let resp = self.transport.post(&url, &headers, body.to_string()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "salesforce error"));
        }
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
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

    async fn update_customer(&self, external_id: &str, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let last_name = customer.name.clone().unwrap_or_else(|| customer.external_id.clone());
        let (url, body) = match &self.mode {
            Mode::Direct { instance_url } => (
                format!(
                    "{}/services/data/{SALESFORCE_API_VERSION}/sobjects/Contact/{external_id}",
                    instance_url.trim_end_matches('/')
                ),
                json!({
                    "Email": customer.email,
                    "LastName": last_name,
                    "Description": format!("Rago customer {}", customer.external_id),
                }),
            ),
            Mode::Aggregator { base_url } => (
                format!("{}/v1/salesforce/contacts", base_url.trim_end_matches('/')),
                json!({
                    "sobject": "Contact",
                    "id": external_id,
                    "fields": {
                        "Email": customer.email,
                        "LastName": last_name,
                        "Lago_External_Id__c": customer.external_id,
                    },
                    "lago_id": customer.customer_id,
                }),
            ),
        };
        let resp = self.transport.patch(&url, &headers, body.to_string()).await?;
        // Deleted in Salesforce (`NOT_FOUND` / `ENTITY_IS_DELETED`): mirror the
        // customer again as a new contact.
        if resp.status == 404 {
            return self.sync_customer(customer).await;
        }
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "salesforce error"));
        }
        // The sObject PATCH answers `204 No Content`.
        Ok(SyncResult { external_id: external_id.to_string() })
    }
}
