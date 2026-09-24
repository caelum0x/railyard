//! Integration round-trips shared by the REST handlers and the durable job
//! queue: provider taxes (Anrok / Avalara), accounting sync (Xero / NetSuite
//! contacts, invoices and credit notes), CRM sync (Salesforce / HubSpot), tax
//! voiding, credit-note tax reporting, the provider catalog cache that feeds
//! the mapping pickers, and the auto-sync sweep behind the integrations'
//! `sync_invoices` / `sync_credit_notes` settings.
//!
//! Dispatch is by the organization's *active* integration of each type (the
//! newest live connection holding credentials), so an org connected to
//! NetSuite syncs to NetSuite and an org on HubSpot syncs to HubSpot. Removed
//! (soft-deleted) integrations are never used.
//!
//! Transports: accounting/CRM calls go **direct** to the provider API with the
//! connection's OAuth token (refreshed automatically before it expires) or
//! NetSuite TBA credentials; when `RAGO_ACCOUNTING_BASE_URL` / `RAGO_CRM_BASE_URL`
//! is set they go through that aggregator instead. Tax calls go through
//! `RAGO_TAX_BASE_URL`. Missing configuration is an explicit error, never a
//! fabricated provider response.
//!
//! Payment sync lives in [`crate::payment_sync`] and CRM sync (customers,
//! invoices, subscriptions) in [`crate::crm_sync`]; both are driven by the same
//! sweep ([`enqueue_auto_syncs`]).
//!
//! Failures emit the Lago-parity error webhooks (`integration.provider_error`,
//! `customer.accounting_provider_error`, `customer.crm_provider_error`,
//! `customer.tax_provider_error`); successes emit `invoice.resynced`,
//! `customer.accounting_provider_created` and `customer.crm_provider_created`.

use std::collections::HashMap;

use chrono::Utc;
use rago_billing::fees::{apply_provider_taxes, ProviderTaxLine};
use rago_core::enums::FeeType;
use rago_core::models::{Customer, Fee, Invoice};
use rago_db::integration_hub::{IntegrationConnection, IntegrationItem};
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{
    AccountingContact, AccountingInvoice, AccountingLine, AccountingPayment, AccountingProvider,
    AnrokTaxProvider, AvalaraTaxProvider, CatalogEntry, HttpTransport, IntegrationError, NetsuiteCredentials,
    NetsuiteProvider, SyncResult, TaxFeeItem, TaxProvider, TaxRequest, XeroProvider,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::integration_auth::fresh_access_token;
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// Job type: sync an invoice to the accounting integration.
pub const JOB_SYNC_INVOICE: &str = "integrations.sync_invoice";
/// Job type: sync a credit note to the accounting integration.
pub const JOB_SYNC_CREDIT_NOTE: &str = "integrations.sync_credit_note";
/// Job type: void an invoice's transaction at the tax provider.
pub const JOB_TAX_VOIDING: &str = "invoices.tax_provider_voiding";
/// Job type: report a credit note to the tax provider.
pub const JOB_CREDIT_NOTE_TAX_REPORTING: &str = "credit_notes.tax_reporting";

fn env_required(name: &'static str) -> Result<String, JobError> {
    env_opt(name).ok_or(JobError::NotConfigured(name))
}

pub(crate) fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub(crate) fn integration_err(e: impl std::fmt::Display) -> JobError {
    JobError::Integration(e.to_string())
}

/// `{ message, error_code }` of a provider failure (webhook payload shape).
pub(crate) fn provider_error_object(e: &IntegrationError) -> Value {
    match e {
        IntegrationError::Provider { code, message } => json!({ "message": message, "error_code": code }),
        other => json!({ "message": other.to_string(), "error_code": Value::Null }),
    }
}

/// Record an outbound webhook; a persistence failure is logged, never masks
/// the sync outcome.
pub(crate) async fn emit(db: &Db, org: Uuid, kind: WebhookKind, object: Value) {
    if let Err(e) = record_webhook(db, org, kind, object).await {
        tracing::warn!(organization_id = %org, error = %e, "integration webhook could not be recorded");
    }
}

/// `integration.provider_error` for a failed sync through `conn`.
pub(crate) async fn emit_provider_error(db: &Db, conn: &IntegrationConnection, action: &str, lago_id: Uuid, e: &IntegrationError) {
    emit(
        db,
        conn.organization_id,
        WebhookKind::IntegrationProviderError,
        json!({
            "lago_integration_id": conn.id,
            "provider": conn.provider_code,
            "provider_code": conn.code,
            "action": action,
            "lago_id": lago_id,
            "provider_error": provider_error_object(e),
        }),
    )
    .await;
}

// ---- tax -----------------------------------------------------------------------

/// Per-org tax credentials: `(api_key, avalara_company_code)`.
///
/// Uses the org's `anrok` / `avalara` integration (sealed `api_key_sealed` /
/// `license_key_sealed` in its settings), else the env fallback.
pub async fn tax_credentials(db: &Db, org: Uuid, provider_code: &str) -> Result<(String, Option<String>), JobError> {
    let provider = if provider_code == "avalara" { "avalara" } else { "anrok" };
    if let Some(i) = db.active_integration_by_provider(org, provider).await? {
        let sealed_key = if provider == "avalara" { "license_key" } else { "api_key" };
        let company = i.setting("company_code");
        if let Some(opened) = i.sealed_setting(sealed_key) {
            let key = opened.map_err(|e| JobError::Integration(format!("{provider}_credentials_unavailable: {e}")))?;
            return Ok((key, company));
        }
    }
    let key = env_required("RAGO_TAX_API_KEY")?;
    let company = env_opt("RAGO_AVALARA_COMPANY_CODE");
    Ok((key, company))
}

/// The org's connected tax provider code (`anrok` / `avalara`), if any.
pub async fn active_tax_provider(db: &Db, org: Uuid) -> Result<Option<String>, JobError> {
    Ok(db.active_integration_of_type(org, "tax").await?.map(|c| c.provider_code))
}

async fn customer_external_id(db: &Db, customer_id: Option<Uuid>) -> Result<String, JobError> {
    Ok(match customer_id {
        Some(cid) => db.customer_external_id(cid).await?.unwrap_or_default(),
        None => String::new(),
    })
}

async fn load_invoice(db: &Db, org: Uuid, invoice_id: Uuid) -> Result<Invoice, JobError> {
    db.find_invoice(invoice_id)
        .await?
        .filter(|i| i.organization_id == org)
        .ok_or_else(|| JobError::Integration("invoice_not_found".into()))
}

fn tax_request(invoice_id: String, currency: String, customer: String, fees: &[Fee]) -> TaxRequest {
    TaxRequest {
        invoice_id,
        currency,
        customer_external_id: customer,
        fees: fees
            .iter()
            .map(|f| TaxFeeItem {
                item_id: f.id.to_string(),
                item_code: f
                    .charge_id
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "subscription".into()),
                amount_cents: f.amount_cents,
            })
            .collect(),
    }
}

