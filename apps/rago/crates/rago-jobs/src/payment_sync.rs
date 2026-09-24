//! Payment sync to the accounting integration — port of
//! `Integrations::Aggregator::Payments::CreateService`.
//!
//! A succeeded payment is recorded at Xero (`PUT /Payments`, deposited to the
//! integration's `account` mapping) or NetSuite (a `customerpayment` applied to
//! the invoice) once its invoice has been synced there, so the provider shows
//! the invoice as paid. Manual (`POST /payments/:id/sync_accounting`) or
//! automatic when the integration's `sync_payments` setting is on.

use chrono::Utc;
use rago_db::Db;
use rago_integrations::{AccountingPayment, HttpTransport};
use serde_json::json;
use uuid::Uuid;

use crate::integrations_sync::{
    accounting_client, accounting_ready, active_accounting, emit_provider_error, integration_err,
};
use crate::JobError;

/// Job type: record a succeeded payment at the accounting integration.
pub const JOB_SYNC_PAYMENT: &str = "integrations.sync_payment";

/// Sync one succeeded payment to the org's accounting integration and store
/// the mapping. Returns the provider payment id.
pub async fn sync_payment_accounting<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    payment_id: Uuid,
) -> Result<String, JobError> {
    let payment = db
        .syncable_payment(payment_id)
        .await?
        .filter(|p| p.organization_id == org)
        .ok_or_else(|| JobError::Integration("payment_not_found".into()))?;
    if payment.status != "succeeded" {
        return Err(JobError::Integration(format!("payment_not_succeeded: the payment is {}", payment.status)));
    }
    let conn = active_accounting(db, org).await?;
    if let Some(existing) = db.synced_external_id(conn.id, "payment", payment.id).await? {
        return Ok(existing);
    }
    let invoice_external_id = db
        .synced_external_id(conn.id, "invoice", payment.invoice_id)
        .await?
        .ok_or_else(|| {
            JobError::Integration(format!(
                "invoice_not_synced: sync invoice {} to {} before its payments",
                payment.invoice_number, conn.provider_code
            ))
        })?;
    let contact_external_id = match payment.customer_id {
        Some(cid) => db.integration_customer_external_id(conn.id, cid).await?,
        None => None,
    };
    let account_code = db
        .collection_mapping(conn.id, "account")
        .await?
        .and_then(|(item, account, _)| account.or(item).filter(|v| !v.is_empty()));
    let client = accounting_client(db, transport, &conn).await?;
    let doc = AccountingPayment {
        payment_id: payment.id.to_string(),
        invoice_external_id,
        invoice_number: payment.invoice_number.clone(),
        contact_external_id,
        amount_cents: payment.amount_cents,
        currency: payment.amount_currency.clone(),
        date: payment.created_at.date().to_string(),
        reference: payment.provider_payment_id.clone().filter(|r| !r.is_empty()),
        account_code,
    };
    let result = match client.sync_payment(&doc).await {
        Ok(r) => r,
        Err(e) => {
            emit_provider_error(db, &conn, "sync_payment", payment.id, &e).await;
            return Err(integration_err(e));
        }
    };
    db.upsert_resource_sync(org, conn.id, "payment", payment.id, &result.external_id).await?;
    Ok(result.external_id)
}

/// Enqueue payment syncs for every org whose active accounting integration
/// has `sync_payments` on. Returns the number of jobs enqueued.
pub async fn enqueue_payment_syncs(db: &Db) -> Result<u32, JobError> {
    let mut enqueued = 0u32;
    for conn in db.integrations_with_flag("accounting", "sync_payments").await? {
        let active = db.active_integration_of_type(conn.organization_id, "accounting").await?;
        if active.map(|a| a.id) != Some(conn.id) || !accounting_ready(&conn) {
            continue;
        }
        for id in db.unsynced_payments(&conn, JOB_SYNC_PAYMENT, 50).await? {
            db.enqueue_job(
                JOB_SYNC_PAYMENT,
                &json!({ "payment_id": id, "resource_id": id, "integration_id": conn.id }),
                Utc::now().naive_utc(),
                5,
            )
            .await?;
            enqueued += 1;
        }
    }
    Ok(enqueued)
}
