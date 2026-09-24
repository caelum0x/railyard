//! CRM sync — port of `Integrations::Aggregator::Contacts::{Create,Update}Service`,
//! `Integrations::Aggregator::{Invoices,Subscriptions}::Hubspot::*Service` and
//! `Integrations::Hubspot::{Invoices,Subscriptions}::DeployObjectService`.
//!
//! * **Customers** sync to the org's CRM (Salesforce or HubSpot) as a
//!   contact/company; a re-sync updates the linked record instead of creating
//!   a duplicate.
//! * **Invoices** (finalized) and **subscriptions** sync to HubSpot as the
//!   `LagoInvoices` / `LagoSubscriptions` custom objects, associated to the
//!   customer's contact/company (the customer is synced first when it has no
//!   CRM record yet). The custom object type id is deployed on first use and
//!   stored on the integration settings.
//!
//! Manual (REST / console) or automatic: the integration's `sync_customers`,
//! `sync_invoices` and `sync_subscriptions` settings drive
//! [`enqueue_crm_syncs`] from the maintenance sweep. Provider access needs the
//! OAuth connection (or `RAGO_CRM_BASE_URL` for the aggregator); a missing
//! connection is an explicit `NotConfigured` error.

use chrono::Utc;
use rago_core::models::{Customer, Invoice, Subscription};
use rago_db::integration_hub::IntegrationConnection;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{
    CrmCustomer, CrmInvoice, CrmProvider, CrmRecordSync, CrmSubscription, HttpTransport, HubspotObject,
    HubspotProvider, IntegrationError, SalesforceProvider, SyncResult,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::integration_auth::fresh_access_token;
use crate::integrations_sync::{emit, emit_provider_error, env_opt, integration_err, provider_error_object};
use crate::JobError;

/// Job type: create/update a customer's CRM record.
pub const JOB_SYNC_CRM_CUSTOMER: &str = "integrations.sync_crm_customer";
/// Job type: create/update a finalized invoice's CRM record.
pub const JOB_SYNC_CRM_INVOICE: &str = "integrations.sync_crm_invoice";
/// Job type: create/update a subscription's CRM record.
pub const JOB_SYNC_CRM_SUBSCRIPTION: &str = "integrations.sync_crm_subscription";

/// Records per resource kind enqueued by one sweep tick.
const SWEEP_BATCH: i64 = 50;
/// Attempts per CRM sync job (same as the accounting syncs).
const SYNC_MAX_ATTEMPTS: i32 = 5;

/// The org's CRM client, built from its active CRM connection.
enum CrmClient<'a, T: HttpTransport> {
    Salesforce(SalesforceProvider<&'a T>),
    Hubspot(HubspotProvider<&'a T>),
}

impl<T: HttpTransport> CrmClient<'_, T> {
    async fn sync_customer(&self, c: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Salesforce(p) => p.sync_customer(c).await,
            Self::Hubspot(p) => p.sync_customer(c).await,
        }
    }

    async fn update_customer(&self, id: &str, c: &CrmCustomer) -> Result<SyncResult, IntegrationError> {
        match self {
            Self::Salesforce(p) => p.update_customer(id, c).await,
            Self::Hubspot(p) => p.update_customer(id, c).await,
        }
    }

    fn hubspot(&self, what: &str, provider: &str) -> Result<&HubspotProvider<&'_ T>, JobError> {
        match self {
            Self::Hubspot(p) => Ok(p),
            Self::Salesforce(_) => Err(JobError::Integration(format!(
                "crm_{what}_sync_unsupported: {provider} mirrors customers only; {what} sync needs HubSpot"
            ))),
        }
    }
}

async fn active_crm(db: &Db, org: Uuid) -> Result<IntegrationConnection, JobError> {
    db.active_integration_of_type(org, "crm")
        .await?
        .ok_or(JobError::NotConfigured("CRM integration (connect Salesforce or HubSpot)"))
}

