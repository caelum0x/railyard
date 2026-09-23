//! NetSuite accounting integration — port of the
//! `Integrations::Aggregator::{Contacts,Invoices,CreditNotes}::CreateService`
//! NetSuite path plus the item / account / subsidiary catalog fetch.
//!
//! Two transports:
//! * **RESTlet (direct)** — the organization's deployed Lago RESTlet script
//!   (`script_endpoint_url`) called with OAuth 1.0 Token-Based Authentication
//!   signed from the integration's consumer key/secret + token id/secret. The
//!   catalog is read through NetSuite's SuiteQL REST API with the same TBA
//!   credentials.
//! * **aggregator** — the legacy `.../v1/netsuite/*` aggregator endpoints with
//!   a bearer key (kept for deployments that front NetSuite with one).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::oauth1::{authorization_header, Oauth1Credentials};
use super::{
    parse_catalog, provider_error, AccountingContact, AccountingInvoice, AccountingProvider, CatalogEntry,
    IntegrationError, SyncResult,
};
use crate::transport::HttpTransport;

/// NetSuite Token-Based Authentication credentials of one integration.
#[derive(Debug, Clone)]
pub struct NetsuiteCredentials {
    /// NetSuite account id (`1234567` or `1234567_SB1` for a sandbox).
    pub account_id: String,
    /// Integration record consumer key / secret.
    pub client_id: String,
    pub client_secret: String,
    /// Access token id / secret.
    pub token_id: String,
    pub token_secret: String,
    /// The deployed RESTlet URL (`.../restlet.nl?script=..&deploy=..`).
    pub script_endpoint_url: String,
}

impl NetsuiteCredentials {
    fn oauth1(&self) -> Oauth1Credentials {
        Oauth1Credentials {
            realm: self.account_id.trim().to_ascii_uppercase().replace('-', "_"),
            consumer_key: self.client_id.clone(),
            consumer_secret: self.client_secret.clone(),
            token_id: self.token_id.clone(),
            token_secret: self.token_secret.clone(),
        }
    }

    /// `https://<account>.suitetalk.api.netsuite.com` (account id lowercased,
    /// `_` → `-`).
    fn suitetalk_host(&self) -> String {
        format!(
            "https://{}.suitetalk.api.netsuite.com",
            self.account_id.trim().to_ascii_lowercase().replace('_', "-")
        )
    }
}

enum Mode {
    Restlet(NetsuiteCredentials),
    Aggregator { base_url: String, api_key: String },
}

pub struct NetsuiteProvider<T: HttpTransport> {
    mode: Mode,
    transport: T,
}

fn cents(v: i64) -> f64 {
    (v as f64) / 100.0
}

