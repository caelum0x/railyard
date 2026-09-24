//! HubSpot CRM integration — port of
//! `Integrations::Aggregator::{Contacts,Companies}::{Create,Update}Service`,
//! `Integrations::Hubspot::{Invoices,Subscriptions}::DeployObjectService` and
//! `Integrations::Aggregator::{Invoices,Subscriptions}::Hubspot::*Service`.
//!
//! * Customers sync as a HubSpot contact or company (the integration's
//!   `default_targeted_object`); a re-sync updates the linked record instead of
//!   creating a duplicate.
//! * Invoices and subscriptions sync as the `LagoInvoices` / `LagoSubscriptions`
//!   custom objects (deployed on first use; the object type id is returned so
//!   the caller can store it on the integration), created or updated, and
//!   associated to the customer's contact/company.
//!
//! Two transports: **direct** (HubSpot CRM v3/v4 APIs with the OAuth access
//! token) and **aggregator** (`.../v1/hubspot/*`).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    provider_error, CrmCustomer, CrmInvoice, CrmProvider, CrmRecordSync, CrmSubscription, IntegrationError,
    SyncResult,
};
use crate::transport::HttpTransport;

/// HubSpot API root.
pub const HUBSPOT_API_BASE: &str = "https://api.hubapi.com";

/// The custom object schemas Rago deploys to a HubSpot portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubspotObject {
    Invoices,
    Subscriptions,
}

impl HubspotObject {
    /// Schema name (unique per portal).
    pub fn schema_name(self) -> &'static str {
        match self {
            Self::Invoices => "LagoInvoices",
            Self::Subscriptions => "LagoSubscriptions",
        }
    }

    /// Integration setting holding the deployed object type id.
    pub fn setting_key(self) -> &'static str {
        match self {
            Self::Invoices => "invoices_object_type_id",
            Self::Subscriptions => "subscriptions_object_type_id",
        }
    }

    fn aggregator_path(self) -> &'static str {
        match self {
            Self::Invoices => "invoices",
            Self::Subscriptions => "subscriptions",
        }
    }

    fn schema(self) -> Value {
        let prop = |name: &str, label: &str, kind: &str| {
            let field_type = match kind {
                "number" => "number",
                "date" => "date",
                _ => "text",
            };
            json!({ "name": name, "label": label, "type": kind, "fieldType": field_type })
        };
        match self {
            Self::Invoices => json!({
                "name": self.schema_name(),
                "description": "Invoices issued by Rago billing",
                "labels": { "singular": "Lago Invoice", "plural": "Lago Invoices" },
                "primaryDisplayProperty": "lago_invoice_number",
                "secondaryDisplayProperties": ["lago_invoice_status", "lago_invoice_total_amount"],
                "searchableProperties": ["lago_invoice_number", "lago_invoice_id"],
                "requiredProperties": ["lago_invoice_id"],
                "associatedObjects": ["COMPANY", "CONTACT"],
                "properties": [
                    { "name": "lago_invoice_id", "label": "Lago Invoice Id", "type": "string", "fieldType": "text", "hasUniqueValue": true },
                    prop("lago_invoice_number", "Number", "string"),
                    prop("lago_invoice_type", "Type", "string"),
                    prop("lago_invoice_status", "Status", "string"),
                    prop("lago_invoice_payment_status", "Payment status", "string"),
                    prop("lago_invoice_currency", "Currency", "string"),
                    prop("lago_invoice_issuing_date", "Issuing date", "date"),
                    prop("lago_invoice_payment_due_date", "Payment due date", "date"),
                    prop("lago_invoice_subtotal_excluding_taxes", "Sub-total excluding taxes", "number"),
                    prop("lago_invoice_taxes_amount", "Taxes", "number"),
                    prop("lago_invoice_total_amount", "Total", "number"),
                ],
            }),
            Self::Subscriptions => json!({
                "name": self.schema_name(),
                "description": "Subscriptions managed by Rago billing",
                "labels": { "singular": "Lago Subscription", "plural": "Lago Subscriptions" },
                "primaryDisplayProperty": "lago_external_subscription_id",
                "secondaryDisplayProperties": ["lago_subscription_status", "lago_subscription_plan_code"],
                "searchableProperties": ["lago_external_subscription_id", "lago_subscription_id"],
                "requiredProperties": ["lago_subscription_id"],
                "associatedObjects": ["COMPANY", "CONTACT"],
                "properties": [
                    { "name": "lago_subscription_id", "label": "Lago Subscription Id", "type": "string", "fieldType": "text", "hasUniqueValue": true },
                    prop("lago_external_subscription_id", "External id", "string"),
                    prop("lago_subscription_name", "Name", "string"),
                    prop("lago_subscription_plan_code", "Plan code", "string"),
                    prop("lago_subscription_status", "Status", "string"),
                    prop("lago_billing_time", "Billing time", "string"),
                    prop("lago_subscription_at", "Subscription date", "date"),
                    prop("lago_subscription_started_at", "Started at", "date"),
                    prop("lago_subscription_ending_at", "Ending at", "date"),
                ],
            }),
        }
    }
}