async fn crm_client<'a, T: HttpTransport>(
    db: &Db,
    transport: &'a T,
    conn: &IntegrationConnection,
) -> Result<CrmClient<'a, T>, JobError> {
    let token = fresh_access_token(db, transport, conn).await?;
    let aggregator = env_opt("RAGO_CRM_BASE_URL");
    match conn.provider_code.as_str() {
        "salesforce" => match (aggregator, conn.setting("instance_url")) {
            (Some(base), _) => Ok(CrmClient::Salesforce(SalesforceProvider::new(base, token, transport))),
            (None, Some(instance)) => Ok(CrmClient::Salesforce(SalesforceProvider::direct(token, instance, transport))),
            (None, None) => Err(JobError::Integration(
                "salesforce_instance_url_missing: reconnect Salesforce from the console".into(),
            )),
        },
        "hubspot" => Ok(CrmClient::Hubspot(match aggregator {
            Some(base) => HubspotProvider::new(base, token, transport),
            None => HubspotProvider::direct(token, conn.setting("default_targeted_object").as_deref(), transport),
        })),
        other => Err(JobError::Integration(format!("unsupported_crm_provider: {other}"))),
    }
}

fn crm_customer(customer: &Customer) -> CrmCustomer {
    CrmCustomer {
        customer_id: customer.id.to_string(),
        external_id: customer.external_id.clone(),
        name: customer.name.clone(),
        email: customer.email.clone(),
    }
}

/// `customer.crm_provider_error` for a failed customer sync.
async fn emit_customer_error(db: &Db, conn: &IntegrationConnection, customer: &Customer, e: &IntegrationError) {
    emit(
        db,
        conn.organization_id,
        WebhookKind::CustomerCrmProviderError,
        json!({
            "lago_customer_id": customer.id,
            "external_customer_id": customer.external_id,
            "crm_provider": conn.provider_code,
            "crm_provider_code": conn.code,
            "provider_error": provider_error_object(e),
        }),
    )
    .await;
}

/// The CRM record id the customer is currently linked to on `conn`.
async fn linked_customer_record(db: &Db, conn: &IntegrationConnection, customer_id: Uuid) -> Result<Option<String>, JobError> {
    if let Some(id) = db.integration_customer_external_id(conn.id, customer_id).await? {
        return Ok(Some(id));
    }
    Ok(db.synced_external_id(conn.id, "customer", customer_id).await?)
}

/// Create or update `customer`'s record through an already-built client and
/// store the link. Returns the CRM record id.
async fn push_customer<T: HttpTransport>(
    db: &Db,
    client: &CrmClient<'_, T>,
    conn: &IntegrationConnection,
    customer: &Customer,
) -> Result<String, JobError> {
    let org = conn.organization_id;
    let doc = crm_customer(customer);
    let existing = linked_customer_record(db, conn, customer.id).await?;
    let result = match &existing {
        Some(id) => client.update_customer(id, &doc).await,
        None => client.sync_customer(&doc).await,
    };
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            emit_customer_error(db, conn, customer, &e).await;
            return Err(integration_err(e));
        }
    };
    db.upsert_resource_sync(org, conn.id, "customer", customer.id, &result.external_id).await?;
    let linked = db.integration_customer_external_id(conn.id, customer.id).await?;
    if linked.as_deref() != Some(result.external_id.as_str()) {
        db.insert_integration_customer(Uuid::new_v4(), org, conn.id, customer.id, Some(&result.external_id), None)
            .await?;
    }
    // A new CRM record (first sync, or the old one was deleted at the provider).
    if existing.as_deref() != Some(result.external_id.as_str()) {
        emit(
            db,
            org,
            WebhookKind::CustomerCrmProviderCreated,
            json!({
                "lago_id": customer.id,
                "external_id": customer.external_id,
                "name": customer.name,
                "email": customer.email,
                "integration_customers": [{
                    "lago_integration_id": conn.id,
                    "type": conn.provider_code,
                    "external_customer_id": result.external_id,
                }],
            }),
        )
        .await;
    }
    Ok(result.external_id)
}

/// Sync a customer to the org's CRM (Salesforce or HubSpot): create the record
/// on first sync, update it afterwards. Returns `(provider_code, crm_id)`.
pub async fn sync_customer_crm_with_provider<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    customer: &Customer,
) -> Result<(String, String), JobError> {
    let conn = active_crm(db, org).await?;
    let client = crm_client(db, transport, &conn).await?;
    let id = push_customer(db, &client, &conn, customer).await?;
    Ok((conn.provider_code, id))
}

/// Sync a customer to the org's CRM and store the mapping. Returns the CRM id.
pub async fn sync_customer_crm<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    customer: &Customer,
) -> Result<String, JobError> {
    Ok(sync_customer_crm_with_provider(db, transport, org, customer).await?.1)
}