async fn emit_tax_error(db: &Db, org: Uuid, provider: &str, customer_id: Option<Uuid>, lago_id: Uuid, e: &IntegrationError) {
    emit(
        db,
        org,
        WebhookKind::CustomerTaxProviderError,
        json!({
            "lago_customer_id": customer_id,
            "external_customer_id": customer_external_id(db, customer_id).await.unwrap_or_default(),
            "tax_provider_code": provider,
            "lago_id": lago_id,
            "provider_error": provider_error_object(e),
        }),
    )
    .await;
}

/// `fee.tax_provider_error` for one fee whose tax could not be computed.
async fn emit_fee_tax_error(db: &Db, org: Uuid, provider: &str, invoice: &Invoice, fee: &Fee, error: Value) {
    emit(
        db,
        org,
        WebhookKind::FeeTaxProviderError,
        json!({
            "lago_id": fee.id,
            "lago_invoice_id": invoice.id,
            "lago_charge_id": fee.charge_id,
            "lago_subscription_id": fee.subscription_id,
            "item_code": fee.charge_id.map(|c| c.to_string()).unwrap_or_else(|| "subscription".into()),
            "amount_cents": fee.amount_cents,
            "amount_currency": fee.amount_currency,
            "tax_provider_code": provider,
            "provider_error": error,
        }),
    )
    .await;
}