fn cents(v: i64) -> f64 {
    (v as f64) / 100.0
}

/// HubSpot record properties of an invoice.
fn invoice_properties(i: &CrmInvoice) -> Value {
    json!({
        "lago_invoice_id": i.invoice_id,
        "lago_invoice_number": i.number,
        "lago_invoice_type": i.invoice_type,
        "lago_invoice_status": i.status,
        "lago_invoice_payment_status": i.payment_status,
        "lago_invoice_currency": i.currency,
        "lago_invoice_issuing_date": i.issuing_date,
        "lago_invoice_payment_due_date": i.payment_due_date,
        "lago_invoice_subtotal_excluding_taxes": cents(i.sub_total_excluding_taxes_amount_cents),
        "lago_invoice_taxes_amount": cents(i.taxes_amount_cents),
        "lago_invoice_total_amount": cents(i.total_amount_cents),
    })
}

/// HubSpot record properties of a subscription.
fn subscription_properties(s: &CrmSubscription) -> Value {
    json!({
        "lago_subscription_id": s.subscription_id,
        "lago_external_subscription_id": s.external_id,
        "lago_subscription_name": s.name,
        "lago_subscription_plan_code": s.plan_code,
        "lago_subscription_status": s.status,
        "lago_billing_time": s.billing_time,
        "lago_subscription_at": s.subscription_at,
        "lago_subscription_started_at": s.started_at,
        "lago_subscription_ending_at": s.ending_at,
    })
}

/// The existing record id HubSpot reports on a duplicate-email conflict
/// (`"Contact already exists. Existing ID: 12345"`).
fn conflict_existing_id(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    let message = json.get("message").and_then(Value::as_str)?;
    let (_, tail) = message.split_once("Existing ID:")?;
    let id: String = tail.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    (!id.is_empty()).then_some(id)
}