/// The customer's CRM record id, syncing the customer first when unlinked.
async fn ensure_customer_record<T: HttpTransport>(
    db: &Db,
    client: &CrmClient<'_, T>,
    conn: &IntegrationConnection,
    customer_id: Option<Uuid>,
) -> Result<Option<String>, JobError> {
    let Some(customer_id) = customer_id else { return Ok(None) };
    if let Some(id) = linked_customer_record(db, conn, customer_id).await? {
        return Ok(Some(id));
    }
    let customer = db
        .find_customer(customer_id)
        .await?
        .ok_or_else(|| JobError::Integration("customer_not_found".into()))?;
    Ok(Some(push_customer(db, client, conn, &customer).await?))
}

/// The HubSpot custom object type id for `object`: the stored setting, or
/// deployed now and stored on the integration.
async fn object_type_id<T: HttpTransport>(
    db: &Db,
    hubspot: &HubspotProvider<&T>,
    conn: &IntegrationConnection,
    object: HubspotObject,
) -> Result<Option<String>, JobError> {
    if let Some(id) = conn.setting(object.setting_key()) {
        return Ok(Some(id));
    }
    let deployed = match hubspot.ensure_object_type(object).await {
        Ok(id) => id,
        Err(e) => {
            emit_provider_error(db, conn, "deploy_object", conn.id, &e).await;
            return Err(integration_err(e));
        }
    };
    if let Some(id) = &deployed {
        let mut settings = match &conn.settings {
            Value::Object(m) => m.clone(),
            _ => serde_json::Map::new(),
        };
        settings.insert(object.setting_key().to_string(), Value::String(id.clone()));
        db.replace_integration_settings(conn.id, &Value::Object(settings)).await?;
    }
    Ok(deployed)
}

/// Store a record sync; an association failure after the record was written
/// is reported (`integration.provider_error`) without failing the sync.
async fn store_record_sync(
    db: &Db,
    conn: &IntegrationConnection,
    resource_type: &str,
    lago_id: Uuid,
    sync: CrmRecordSync,
) -> Result<String, JobError> {
    db.upsert_resource_sync(conn.organization_id, conn.id, resource_type, lago_id, &sync.external_id)
        .await?;
    if let Some(e) = sync.association_error {
        tracing::warn!(integration_id = %conn.id, %lago_id, resource_type, error = %e, "crm association failed");
        emit_provider_error(db, conn, "associate", lago_id, &e).await;
    }
    Ok(sync.external_id)
}

fn crm_invoice(invoice: &Invoice, payment_due_date: Option<chrono::NaiveDate>) -> CrmInvoice {
    CrmInvoice {
        invoice_id: invoice.id.to_string(),
        number: invoice.number.clone(),
        invoice_type: invoice.invoice_type.as_str().to_string(),
        status: invoice.status.as_str().to_string(),
        payment_status: invoice.payment_status.as_str().to_string(),
        currency: invoice.currency.clone().unwrap_or_else(|| "USD".into()),
        issuing_date: invoice.issuing_date.map(|d| d.to_string()),
        payment_due_date: payment_due_date.map(|d| d.to_string()),
        sub_total_excluding_taxes_amount_cents: invoice.sub_total_excluding_taxes_amount_cents,
        taxes_amount_cents: invoice.taxes_amount_cents,
        total_amount_cents: invoice.total_amount_cents,
    }
}

/// Sync a finalized invoice to the org's HubSpot as a `LagoInvoices` record.
/// Returns the CRM record id.
pub async fn sync_invoice_crm<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    invoice_id: Uuid,
) -> Result<String, JobError> {
    let invoice = db
        .find_invoice(invoice_id)
        .await?
        .filter(|i| i.organization_id == org)
        .ok_or_else(|| JobError::Integration("invoice_not_found".into()))?;
    if invoice.status.as_str() != "finalized" {
        return Err(JobError::Integration(format!(
            "invoice_not_finalized: invoice {} is {}",
            invoice.number,
            invoice.status.as_str()
        )));
    }
    let conn = active_crm(db, org).await?;
    let client = crm_client(db, transport, &conn).await?;
    let hubspot = client.hubspot("invoice", &conn.provider_code)?;
    let customer_record = ensure_customer_record(db, &client, &conn, invoice.customer_id).await?;
    let type_id = object_type_id(db, hubspot, &conn, HubspotObject::Invoices).await?;
    let existing = db.synced_external_id(conn.id, "invoice", invoice.id).await?;
    let due = db.invoice_payment_due_date(invoice.id).await?;
    let doc = crm_invoice(&invoice, due);
    let sync = match hubspot
        .sync_invoice(type_id.as_deref(), existing.as_deref(), &doc, customer_record.as_deref())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            emit_provider_error(db, &conn, "sync_invoice", invoice.id, &e).await;
            return Err(integration_err(e));
        }
    };
    store_record_sync(db, &conn, "invoice", invoice.id, sync).await
}