/// Fetch the invoice's taxes from the tax provider (`anrok` default or
/// `avalara`), apply them to its fees, roll up and persist the invoice totals.
pub async fn apply_provider_taxes_to_invoice<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
    provider_code: &str,
) -> Result<(Invoice, Vec<Fee>), JobError> {
    let base_url = env_required("RAGO_TAX_BASE_URL")?;
    let (api_key, company_code) = tax_credentials(db, org, provider_code).await?;
    let mut invoice = load_invoice(db, org, invoice_id).await?;
    let mut fees = db.fees_for_invoice(invoice.id).await?;
    let req = tax_request(
        invoice.id.to_string(),
        invoice.currency().code(),
        customer_external_id(db, invoice.customer_id).await?,
        &fees,
    );

    let fetched = match provider_code {
        "avalara" => {
            let company = company_code.clone().unwrap_or_else(|| "DEFAULT".into());
            AvalaraTaxProvider::new(base_url, api_key, company, transport)
                .fetch_invoice_taxes(&req)
                .await
        }
        _ => AnrokTaxProvider::new(base_url, api_key, transport).fetch_invoice_taxes(&req).await,
    };
    let results = match fetched {
        Ok(r) => r,
        Err(e) => {
            emit_tax_error(db, org, provider_code, invoice.customer_id, invoice.id, &e).await;
            let error = provider_error_object(&e);
            for fee in &fees {
                emit_fee_tax_error(db, org, provider_code, &invoice, fee, error.clone()).await;
            }
            return Err(integration_err(e));
        }
    };
    let by_id: HashMap<String, _> = results.into_iter().map(|r| (r.item_id.clone(), r)).collect();
    // A taxable fee the provider returned no result for keeps no tax; flag it
    // (`Fees::ApplyProviderTaxesService` reports it as a fee-level error).
    for fee in fees.iter().filter(|f| f.amount_cents > 0 && !by_id.contains_key(&f.id.to_string())) {
        emit_fee_tax_error(
            db,
            org,
            provider_code,
            &invoice,
            fee,
            json!({ "message": "tax provider returned no result for this fee", "error_code": "missing_fee_tax_result" }),
        )
        .await;
    }

    for fee in fees.iter_mut() {
        if let Some(res) = by_id.get(&fee.id.to_string()) {
            let lines: Vec<ProviderTaxLine> = res
                .tax_breakdown
                .iter()
                .map(|b| ProviderTaxLine {
                    name: b.name.clone(),
                    rate: b.rate,
                    tax_amount: b.tax_amount,
                    type_: b.type_.clone(),
                })
                .collect();
            fee.applied_taxes.clear();
            apply_provider_taxes(fee, &lines);
        }
    }

    let taxes_amount_cents: i64 = fees.iter().map(|f| f.taxes_amount_cents).sum();
    invoice.taxes_amount_cents = taxes_amount_cents;
    invoice.sub_total_including_taxes_amount_cents =
        invoice.sub_total_excluding_taxes_amount_cents + taxes_amount_cents;
    invoice.total_amount_cents = invoice.sub_total_including_taxes_amount_cents
        - invoice.credit_notes_amount_cents
        - invoice.prepaid_credit_amount_cents;

    for fee in &fees {
        db.update_fee_taxes(fee.id, fee.taxes_amount_cents, fee.taxes_rate, fee.taxes_precise_amount_cents)
            .await?;
    }
    db.update_invoice_totals(
        invoice.id,
        invoice.taxes_amount_cents,
        invoice.sub_total_including_taxes_amount_cents,
        invoice.total_amount_cents,
    )
    .await?;
    Ok((invoice, fees))
}

/// Void a voided invoice's transaction at the org's tax provider (Anrok or
/// Avalara).
pub async fn void_invoice_taxes<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
) -> Result<(), JobError> {
    let base_url = env_required("RAGO_TAX_BASE_URL")?;
    let provider = active_tax_provider(db, org).await?.unwrap_or_else(|| "anrok".into());
    let (api_key, company_code) = tax_credentials(db, org, &provider).await?;
    let invoice = load_invoice(db, org, invoice_id).await?;
    let result = match provider.as_str() {
        "avalara" => {
            AvalaraTaxProvider::new(base_url, api_key, company_code.unwrap_or_else(|| "DEFAULT".into()), transport)
                .void_invoice(&invoice.id.to_string())
                .await
        }
        _ => AnrokTaxProvider::new(base_url, api_key, transport).void_invoice(&invoice.id.to_string()).await,
    };
    if let Err(e) = &result {
        emit_tax_error(db, org, &provider, invoice.customer_id, invoice.id, e).await;
    }
    result.map_err(integration_err)
}

/// Report a credit note to the org's tax provider as a negative transaction.
pub async fn report_credit_note_taxes<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    credit_note_id: Uuid,
) -> Result<(), JobError> {
    let base_url = env_required("RAGO_TAX_BASE_URL")?;
    let provider = active_tax_provider(db, org).await?.unwrap_or_else(|| "anrok".into());
    let (api_key, company_code) = tax_credentials(db, org, &provider).await?;
    let cn = db
        .find_credit_note(credit_note_id)
        .await?
        .filter(|c| c.organization_id == org)
        .ok_or_else(|| JobError::Integration("credit_note_not_found".into()))?;
    let items = db.credit_note_items(cn.id).await?;
    let req = TaxRequest {
        invoice_id: cn.id.to_string(),
        currency: cn.total_amount_currency.clone(),
        customer_external_id: customer_external_id(db, Some(cn.customer_id)).await?,
        fees: items
            .iter()
            .map(|(fee_id, amount)| TaxFeeItem {
                item_id: fee_id.to_string(),
                item_code: "credit_note_item".into(),
                amount_cents: *amount,
            })
            .collect(),
    };
    let result = match provider.as_str() {
        "avalara" => {
            AvalaraTaxProvider::new(base_url, api_key, company_code.unwrap_or_else(|| "DEFAULT".into()), transport)
                .report_credit_note(&req)
                .await
        }
        _ => AnrokTaxProvider::new(base_url, api_key, transport).report_credit_note(&req).await,
    };
    if let Err(e) = &result {
        emit_tax_error(db, org, &provider, Some(cn.customer_id), cn.id, e).await;
    }
    result.map_err(integration_err)
}

