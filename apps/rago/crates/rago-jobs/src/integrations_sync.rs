//! Integration round-trips shared by the REST handlers and the durable job
//! queue: provider taxes (Anrok / Avalara), accounting sync (Xero invoices +
//! credit notes), CRM sync (Salesforce), tax voiding and credit-note tax
//! reporting.
//!
//! Every call goes to the configured aggregator over the build's transport
//! (`rago_integrations::default_transport()` — real HTTP under `live`). Missing
//! configuration is an explicit [`JobError::NotConfigured`], never a fabricated
//! provider response.
//!
//! Env: `RAGO_TAX_BASE_URL` + `RAGO_TAX_API_KEY` (tax), `RAGO_ACCOUNTING_BASE_URL`
//! (accounting; the connection's OAuth access token authenticates), and
//! `RAGO_CRM_BASE_URL` (CRM).

use std::collections::HashMap;

use rago_billing::fees::{apply_provider_taxes, ProviderTaxLine};
use rago_core::models::{Fee, Invoice};
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{
    AccountingInvoice, AccountingProvider, AnrokTaxProvider, AvalaraTaxProvider, CrmCustomer,
    CrmProvider, HttpTransport, SalesforceProvider, TaxFeeItem, TaxProvider, TaxRequest,
    XeroProvider,
};
use serde_json::json;
use uuid::Uuid;

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
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or(JobError::NotConfigured(name))
}

fn integration_err(e: impl std::fmt::Display) -> JobError {
    JobError::Integration(e.to_string())
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

// ---- provider taxes ----------------------------------------------------------

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
    let api_key = env_required("RAGO_TAX_API_KEY")?;
    let mut invoice = load_invoice(db, org, invoice_id).await?;
    let mut fees = db.fees_for_invoice(invoice.id).await?;
    let req = tax_request(
        invoice.id.to_string(),
        invoice.currency().code(),
        customer_external_id(db, invoice.customer_id).await?,
        &fees,
    );

    let results = match provider_code {
        "avalara" => {
            let company = std::env::var("RAGO_AVALARA_COMPANY_CODE").unwrap_or_else(|_| "DEFAULT".into());
            AvalaraTaxProvider::new(base_url, api_key, company, transport)
                .fetch_invoice_taxes(&req)
                .await
        }
        _ => AnrokTaxProvider::new(base_url, api_key, transport).fetch_invoice_taxes(&req).await,
    }
    .map_err(integration_err)?;
    let by_id: HashMap<String, _> = results.into_iter().map(|r| (r.item_id.clone(), r)).collect();

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

/// Void a voided invoice's transaction at the tax provider (Anrok).
pub async fn void_invoice_taxes<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
) -> Result<(), JobError> {
    let base_url = env_required("RAGO_TAX_BASE_URL")?;
    let api_key = env_required("RAGO_TAX_API_KEY")?;
    let invoice = load_invoice(db, org, invoice_id).await?;
    AnrokTaxProvider::new(base_url, api_key, transport)
        .void_invoice(&invoice.id.to_string())
        .await
        .map_err(integration_err)
}

/// Report a credit note to the tax provider (Anrok) as a negative transaction.
pub async fn report_credit_note_taxes<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    credit_note_id: Uuid,
) -> Result<(), JobError> {
    let base_url = env_required("RAGO_TAX_BASE_URL")?;
    let api_key = env_required("RAGO_TAX_API_KEY")?;
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
    AnrokTaxProvider::new(base_url, api_key, transport)
        .report_credit_note(&req)
        .await
        .map_err(integration_err)
}

// ---- accounting (Xero) ---------------------------------------------------------

async fn accounting_connection(db: &Db, org: Uuid) -> Result<(Uuid, String), JobError> {
    let (id, token) = db
        .find_connection_by_provider(org, "xero")
        .await?
        .ok_or(JobError::NotConfigured("xero integration connection"))?;
    let token = token
        .filter(|t| !t.is_empty())
        .ok_or(JobError::NotConfigured("xero access token"))?;
    Ok((id, token))
}

/// Sync a finalized invoice to Xero and store the mapping. Returns the Xero id.
pub async fn sync_invoice_accounting<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
) -> Result<String, JobError> {
    let base_url = env_required("RAGO_ACCOUNTING_BASE_URL")?;
    let (connection_id, token) = accounting_connection(db, org).await?;
    let invoice = load_invoice(db, org, invoice_id).await?;
    let acc = AccountingInvoice {
        invoice_id: invoice.id.to_string(),
        number: invoice.number.clone(),
        currency: invoice.currency().code(),
        total_amount_cents: invoice.total_amount_cents,
        customer_external_id: customer_external_id(db, invoice.customer_id).await?,
    };
    let result = XeroProvider::new(base_url, token, transport)
        .sync_invoice(&acc)
        .await
        .map_err(integration_err)?;
    db.insert_integration_mapping(Uuid::new_v4(), org, connection_id, "invoice", invoice.id, &result.external_id)
        .await?;
    let _ = record_webhook(
        db,
        org,
        WebhookKind::InvoiceResynced,
        json!({ "lago_id": invoice.id, "number": invoice.number, "external_id": result.external_id }),
    )
    .await;
    Ok(result.external_id)
}

/// Sync a credit note to Xero and store the mapping. Returns the Xero id.
pub async fn sync_credit_note_accounting<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    credit_note_id: Uuid,
) -> Result<String, JobError> {
    let base_url = env_required("RAGO_ACCOUNTING_BASE_URL")?;
    let (connection_id, token) = accounting_connection(db, org).await?;
    let cn = db
        .find_credit_note(credit_note_id)
        .await?
        .filter(|c| c.organization_id == org)
        .ok_or_else(|| JobError::Integration("credit_note_not_found".into()))?;
    let note = AccountingInvoice {
        invoice_id: cn.id.to_string(),
        number: cn.number.clone(),
        currency: cn.total_amount_currency.clone(),
        total_amount_cents: cn.total_amount_cents,
        customer_external_id: customer_external_id(db, Some(cn.customer_id)).await?,
    };
    let result = XeroProvider::new(base_url, token, transport)
        .sync_credit_note(&note)
        .await
        .map_err(integration_err)?;
    db.insert_integration_mapping(Uuid::new_v4(), org, connection_id, "credit_note", cn.id, &result.external_id)
        .await?;
    Ok(result.external_id)
}

// ---- CRM (Salesforce) ------------------------------------------------------------

/// Sync a customer to Salesforce and store the mapping. Returns the CRM id.
pub async fn sync_customer_crm<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    customer: &rago_core::models::Customer,
) -> Result<String, JobError> {
    let base_url = env_required("RAGO_CRM_BASE_URL")?;
    let (connection_id, token) = db
        .find_connection_by_provider(org, "salesforce")
        .await?
        .ok_or(JobError::NotConfigured("salesforce integration connection"))?;
    let token = token
        .filter(|t| !t.is_empty())
        .ok_or(JobError::NotConfigured("salesforce access token"))?;
    let crm = CrmCustomer {
        customer_id: customer.id.to_string(),
        external_id: customer.external_id.clone(),
        name: customer.name.clone(),
        email: customer.email.clone(),
    };
    let result = SalesforceProvider::new(base_url, token, transport)
        .sync_customer(&crm)
        .await
        .map_err(integration_err)?;
    db.insert_integration_mapping(Uuid::new_v4(), org, connection_id, "customer", customer.id, &result.external_id)
        .await?;
    Ok(result.external_id)
}