async fn crm_subscription(db: &Db, sub: &Subscription) -> Result<CrmSubscription, JobError> {
    let plan = db.find_plan(sub.plan_id).await?;
    Ok(CrmSubscription {
        subscription_id: sub.id.to_string(),
        external_id: sub.external_id.clone(),
        name: sub.name.clone(),
        plan_code: plan.code,
        status: sub.status.as_str().to_string(),
        billing_time: sub.billing_time.as_str().to_string(),
        subscription_at: sub.subscription_at.map(|d| d.date_naive().to_string()),
        started_at: sub.started_at.map(|d| d.date_naive().to_string()),
        ending_at: sub.ending_at.map(|d| d.date_naive().to_string()),
    })
}

/// Sync a subscription to the org's HubSpot as a `LagoSubscriptions` record.
/// Returns the CRM record id.
pub async fn sync_subscription_crm<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    subscription_id: Uuid,
) -> Result<String, JobError> {
    let sub = db.find_subscription(subscription_id).await?;
    if sub.organization_id != org {
        return Err(JobError::Integration("subscription_not_found".into()));
    }
    let conn = active_crm(db, org).await?;
    let client = crm_client(db, transport, &conn).await?;
    let hubspot = client.hubspot("subscription", &conn.provider_code)?;
    let customer_record = ensure_customer_record(db, &client, &conn, Some(sub.customer_id)).await?;
    let type_id = object_type_id(db, hubspot, &conn, HubspotObject::Subscriptions).await?;
    let existing = db.synced_external_id(conn.id, "subscription", sub.id).await?;
    let doc = crm_subscription(db, &sub).await?;
    let sync = match hubspot
        .sync_subscription(type_id.as_deref(), existing.as_deref(), &doc, customer_record.as_deref())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            emit_provider_error(db, &conn, "sync_subscription", sub.id, &e).await;
            return Err(integration_err(e));
        }
    };
    store_record_sync(db, &conn, "subscription", sub.id, sync).await
}

/// Enqueue CRM syncs for changed customers / finalized invoices /
/// subscriptions of every org whose active CRM integration has the matching
/// `sync_*` setting on. Invoices and subscriptions are HubSpot-only. Returns
/// the number of jobs enqueued.
pub async fn enqueue_crm_syncs(db: &Db) -> Result<u32, JobError> {
    let mut enqueued = 0u32;
    for (flag, resource, job_type, id_key) in [
        ("sync_customers", "customer", JOB_SYNC_CRM_CUSTOMER, "customer_id"),
        ("sync_invoices", "invoice", JOB_SYNC_CRM_INVOICE, "invoice_id"),
        ("sync_subscriptions", "subscription", JOB_SYNC_CRM_SUBSCRIPTION, "subscription_id"),
    ] {
        for conn in db.integrations_with_flag("crm", flag).await? {
            let active = db.active_integration_of_type(conn.organization_id, "crm").await?;
            if active.map(|a| a.id) != Some(conn.id) || conn.access_token.is_none() {
                continue;
            }
            if resource != "customer" && conn.provider_code != "hubspot" {
                continue;
            }
            for id in db.stale_crm_resources(&conn, resource, job_type, SWEEP_BATCH).await? {
                db.enqueue_job(
                    job_type,
                    &json!({ id_key: id, "resource_id": id, "integration_id": conn.id }),
                    Utc::now().naive_utc(),
                    SYNC_MAX_ATTEMPTS,
                )
                .await?;
                enqueued += 1;
            }
        }
    }
    Ok(enqueued)
}