// ---- accounting client -----------------------------------------------------------

/// An accounting provider client bound to one connection.
pub enum AccountingClient<'a, T: HttpTransport> {
    Xero(XeroProvider<&'a T>),
    Netsuite(NetsuiteProvider<&'a T>),
}

impl<'a, T: HttpTransport> AccountingClient<'a, T> {
    async fn sync_invoice(&self, doc: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Xero(p) => p.sync_invoice(doc).await,
            Self::Netsuite(p) => p.sync_invoice(doc).await,
        }
    }
    async fn sync_credit_note(&self, doc: &AccountingInvoice) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Xero(p) => p.sync_credit_note(doc).await,
            Self::Netsuite(p) => p.sync_credit_note(doc).await,
        }
    }
    pub(crate) async fn sync_payment(&self, p: &AccountingPayment) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Xero(x) => x.sync_payment(p).await,
            Self::Netsuite(n) => n.sync_payment(p).await,
        }
    }
    async fn create_contact(&self, c: &AccountingContact) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Xero(p) => p.create_contact(c).await,
            Self::Netsuite(p) => p.create_contact(c).await,
        }
    }
    async fn fetch_catalog(&self, kind: &str) -> Result<Vec<CatalogEntry>, IntegrationError> {
        match self {
            Self::Xero(p) => p.fetch_catalog(kind).await,
            Self::Netsuite(p) => p.fetch_catalog(kind).await,
        }
    }
    /// The catalog kinds this provider exposes to the mapping pickers.
    fn catalog_kinds(&self) -> &'static [&'static str] {
        match self {
            Self::Xero(_) => &["item", "account"],
            Self::Netsuite(_) => &["item", "account", "subsidiary"],
        }
    }
    fn requires_item_mapping(&self) -> bool {
        matches!(self, Self::Netsuite(_))
    }
}

fn opened(conn: &IntegrationConnection, key: &str) -> Result<Option<String>, JobError> {
    conn.sealed_setting(key)
        .transpose()
        .map_err(|e| JobError::Integration(format!("{}_{key}_unavailable: {e}", conn.provider_code)))
}

/// NetSuite TBA credentials from the integration's (sealed) settings, when
/// complete.
pub(crate) fn netsuite_credentials(conn: &IntegrationConnection) -> Result<Option<NetsuiteCredentials>, JobError> {
    let (Some(account_id), Some(client_id), Some(token_id), Some(script_endpoint_url)) = (
        conn.setting("account_id"),
        conn.setting("client_id"),
        conn.setting("token_id"),
        conn.setting("script_endpoint_url"),
    ) else {
        return Ok(None);
    };
    let (Some(client_secret), Some(token_secret)) = (opened(conn, "client_secret")?, opened(conn, "token_secret")?) else {
        return Ok(None);
    };
    Ok(Some(NetsuiteCredentials { account_id, client_id, client_secret, token_id, token_secret, script_endpoint_url }))
}

/// Build the accounting client for `conn`.
pub async fn accounting_client<'a, T: HttpTransport>(
    db: &Db,
    transport: &'a T,
    conn: &IntegrationConnection,
) -> Result<AccountingClient<'a, T>, JobError> {
    let aggregator = env_opt("RAGO_ACCOUNTING_BASE_URL");
    match conn.provider_code.as_str() {
        "xero" => {
            let token = fresh_access_token(db, transport, conn).await?;
            match (aggregator, conn.setting("tenant_id")) {
                (Some(base), _) => Ok(AccountingClient::Xero(XeroProvider::new(base, token, transport))),
                (None, Some(tenant)) => Ok(AccountingClient::Xero(XeroProvider::direct(token, tenant, transport))),
                (None, None) => Err(JobError::Integration("xero_tenant_missing: reconnect Xero from the console".into())),
            }
        }
        "netsuite" => {
            if let Some(creds) = netsuite_credentials(conn)? {
                return Ok(AccountingClient::Netsuite(NetsuiteProvider::restlet(creds, transport)));
            }
            match (aggregator, conn.access_token.clone().filter(|t| !t.is_empty())) {
                (Some(base), Some(token)) => Ok(AccountingClient::Netsuite(NetsuiteProvider::new(base, token, transport))),
                _ => Err(JobError::Integration(
                    "netsuite_credentials_missing: save the account ID, consumer key/secret, token ID/secret and RESTlet URL on the NetSuite integration".into(),
                )),
            }
        }
        other => Err(JobError::Integration(format!("unsupported_accounting_provider: {other}"))),
    }
}