impl<T: HttpTransport> NetsuiteProvider<T> {
    /// Aggregator mode (`{base_url}/v1/netsuite/*` with a bearer key).
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self { mode: Mode::Aggregator { base_url: base_url.into(), api_key: api_key.into() }, transport }
    }

    /// Direct mode: the org's RESTlet + SuiteQL over OAuth 1.0 TBA.
    pub fn restlet(credentials: NetsuiteCredentials, transport: T) -> Self {
        Self { mode: Mode::Restlet(credentials), transport }
    }

    async fn post_record(&self, record_type: &str, body: Value) -> Result<SyncResult, IntegrationError> {
        let (url, headers) = match &self.mode {
            Mode::Restlet(c) => {
                let url = c.script_endpoint_url.clone();
                let auth = authorization_header(&c.oauth1(), "POST", &url);
                (url, vec![("Authorization".to_string(), auth), ("Content-Type".to_string(), "application/json".to_string())])
            }
            Mode::Aggregator { base_url, api_key } => {
                let path = match record_type {
                    "invoice" => "invoices",
                    "creditmemo" => "creditnotes",
                    "customer" => "contacts",
                    other => other,
                };
                (
                    format!("{}/v1/netsuite/{path}", base_url.trim_end_matches('/')),
                    vec![
                        ("Authorization".to_string(), format!("Bearer {api_key}")),
                        ("Content-Type".to_string(), "application/json".to_string()),
                    ],
                )
            }
        };
        let resp = self.transport.post(&url, &headers, body.to_string()).await?;
        if !resp.is_success() {
            return Err(provider_error(&resp.body, "netsuite error"));
        }
        // The RESTlet returns the new record's internal id (a bare number or
        // string); the aggregator wraps it as `succeededId` / `id`.
        let parsed: Value = serde_json::from_str(resp.body.trim()).unwrap_or(Value::String(resp.body.trim().to_string()));
        let external_id = match &parsed {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) if !s.is_empty() => Some(s.trim_matches('"').to_string()),
            Value::Object(_) => parsed
                .get("succeededId")
                .or_else(|| parsed.get("id"))
                .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string()))),
            _ => None,
        }
        .ok_or_else(|| IntegrationError::InvalidResponse("missing NetSuite record id".into()))?;
        Ok(SyncResult { external_id })
    }

    fn transaction_body(record_type: &str, doc: &AccountingInvoice) -> Value {
        let entity = doc.contact_external_id.clone().unwrap_or_else(|| doc.customer_external_id.clone());
        let lines: Vec<Value> = doc
            .lines
            .iter()
            .map(|l| {
                json!({
                    "item": l.item_external_id,
                    "account": l.account_code,
                    "quantity": l.units,
                    "rate": cents(l.unit_amount_cents),
                    "amount": cents(l.amount_cents),
                    "taxamount": cents(l.taxes_amount_cents),
                    "description": l.description,
                    "custcol_lago_fee_id": l.lago_id,
                })
            })
            .collect();
        json!({
            "type": record_type,
            "isDynamic": true,
            "columns": {
                "tranid": doc.number,
                "entity": entity,
                "currency": doc.currency,
                "trandate": doc.issuing_date,
                "duedate": doc.due_date,
                "taxregoverride": true,
                "taxdetailsoverride": true,
                "custbody_lago_id": doc.invoice_id,
                "custbody_ava_disable_tax_calculation": true,
            },
            "lines": [{ "sublistId": "item", "lineItems": lines }],
            "options": { "ignoreMandatoryFields": false },
            "total": doc.total_amount_cents,
            "lago_id": doc.invoice_id,
        })
    }

    /// Sync a credit note as a NetSuite `creditmemo`.
    pub async fn sync_credit_note(&self, note: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        self.post_record("creditmemo", Self::transaction_body("creditmemo", note)).await
    }

    /// Create the customer as a NetSuite `customer` record.
    pub async fn create_contact(&self, contact: &AccountingContact) -> Result<SyncResult, IntegrationError> {
        let body = json!({
            "type": "customer",
            "isDynamic": false,
            "columns": {
                "companyname": contact.name.clone().unwrap_or_else(|| contact.external_id.clone()),
                "subsidiary": contact.subsidiary_id,
                "custentity_lago_id": contact.customer_id,
                "custentity_lago_customer_id": contact.external_id,
                "email": contact.email,
                "vatregnumber": contact.tax_identification_number,
            },
            "options": { "ignoreMandatoryFields": false },
            "lago_id": contact.customer_id,
        });
        self.post_record("customer", body).await
    }

    /// Fetch one catalog (`item`, `account`, `subsidiary`) through SuiteQL
    /// (direct mode) or the aggregator.
    pub async fn fetch_catalog(&self, kind: &str) -> Result<Vec<CatalogEntry>, IntegrationError> {
        match &self.mode {
            Mode::Restlet(c) => {
                let q = match kind {
                    "item" => "SELECT id, itemid AS name, NULL AS code FROM item WHERE isinactive = 'F'",
                    "account" => "SELECT id, accountsearchdisplayname AS name, acctnumber AS code FROM account WHERE isinactive = 'F'",
                    "subsidiary" => "SELECT id, name, NULL AS code FROM subsidiary WHERE isinactive = 'F'",
                    other => return Err(IntegrationError::InvalidResponse(format!("unsupported catalog: {other}"))),
                };
                let url = format!("{}/services/rest/query/v1/suiteql?limit=1000", c.suitetalk_host());
                let headers = vec![
                    ("Authorization".to_string(), authorization_header(&c.oauth1(), "POST", &url)),
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("Prefer".to_string(), "transient".to_string()),
                ];
                let resp = self.transport.post(&url, &headers, json!({ "q": q }).to_string()).await?;
                if !resp.is_success() {
                    return Err(provider_error(&resp.body, "netsuite suiteql error"));
                }
                parse_catalog(&resp.body, &["items"], &["id"], &["name"], &["code"])
            }
            Mode::Aggregator { base_url, api_key } => {
                let path = match kind {
                    "item" => "items",
                    "account" => "accounts",
                    "subsidiary" => "subsidiaries",
                    other => return Err(IntegrationError::InvalidResponse(format!("unsupported catalog: {other}"))),
                };
                let url = format!("{}/v1/netsuite/{path}", base_url.trim_end_matches('/'));
                let headers = vec![("Authorization".to_string(), format!("Bearer {api_key}"))];
                let resp = self.transport.get(&url, &headers).await?;
                if !resp.is_success() {
                    return Err(provider_error(&resp.body, "netsuite catalog error"));
                }
                parse_catalog(&resp.body, &["records", "items", "accounts", "subsidiaries"], &["id", "external_id"], &["name", "external_name", "itemid"], &["external_account_code", "acctnumber"])
            }
        }
    }
}

#[async_trait]
impl<T: HttpTransport> AccountingProvider for NetsuiteProvider<T> {
    fn provider_type(&self) -> &'static str {
        "netsuite"
    }

    async fn sync_invoice(&self, invoice: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        self.post_record("invoice", Self::transaction_body("invoice", invoice)).await
    }
}