fn record_id(body: &str) -> Result<String, IntegrationError> {
    let json: Value = serde_json::from_str(body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
    json.get("succeededId")
        .or_else(|| json.get("id"))
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string())))
        .ok_or_else(|| IntegrationError::InvalidResponse("missing id".into()))
}

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

    /// The CRM object customers are mirrored as (`contacts` / `companies`).
    pub fn customer_object(&self) -> &str {
        match &self.mode {
            Mode::Direct { targeted_object } => targeted_object,
            Mode::Aggregator { .. } => "contacts",
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    fn customer_properties(&self, customer: &CrmCustomer) -> Value {
        match &self.mode {
            // Standard properties only: custom properties would 400 on a
            // portal that has not defined them.
            Mode::Direct { targeted_object } if targeted_object == "companies" => json!({
                "name": customer.name.clone().unwrap_or_else(|| customer.external_id.clone()),
                "description": format!("Rago customer {}", customer.external_id),
            }),
            Mode::Direct { .. } => json!({
                "email": customer.email,
                "lastname": customer.name.clone().unwrap_or_else(|| customer.external_id.clone()),
                "company": customer.name,
            }),
            Mode::Aggregator { .. } => json!({
                "email": customer.email,
                "lago_customer_external_id": customer.external_id,
                "company": customer.name,
            }),
        }
    }

    /// PATCH a direct-mode record; `Ok(None)` when it no longer exists.
    async fn patch_record(&self, object: &str, id: &str, properties: Value) -> Result<Option<String>, IntegrationError> {
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/{object}/{id}");
        let resp = self.transport.patch(&url, &self.headers(), json!({ "properties": properties }).to_string()).await?;
        if resp.status == 404 {
            return Ok(None);
        }
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "hubspot error"));
        }
        record_id(&resp.body).map(Some)
    }

    /// Ensure the custom object schema exists in the portal and return its
    /// object type id (`2-12345`). Direct mode only; the aggregator deploys
    /// its own objects.
    pub async fn ensure_object_type(&self, object: HubspotObject) -> Result<Option<String>, IntegrationError> {
        if matches!(self.mode, Mode::Aggregator { .. }) {
            return Ok(None);
        }
        if let Some(id) = self.find_object_type(object).await? {
            return Ok(Some(id));
        }
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/schemas");
        let resp = self.transport.post(&url, &self.headers(), object.schema().to_string()).await?;
        if resp.status == 409 {
            // Deployed concurrently (or under another app): read it back.
            return self
                .find_object_type(object)
                .await?
                .map(Some)
                .ok_or_else(|| provider_error(&resp.body, "hubspot schema conflict"));
        }
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "hubspot schema error"));
        }
        let json: Value = serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        json.get("objectTypeId")
            .and_then(Value::as_str)
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| IntegrationError::InvalidResponse("missing objectTypeId".into()))
    }

    async fn find_object_type(&self, object: HubspotObject) -> Result<Option<String>, IntegrationError> {
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/schemas");
        let resp = self.transport.get(&url, &self.headers()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "hubspot schema lookup error"));
        }
        let json: Value = serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        let name = object.schema_name().to_ascii_lowercase();
        Ok(json
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("name").and_then(Value::as_str).map(str::to_ascii_lowercase).as_deref() == Some(name.as_str()))
            .and_then(|s| s.get("objectTypeId").and_then(Value::as_str))
            .map(str::to_string))
    }

    /// Create or update one custom-object record and (direct mode) associate
    /// it to the customer's contact/company.
    async fn upsert_object(
        &self,
        object: HubspotObject,
        object_type_id: Option<&str>,
        existing_id: Option<&str>,
        properties: Value,
        lago_id: &str,
        customer_record_id: Option<&str>,
    ) -> Result<CrmRecordSync, IntegrationError> {
        let headers = self.headers();
        match &self.mode {
            Mode::Aggregator { base_url } => {
                let url = format!("{}/v1/hubspot/{}", base_url.trim_end_matches('/'), object.aggregator_path());
                let body = json!({
                    "objectId": existing_id,
                    "input": { "properties": properties, "associations": customer_record_id.map(|id| json!([{ "id": id }])) },
                    "lago_id": lago_id,
                })
                .to_string();
                let resp = match existing_id {
                    Some(_) => self.transport.put(&url, &headers, body).await?,
                    None => self.transport.post(&url, &headers, body).await?,
                };
                if !resp.is_success() {
                    return Err(provider_error(&resp.body, "hubspot error"));
                }
                Ok(CrmRecordSync { external_id: record_id(&resp.body)?, association_error: None })
            }
            Mode::Direct { targeted_object } => {
                let type_id = object_type_id
                    .ok_or_else(|| IntegrationError::InvalidResponse("hubspot custom object not deployed".into()))?;
                if let Some(id) = existing_id {
                    if let Some(updated) = self.patch_record(type_id, id, properties.clone()).await? {
                        return Ok(CrmRecordSync { external_id: updated, association_error: None });
                    }
                }
                let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/{type_id}");
                let resp = self.transport.post(&url, &headers, json!({ "properties": properties }).to_string()).await?;
                if !resp.is_success() {
                    return Err(provider_error(&resp.body, "hubspot error"));
                }
                let external_id = record_id(&resp.body)?;
                let association_error = match customer_record_id {
                    Some(to) => {
                        let url = format!(
                            "{HUBSPOT_API_BASE}/crm/v4/objects/{type_id}/{external_id}/associations/default/{targeted_object}/{to}"
                        );
                        match self.transport.put(&url, &headers, String::new()).await {
                            Ok(r) if r.is_success() => None,
                            Ok(r) => Some(provider_error(&r.body, "hubspot association error")),
                            Err(e) => Some(IntegrationError::Transport(e)),
                        }
                    }
                    None => None,
                };
                Ok(CrmRecordSync { external_id, association_error })
            }
        }
    }

    /// Create or update the `LagoInvoices` record of an invoice.
    pub async fn sync_invoice(
        &self,
        object_type_id: Option<&str>,
        existing_id: Option<&str>,
        invoice: &CrmInvoice,
        customer_record_id: Option<&str>,
    ) -> Result<CrmRecordSync, IntegrationError> {
        self.upsert_object(
            HubspotObject::Invoices,
            object_type_id,
            existing_id,
            invoice_properties(invoice),
            &invoice.invoice_id,
            customer_record_id,
        )
        .await
    }

    /// Create or update the `LagoSubscriptions` record of a subscription.
    pub async fn sync_subscription(
        &self,
        object_type_id: Option<&str>,
        existing_id: Option<&str>,
        subscription: &CrmSubscription,
        customer_record_id: Option<&str>,
    ) -> Result<CrmRecordSync, IntegrationError> {
        self.upsert_object(
            HubspotObject::Subscriptions,
            object_type_id,
            existing_id,
            subscription_properties(subscription),
            &subscription.subscription_id,
            customer_record_id,
        )
        .await
    }
}