/// Whether an accounting connection holds the credentials a sync needs.
pub(crate) fn accounting_ready(conn: &IntegrationConnection) -> bool {
    match conn.provider_code.as_str() {
        "netsuite" => netsuite_credentials(conn).ok().flatten().is_some() || conn.access_token.is_some(),
        _ => conn.access_token.is_some(),
    }
}

pub(crate) async fn active_accounting(db: &Db, org: Uuid) -> Result<IntegrationConnection, JobError> {
    db.active_integration_of_type(org, "accounting")
        .await?
        .ok_or(JobError::NotConfigured("accounting integration (connect Xero or NetSuite)"))
}

// ---- accounting payloads ----------------------------------------------------------

/// Resolves fee lines against one integration's mappings (memoized).
struct MappingResolver<'a> {
    db: &'a Db,
    integration_id: Uuid,
    objects: HashMap<(String, Uuid), Option<(String, Option<String>)>>,
    collections: HashMap<String, Option<(Option<String>, Option<String>)>>,
    charges: HashMap<Uuid, (Option<Uuid>, String)>,
}

impl<'a> MappingResolver<'a> {
    fn new(db: &'a Db, integration_id: Uuid) -> Self {
        Self { db, integration_id, objects: HashMap::new(), collections: HashMap::new(), charges: HashMap::new() }
    }

    async fn object(&mut self, kind: &str, id: Uuid) -> Result<Option<(String, Option<String>)>, JobError> {
        let key = (kind.to_string(), id);
        if !self.objects.contains_key(&key) {
            let m = self.db.object_mapping(self.integration_id, kind, id).await?.map(|(ext, acct, _)| (ext, acct));
            self.objects.insert(key.clone(), m);
        }
        Ok(self.objects.get(&key).cloned().flatten())
    }

    async fn collection(&mut self, mapping_type: &str) -> Result<Option<(Option<String>, Option<String>)>, JobError> {
        if !self.collections.contains_key(mapping_type) {
            let m = self.db.collection_mapping(self.integration_id, mapping_type).await?.map(|(ext, acct, _)| (ext, acct));
            self.collections.insert(mapping_type.to_string(), m);
        }
        Ok(self.collections.get(mapping_type).cloned().flatten())
    }

    /// `(billable_metric_id, charge_code)` of a charge.
    async fn charge(&mut self, charge_id: Uuid) -> (Option<Uuid>, String) {
        if let Some(c) = self.charges.get(&charge_id) {
            return c.clone();
        }
        let c = match self.db.find_charge(charge_id).await {
            Ok(ch) => (ch.billable_metric_id, ch.code),
            Err(_) => (None, "usage".to_string()),
        };
        self.charges.insert(charge_id, c.clone());
        c
    }

    /// `(description, item_external_id, account_code)` for a fee.
    async fn fee(&mut self, fee: &Fee) -> Result<(String, Option<String>, Option<String>), JobError> {
        let (description, mapped, collection) = match fee.fee_type {
            Some(FeeType::Charge) => match fee.charge_id {
                Some(cid) => {
                    let (metric, code) = self.charge(cid).await;
                    let mapped = match metric {
                        Some(m) => self.object("BillableMetric", m).await?,
                        None => None,
                    };
                    (code, mapped, None)
                }
                None => ("Usage".to_string(), None, None),
            },
            Some(FeeType::AddOn) => {
                let mapped = match fee.invoiceable_id {
                    Some(id) => self.object("AddOn", id).await?,
                    None => None,
                };
                ("Add-on".to_string(), mapped, None)
            }
            Some(FeeType::Subscription) => ("Subscription".to_string(), None, Some("subscription_fee")),
            Some(FeeType::Commitment) => ("Minimum commitment".to_string(), None, Some("minimum_commitment")),
            Some(FeeType::Credit) => ("Prepaid credits".to_string(), None, Some("prepaid_credit")),
            Some(other) => (other.as_str().replace('_', " "), None, None),
            None => ("Fee".to_string(), None, None),
        };
        if let Some((item, account)) = mapped {
            return Ok((description, Some(item), account));
        }
        if let Some(kind) = collection {
            if let Some((item, account)) = self.collection(kind).await? {
                return Ok((description, item, account));
            }
        }
        let fallback = self.collection("fallback_item").await?.unwrap_or((None, None));
        Ok((description, fallback.0, fallback.1))
    }
}

