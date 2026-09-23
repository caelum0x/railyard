//! HubSpot CRM integration — port of
//! `Integrations::Aggregator::{Contacts,Companies}::CreateService` (HubSpot
//! path).
//!
//! Syncs a customer as a HubSpot contact or company (the integration's
//! `default_targeted_object`) and returns the external record id. Two
//! transports: **direct** (HubSpot CRM v3 API with the OAuth access token) and
//! **aggregator** (`.../v1/hubspot/contacts`).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{provider_error, CrmCustomer, CrmProvider, IntegrationError, SyncResult};
use crate::transport::HttpTransport;

/// HubSpot API root.
pub const HUBSPOT_API_BASE: &str = "https://api.hubapi.com";

enum Mode {
    /// `companies` or `contacts`.
    Direct { targeted_object: String },
    Aggregator { base_url: String },
}

pub struct HubspotProvider<T: HttpTransport> {
    mode: Mode,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> HubspotProvider<T> {
    /// Aggregator mode.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Aggregator { base_url: base_url.into() }, api_key: api_key.into(), transport }
    }

    /// Direct mode (`targeted_object`: `companies` or `contacts`, default
    /// `contacts`).
    pub fn direct(access_token: impl Into<String>, targeted_object: Option<&str>, transport: T) -> Self {
        let targeted_object = match targeted_object.map(str::to_ascii_lowercase).as_deref() {
            Some("companies") | Some("company") => "companies".to_string(),
            _ => "contacts".to_string(),
        };
        Self { mode: Mode::Direct { targeted_object }, api_key: access_token.into(), transport }
    }
}

#[async_trait]
impl<T: HttpTransport> CrmProvider for HubspotProvider<T> {
    fn provider_type(&self) -> &'static str {
        "hubspot"
    }

    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let (url, body) = match &self.mode {
            Mode::Direct { targeted_object } if targeted_object == "companies" => (
                format!("{HUBSPOT_API_BASE}/crm/v3/objects/companies"),
                // Standard properties only: custom properties would 400 on a
                // portal that has not defined them.
                json!({ "properties": {
                    "name": customer.name.clone().unwrap_or_else(|| customer.external_id.clone()),
                    "description": format!("Rago customer {}", customer.external_id),
                } }),
            ),
            Mode::Direct { .. } => (
                format!("{HUBSPOT_API_BASE}/crm/v3/objects/contacts"),
                json!({ "properties": {
                    "email": customer.email,
                    "lastname": customer.name.clone().unwrap_or_else(|| customer.external_id.clone()),
                    "company": customer.name,
                } }),
            ),
            Mode::Aggregator { base_url } => (
                format!("{}/v1/hubspot/contacts", base_url.trim_end_matches('/')),
                json!({
                    "properties": {
                        "email": customer.email,
                        "lago_customer_external_id": customer.external_id,
                        "company": customer.name,
                    },
                    "lago_id": customer.customer_id,
                }),
            ),
        };

        let resp = self.transport.post(&url, &headers, body.to_string()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "hubspot error"));
        }
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        let external_id = json
            .get("succeededId")
            .or_else(|| json.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| IntegrationError::InvalidResponse("missing id".into()))?
            .to_string();
        Ok(SyncResult { external_id })
    }
}
