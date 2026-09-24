//! Sweep queries and sync-record helpers behind the payment (accounting) and
//! CRM (customers, invoices, subscriptions) auto-syncs.
//!
//! Jobs enqueued by these sweeps carry `resource_id` + `integration_id` in
//! their payload (indexed by `idx_jobs_type_resource_id`), so "is a job already
//! queued for this record" stays an index probe.

use chrono::NaiveDateTime;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::integration_hub::IntegrationConnection;
use crate::repo::Db;

/// A succeeded payment with what an accounting sync needs:
/// `(payment, invoice number, invoice's provider id)`.
#[derive(Debug, Clone)]
pub struct SyncablePayment {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub invoice_id: Uuid,
    pub customer_id: Option<Uuid>,
    pub invoice_number: String,
    pub provider: String,
    pub provider_payment_id: Option<String>,
    pub status: String,
    pub amount_cents: i64,
    pub amount_currency: String,
    pub created_at: NaiveDateTime,
}

impl Db {
    /// Succeeded payments of `conn`'s org, made since the integration was
    /// connected, whose invoice is already synced to it, with no payment sync
    /// record and no payment sync job for it yet.
    pub async fn unsynced_payments(&self, conn: &IntegrationConnection, job_type: &str, limit: i64) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT p.id FROM payments p \
             WHERE p.organization_id = $1 AND p.status = 'succeeded' AND p.updated_at >= $2 \
               AND EXISTS (SELECT 1 FROM integration_mappings mi WHERE mi.integration_connection_id = $3 \
                           AND mi.resource_type = 'invoice' AND mi.lago_id = p.invoice_id AND mi.deleted_at IS NULL) \
               AND NOT EXISTS (SELECT 1 FROM integration_mappings m WHERE m.integration_connection_id = $3 \
                               AND m.resource_type = 'payment' AND m.lago_id = p.id AND m.deleted_at IS NULL) \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.job_type = $4 AND j.payload->>'resource_id' = p.id::text \
                               AND j.payload->>'integration_id' = $3::text) \
             ORDER BY p.updated_at LIMIT $5",
        )
        .bind(conn.organization_id)
        .bind(conn.created_at)
        .bind(conn.id)
        .bind(job_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// Records of `conn`'s org (`customer`, `invoice` — finalized only — or
    /// `subscription`) changed since the integration was connected whose CRM
    /// copy is missing or older than the record, and that have no sync job
    /// created since their last change (so a failing record is retried once
    /// per change, not on every tick).
    pub async fn stale_crm_resources(
        &self,
        conn: &IntegrationConnection,
        resource: &str,
        job_type: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>> {
        let (table, extra) = match resource {
            "invoice" => ("invoices", "AND r.status = 1"),
            "subscription" => ("subscriptions", ""),
            _ => ("customers", "AND r.deleted_at IS NULL"),
        };
        let sql = format!(
            "SELECT r.id FROM {table} r \
             WHERE r.organization_id = $1 AND r.updated_at >= $2 {extra} \
               AND NOT EXISTS (SELECT 1 FROM integration_mappings m WHERE m.integration_connection_id = $3 \
                               AND m.resource_type = $4 AND m.lago_id = r.id AND m.deleted_at IS NULL \
                               AND COALESCE(m.updated_at, m.created_at) >= r.updated_at) \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.job_type = $5 AND j.payload->>'resource_id' = r.id::text \
                               AND j.payload->>'integration_id' = $3::text AND j.created_at >= r.updated_at) \
             ORDER BY r.updated_at LIMIT $6"
        );
        let rows = sqlx::query(&sql)
            .bind(conn.organization_id)
            .bind(conn.created_at)
            .bind(conn.id)
            .bind(resource)
            .bind(job_type)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// The provider id a record was last synced as on one integration.
    pub async fn synced_external_id(&self, integration_id: Uuid, resource_type: &str, lago_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT external_id FROM integration_mappings \
             WHERE integration_connection_id = $1 AND resource_type = $2 AND lago_id = $3 AND deleted_at IS NULL \
             ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 1",
        )
        .bind(integration_id)
        .bind(resource_type)
        .bind(lago_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("external_id")))
    }

    /// Record (or refresh) the sync record of a Lago resource on one
    /// integration: the existing mapping is updated in place so repeated syncs
    /// of the same record never pile up duplicate rows.
    pub async fn upsert_resource_sync(
        &self,
        org: Uuid,
        integration_id: Uuid,
        resource_type: &str,
        lago_id: Uuid,
        external_id: &str,
    ) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE integration_mappings SET external_id = $4, updated_at = now() \
             WHERE id = (SELECT id FROM integration_mappings WHERE integration_connection_id = $1 \
                         AND resource_type = $2 AND lago_id = $3 AND deleted_at IS NULL \
                         ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 1)",
        )
        .bind(integration_id)
        .bind(resource_type)
        .bind(lago_id)
        .bind(external_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            self.record_resource_sync(org, integration_id, resource_type, lago_id, external_id).await?;
        }
        Ok(())
    }

    /// One payment with its invoice's number and customer.
    pub async fn syncable_payment(&self, id: Uuid) -> Result<Option<SyncablePayment>> {
        let row = sqlx::query(
            "SELECT p.id, p.organization_id, p.invoice_id, i.customer_id, i.number AS invoice_number, p.provider, \
                    p.provider_payment_id, p.status, p.amount_cents, p.amount_currency, p.created_at \
             FROM payments p JOIN invoices i ON i.id = p.invoice_id WHERE p.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SyncablePayment {
            id: r.get("id"),
            organization_id: r.get("organization_id"),
            invoice_id: r.get("invoice_id"),
            customer_id: r.get("customer_id"),
            invoice_number: r.get("invoice_number"),
            provider: r.get("provider"),
            provider_payment_id: r.get("provider_payment_id"),
            status: r.get("status"),
            amount_cents: r.get("amount_cents"),
            amount_currency: r.get("amount_currency"),
            created_at: r.get("created_at"),
        }))
    }

    /// An invoice's payment due date (set at finalization from the
    /// customer's net payment term).
    pub async fn invoice_payment_due_date(&self, id: Uuid) -> Result<Option<chrono::NaiveDate>> {
        let row = sqlx::query("SELECT payment_due_date FROM invoices WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.get::<Option<chrono::NaiveDate>, _>("payment_due_date")))
    }

    /// The organization a subscription belongs to.
    pub async fn subscription_org(&self, id: Uuid) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT organization_id FROM subscriptions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("organization_id")))
    }
}