fn line_from_fee(fee: &Fee, description: String, item: Option<String>, account: Option<String>) -> AccountingLine {
    AccountingLine {
        lago_id: fee.id.to_string(),
        description,
        item_external_id: item,
        account_code: account,
        units: fee.units.normalize().to_string(),
        unit_amount_cents: fee.unit_amount_cents,
        amount_cents: fee.amount_cents,
        taxes_amount_cents: fee.taxes_amount_cents,
        taxes_rate: fee.taxes_rate,
    }
}

/// A negative adjustment line (coupons, applied credit notes, prepaid credits).
async fn adjustment_line(
    resolver: &mut MappingResolver<'_>,
    mapping_type: &str,
    description: &str,
    amount_cents: i64,
) -> Result<Option<AccountingLine>, JobError> {
    if amount_cents == 0 {
        return Ok(None);
    }
    let (item, account) = match resolver.collection(mapping_type).await? {
        Some(m) => m,
        None => resolver.collection("fallback_item").await?.unwrap_or((None, None)),
    };
    Ok(Some(AccountingLine {
        lago_id: mapping_type.to_string(),
        description: description.to_string(),
        item_external_id: item,
        account_code: account,
        units: "1".into(),
        unit_amount_cents: -amount_cents.abs(),
        amount_cents: -amount_cents.abs(),
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
    }))
}

fn ensure_mapped(client_requires: bool, lines: &[AccountingLine]) -> Result<(), JobError> {
    if client_requires {
        if let Some(l) = lines.iter().find(|l| l.item_external_id.is_none()) {
            return Err(JobError::Integration(format!(
                "item_mapping_missing: map \"{}\" (or set a fallback item) on the integration before syncing",
                l.description
            )));
        }
    }
    Ok(())
}

/// The provider contact id for `customer` on `conn`, creating the contact at
/// the provider (and linking it) on first sync.
async fn ensure_contact<T: HttpTransport>(
    db: &Db,
    client: &AccountingClient<'_, T>,
    conn: &IntegrationConnection,
    customer: &Customer,
) -> Result<String, JobError> {
    if let Some(existing) = db.integration_customer_external_id(conn.id, customer.id).await? {
        return Ok(existing);
    }
    let contact = AccountingContact {
        customer_id: customer.id.to_string(),
        external_id: customer.external_id.clone(),
        name: customer.name.clone(),
        email: customer.email.clone(),
        currency: customer.currency.clone(),
        country: customer.country.clone(),
        tax_identification_number: customer.tax_identification_number.clone(),
        subsidiary_id: conn.setting("subsidiary_id"),
    };
    match client.create_contact(&contact).await {
        Ok(created) => {
            db.insert_integration_customer(Uuid::new_v4(), conn.organization_id, conn.id, customer.id, Some(&created.external_id), None)
                .await?;
            emit(
                db,
                conn.organization_id,
                WebhookKind::CustomerAccountingProviderCreated,
                json!({
                    "lago_id": customer.id,
                    "external_id": customer.external_id,
                    "name": customer.name,
                    "email": customer.email,
                    "integration_customers": [{
                        "lago_integration_id": conn.id,
                        "type": conn.provider_code,
                        "external_customer_id": created.external_id,
                    }],
                }),
            )
            .await;
            Ok(created.external_id)
        }
        Err(e) => {
            emit(
                db,
                conn.organization_id,
                WebhookKind::CustomerAccountingProviderError,
                json!({
                    "lago_customer_id": customer.id,
                    "external_customer_id": customer.external_id,
                    "accounting_provider": conn.provider_code,
                    "accounting_provider_code": conn.code,
                    "provider_error": provider_error_object(&e),
                }),
            )
            .await;
            Err(integration_err(e))
        }
    }
}

// ---- accounting sync -----------------------------------------------------------

