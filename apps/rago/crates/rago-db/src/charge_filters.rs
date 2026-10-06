//! Charge filters with their matched values — the REST (`charge.filters`) and
//! console side of `ChargeFilters::CreateOrUpdateBatchService`.
//!
//! A filter prices the events whose properties take one of the listed values
//! per key with its own `properties` (and optional invoice display name).

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// One charge filter as submitted / listed.
#[derive(Debug, Clone, PartialEq)]
pub struct ChargeFilterSpec {
    pub invoice_display_name: Option<String>,
    /// Charge-model properties applied to the filtered events.
    pub properties: Value,
    /// `{ "<event property>": ["value", ...] }`.
    pub values: Value,
}

/// A stored filter.
#[derive(Debug, Clone)]
pub struct ChargeFilterRow {
    pub id: Uuid,
    pub spec: ChargeFilterSpec,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Db {
    /// Replace a charge's live filters with `filters` (the batch semantics of
    /// Lago's charge update: filters absent from the payload are removed).
    pub async fn replace_charge_filters(&self, org: Uuid, charge_id: Uuid, filters: &[ChargeFilterSpec]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE charge_filters SET deleted_at = now(), updated_at = now() \
             WHERE charge_id = $1 AND organization_id = $2 AND deleted_at IS NULL",
        )
        .bind(charge_id)
        .bind(org)
        .execute(&mut *tx)
        .await?;
        for f in filters {
            sqlx::query(
                "INSERT INTO charge_filters (id, organization_id, charge_id, invoice_display_name, properties, \"values\", created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
            )
            .bind(Uuid::new_v4())
            .bind(org)
            .bind(charge_id)
            .bind(f.invoice_display_name.as_deref())
            .bind(&f.properties)
            .bind(&f.values)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// A charge's live filters, oldest first.
    pub async fn charge_filter_rows(&self, charge_id: Uuid) -> Result<Vec<ChargeFilterRow>> {
        let rows = sqlx::query(
            "SELECT id, invoice_display_name, properties, \"values\", created_at FROM charge_filters \
             WHERE charge_id = $1 AND deleted_at IS NULL ORDER BY created_at, id",
        )
        .bind(charge_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ChargeFilterRow {
                    id: r.try_get("id")?,
                    spec: ChargeFilterSpec {
                        invoice_display_name: r.try_get("invoice_display_name")?,
                        properties: r.try_get("properties")?,
                        values: r.try_get("values")?,
                    },
                    // `charge_filters.created_at` is a timestamp without zone (UTC).
                    created_at: r.try_get::<chrono::NaiveDateTime, _>("created_at")?.and_utc(),
                })
            })
            .collect()
    }

    /// A charge's live filters as billing reads them (oldest first).
    pub async fn charge_filter_specs(&self, charge_id: Uuid) -> Result<Vec<rago_billing::charge_fees::FilterSpec>> {
        Ok(self
            .charge_filter_rows(charge_id)
            .await?
            .into_iter()
            .map(|r| rago_billing::charge_fees::FilterSpec::from_parts(r.id, r.spec.properties, &r.spec.values, r.created_at))
            .collect())
    }
}