#[async_trait]
impl<T: HttpTransport> CrmProvider for HubspotProvider<T> {
    fn provider_type(&self) -> &'static str {
        "hubspot"
    }

    async fn sync_customer(&self, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        let headers = self.headers();
        let properties = self.customer_properties(customer);
        let (url, body) = match &self.mode {
            Mode::Direct { targeted_object } => (
                format!("{HUBSPOT_API_BASE}/crm/v3/objects/{targeted_object}"),
                json!({ "properties": properties }),
            ),
            Mode::Aggregator { base_url } => (
                format!("{}/v1/hubspot/contacts", base_url.trim_end_matches('/')),
                json!({ "properties": properties, "lago_id": customer.customer_id }),
            ),
        };
        let resp = self.transport.post(&url, &headers, body.to_string()).await?;
        if resp.status == 409 {
            // A contact with this email already exists: adopt and update it
            // instead of failing the sync.
            if let (Mode::Direct { targeted_object }, Some(existing)) = (&self.mode, conflict_existing_id(&resp.body)) {
                if let Some(id) = self.patch_record(targeted_object, &existing, properties).await? {
                    return Ok(SyncResult { external_id: id });
                }
            }
        }
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "hubspot error"));
        }
        Ok(SyncResult { external_id: record_id(&resp.body)? })
    }

    async fn update_customer(&self, external_id: &str, customer: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        match &self.mode {
            Mode::Direct { targeted_object } => {
                match self.patch_record(targeted_object, external_id, self.customer_properties(customer)).await? {
                    Some(id) => Ok(SyncResult { external_id: id }),
                    // Deleted in HubSpot: mirror the customer again.
                    None => self.sync_customer(customer).await,
                }
            }
            Mode::Aggregator { base_url } => {
                let url = format!("{}/v1/hubspot/contacts", base_url.trim_end_matches('/'));
                let body = json!({
                    "contactId": external_id,
                    "properties": self.customer_properties(customer),
                    "lago_id": customer.customer_id,
                });
                let resp = self.transport.put(&url, &self.headers(), body.to_string()).await?;
                if resp.status == 404 {
                    return self.sync_customer(customer).await;
                }
                if !resp.is_success() {
                    return Err(provider_error(&resp.body, "hubspot error"));
                }
                Ok(SyncResult { external_id: record_id(&resp.body).unwrap_or_else(|_| external_id.to_string()) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_id_from_conflict() {
        let body = r#"{"status":"error","message":"Contact already exists. Existing ID: 5012","category":"CONFLICT"}"#;
        assert_eq!(conflict_existing_id(body).as_deref(), Some("5012"));
        assert_eq!(conflict_existing_id(r#"{"message":"other"}"#), None);
    }
}