/// Sync a finalized invoice to the org's accounting integration (Xero or
/// NetSuite) and store the mapping. Returns the provider id.
pub async fn sync_invoice_accounting<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
) -> Result<String, JobError> {
    let conn = active_accounting(db, org).await?;
    let client = accounting_client(db, transport, &conn).await?;
    let invoice = load_invoice(db, org, invoice_id).await?;
    let customer = match invoice.customer_id {
        Some(cid) => db.find_customer(cid).await?,
        None => None,
    }
    .ok_or_else(|| JobError::Integration("customer_not_found".into()))?;
    let contact = ensure_contact(db, &client, &conn, &customer).await?;

    let mut resolver = MappingResolver::new(db, conn.id);
    let mut lines = Vec::new();
    for fee in db.fees_for_invoice(invoice.id).await? {
        if fee.amount_cents == 0 && fee.units.is_zero() {
            continue;
        }
        let (description, item, account) = resolver.fee(&fee).await?;
        lines.push(line_from_fee(&fee, description, item, account));
    }
    for (kind, label, amount) in [
        ("coupon", "Coupons", invoice.coupons_amount_cents),
        ("credit_note", "Credit notes applied", invoice.credit_notes_amount_cents),
        ("prepaid_credit", "Prepaid credits applied", invoice.prepaid_credit_amount_cents),
    ] {
        if let Some(line) = adjustment_line(&mut resolver, kind, label, amount).await? {
            lines.push(line);
        }
    }
    ensure_mapped(client.requires_item_mapping(), &lines)?;

    let doc = AccountingInvoice {
        invoice_id: invoice.id.to_string(),
        number: invoice.number.clone(),
        currency: invoice.currency().code(),
        total_amount_cents: invoice.total_amount_cents,
        customer_external_id: customer.external_id.clone(),
        contact_external_id: Some(contact),
        issuing_date: invoice.issuing_date.map(|d| d.to_string()),
        due_date: None,
        taxes_amount_cents: invoice.taxes_amount_cents,
        lines,
    };
    let result = match client.sync_invoice(&doc).await {
        Ok(r) => r,
        Err(e) => {
            emit_provider_error(db, &conn, "sync_invoice", invoice.id, &e).await;
            return Err(integration_err(e));
        }
    };
    db.record_resource_sync(org, conn.id, "invoice", invoice.id, &result.external_id).await?;
    emit(
        db,
        org,
        WebhookKind::InvoiceResynced,
        json!({
            "lago_id": invoice.id,
            "number": invoice.number,
            "external_id": result.external_id,
            "integration": conn.provider_code,
        }),
    )
    .await;
    Ok(result.external_id)
}

/// Sync a credit note to the org's accounting integration and store the
/// mapping. Returns the provider id.
pub async fn sync_credit_note_accounting<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    credit_note_id: Uuid,
) -> Result<String, JobError> {
    let conn = active_accounting(db, org).await?;
    let client = accounting_client(db, transport, &conn).await?;
    let cn = db
        .find_credit_note(credit_note_id)
        .await?
        .filter(|c| c.organization_id == org)
        .ok_or_else(|| JobError::Integration("credit_note_not_found".into()))?;
    let customer = db
        .find_customer(cn.customer_id)
        .await?
        .ok_or_else(|| JobError::Integration("customer_not_found".into()))?;
    let contact = ensure_contact(db, &client, &conn, &customer).await?;

    let fees: HashMap<Uuid, Fee> = db.fees_for_invoice(cn.invoice_id).await?.into_iter().map(|f| (f.id, f)).collect();
    let mut resolver = MappingResolver::new(db, conn.id);
    let mut lines = Vec::new();
    for (fee_id, amount) in db.credit_note_items(cn.id).await? {
        let (description, item, account) = match fees.get(&fee_id) {
            Some(fee) => resolver.fee(fee).await?,
            None => {
                let fb = resolver.collection("credit_note").await?.unwrap_or((None, None));
                ("Credit".to_string(), fb.0, fb.1)
            }
        };
        lines.push(AccountingLine {
            lago_id: fee_id.to_string(),
            description,
            item_external_id: item,
            account_code: account,
            units: "1".into(),
            unit_amount_cents: amount,
            amount_cents: amount,
            taxes_amount_cents: 0,
            taxes_rate: cn.taxes_rate,
        });
    }
    if let Some(line) = adjustment_line(&mut resolver, "coupon", "Coupon adjustment", cn.coupons_adjustment_amount_cents).await? {
        lines.push(line);
    }
    ensure_mapped(client.requires_item_mapping(), &lines)?;

    let doc = AccountingInvoice {
        invoice_id: cn.id.to_string(),
        number: cn.number.clone(),
        currency: cn.total_amount_currency.clone(),
        total_amount_cents: cn.total_amount_cents,
        customer_external_id: customer.external_id.clone(),
        contact_external_id: Some(contact),
        issuing_date: Some(cn.issuing_date.to_string()),
        due_date: None,
        taxes_amount_cents: cn.taxes_amount_cents,
        lines,
    };
    let result = match client.sync_credit_note(&doc).await {
        Ok(r) => r,
        Err(e) => {
            emit_provider_error(db, &conn, "sync_credit_note", cn.id, &e).await;
            return Err(integration_err(e));
        }
    };
    db.record_resource_sync(org, conn.id, "credit_note", cn.id, &result.external_id).await?;
    Ok(result.external_id)
}

/// Fetch the provider catalog (items, accounts, subsidiaries) for one
/// integration and refresh the local cache the mapping pickers read.
/// Returns `(kind, count)` per catalog.
pub async fn refresh_integration_catalog<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    integration_id: Uuid,
) -> Result<Vec<(String, usize)>, JobError> {
    let conn = db
        .find_org_integration(org, integration_id)
        .await?
        .ok_or_else(|| JobError::Integration("integration_not_found".into()))?;
    if conn.integration_type != "accounting" {
        return Err(JobError::Integration(format!(
            "catalog_unsupported: {} integrations have no item catalog",
            conn.provider_code
        )));
    }
    let client = accounting_client(db, transport, &conn).await?;
    let mut out = Vec::new();
    for kind in client.catalog_kinds() {
        let entries = match client.fetch_catalog(kind).await {
            Ok(e) => e,
            Err(e) => {
                emit_provider_error(db, &conn, "fetch_catalog", conn.id, &e).await;
                return Err(integration_err(e));
            }
        };
        let ids: Vec<String> = entries.iter().map(|e| e.external_id.clone()).collect();
        for e in &entries {
            db.upsert_integration_item(
                org,
                conn.id,
                &IntegrationItem {
                    external_id: e.external_id.clone(),
                    item_type: kind.to_string(),
                    external_name: e.name.clone(),
                    external_account_code: e.account_code.clone(),
                },
            )
            .await?;
        }
        db.prune_integration_items(conn.id, kind, &ids).await?;
        out.push((kind.to_string(), entries.len()));
    }
    Ok(out)
}

// ---- CRM -----------------------------------------------------------------------

// Customer / invoice / subscription CRM sync lives in `crate::crm_sync`; these
// re-exports keep the historical paths used by the REST layer.
pub use crate::crm_sync::{sync_customer_crm, sync_customer_crm_with_provider};

// ---- auto-sync sweep -------------------------------------------------------------

/// Enqueue accounting sync jobs for finalized invoices / credit notes /
/// succeeded payments of every org whose active accounting integration has
/// `sync_invoices` / `sync_credit_notes` / `sync_payments` enabled, CRM sync
/// jobs for changed customers / invoices / subscriptions (`sync_customers` /
/// `sync_invoices` / `sync_subscriptions` on the CRM integration), and
/// credit-note tax reporting for orgs with a tax integration. Each resource is enqueued once per integration (a queued
/// or finished job, or a stored mapping, suppresses re-enqueueing). Returns
/// the number of jobs enqueued.
pub async fn enqueue_auto_syncs(db: &Db) -> Result<u32, JobError> {
    let mut enqueued = 0u32;
    for (flag, resource, job_type, id_key) in [
        ("sync_invoices", "invoice", JOB_SYNC_INVOICE, "invoice_id"),
        ("sync_credit_notes", "credit_note", JOB_SYNC_CREDIT_NOTE, "credit_note_id"),
    ] {
        for conn in db.integrations_with_flag("accounting", flag).await? {
            let active = db.active_integration_of_type(conn.organization_id, "accounting").await?;
            if active.map(|a| a.id) != Some(conn.id) {
                continue;
            }
            // Only once the connection can authenticate.
            if !accounting_ready(&conn) {
                continue;
            }
            for id in db.unsynced_resources(&conn, resource, job_type, 50).await? {
                db.enqueue_job(job_type, &json!({ id_key: id, "integration_id": conn.id }), Utc::now().naive_utc(), 5)
                    .await?;
                enqueued += 1;
            }
        }
    }
    enqueued += crate::payment_sync::enqueue_payment_syncs(db).await?;
    enqueued += crate::crm_sync::enqueue_crm_syncs(db).await?;
    // Credit notes are reported to the org's tax provider as negative
    // transactions (the tax counterpart of an issued credit note).
    for conn in db.integrations_of_type("tax").await? {
        let active = db.active_integration_of_type(conn.organization_id, "tax").await?;
        if active.map(|a| a.id) != Some(conn.id) {
            continue;
        }
        for id in db.unreported_tax_credit_notes(&conn, JOB_CREDIT_NOTE_TAX_REPORTING, 50).await? {
            db.enqueue_job(
                JOB_CREDIT_NOTE_TAX_REPORTING,
                &json!({ "credit_note_id": id, "integration_id": conn.id }),
                Utc::now().naive_utc(),
                5,
            )
            .await?;
            enqueued += 1;
        }
    }
    Ok(enqueued)
}

/// One maintenance tick for integrations: refresh expiring OAuth tokens and
/// enqueue the auto-syncs. Errors are logged, never abort the caller's loop.
pub async fn run_integration_maintenance<T: HttpTransport>(db: &Db, transport: &T) -> (u32, u32) {
    let refreshed = crate::integration_auth::refresh_expiring_tokens(db, transport).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "integration token refresh sweep failed");
        0
    });
    let enqueued = enqueue_auto_syncs(db).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "integration auto-sync sweep failed");
        0
    });
    (refreshed, enqueued)
}
