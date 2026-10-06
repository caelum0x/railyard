//! Plan fixed charges addressed by code (`/plans/:code/fixed_charges`).

use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A plan's fixed charge as the REST API shows it.
#[derive(Debug, Clone)]
pub struct FixedChargeRecord {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub code: String,
    pub add_on_id: Option<Uuid>,
    pub add_on_code: Option<String>,
    pub charge_model: i32,
    pub properties: Value,
    pub units: Decimal,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoice_display_name: Option<String>,
    pub parent_id: Option<Uuid>,
    pub created_at: chrono::NaiveDateTime,
}

/// A new fixed charge.
#[derive(Debug, Clone)]
pub struct NewFixedCharge {
    pub code: String,
    pub add_on_id: Option<Uuid>,
    pub charge_model: i32,
    pub properties: Value,
    pub units: Decimal,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoice_display_name: Option<String>,
}

/// The fields an update may change (`None` keeps the stored value).
#[derive(Debug, Clone, Default)]
pub struct FixedChargeChanges {
    pub charge_model: Option<i32>,
    pub properties: Option<Value>,
    pub units: Option<Decimal>,
    pub pay_in_advance: Option<bool>,
    pub prorated: Option<bool>,
    pub invoice_display_name: Option<String>,
}

const SELECT: &str = "SELECT f.id, f.plan_id, f.code, f.add_on_id, a.code AS add_on_code, f.charge_model, \
     f.properties, f.units, f.pay_in_advance, f.prorated, f.invoice_display_name, f.parent_id, f.created_at \
     FROM fixed_charges f LEFT JOIN add_ons a ON a.id = f.add_on_id";

fn record(r: &sqlx::postgres::PgRow) -> Result<FixedChargeRecord> {
    Ok(FixedChargeRecord {
        id: r.try_get("id")?,
        plan_id: r.try_get("plan_id")?,
        code: r.try_get::<Option<String>, _>("code")?.unwrap_or_default(),
        add_on_id: r.try_get("add_on_id")?,
        add_on_code: r.try_get("add_on_code")?,
        charge_model: r.try_get("charge_model")?,
        properties: r.try_get("properties")?,
        units: r.try_get("units")?,
        pay_in_advance: r.try_get("pay_in_advance")?,
        prorated: r.try_get("prorated")?,
        invoice_display_name: r.try_get("invoice_display_name")?,
        parent_id: r.try_get("parent_id")?,
        created_at: r.try_get("created_at")?,
    })
}

impl Db {
    /// A plan's live fixed charges, newest first.
    pub async fn plan_fixed_charges(&self, plan_id: Uuid) -> Result<Vec<FixedChargeRecord>> {
        let rows = sqlx::query(&format!(
            "{SELECT} WHERE f.plan_id = $1 AND f.deleted_at IS NULL ORDER BY f.created_at DESC, f.id"
        ))
        .bind(plan_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(record).collect()
    }

    /// One live fixed charge of a plan by code.
    pub async fn plan_fixed_charge(&self, plan_id: Uuid, code: &str) -> Result<Option<FixedChargeRecord>> {
        let row = sqlx::query(&format!(
            "{SELECT} WHERE f.plan_id = $1 AND f.code = $2 AND f.deleted_at IS NULL"
        ))
        .bind(plan_id)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(record).transpose()
    }

    /// Create a fixed charge; `None` when the plan already has one with this code.
    pub async fn create_plan_fixed_charge(
        &self,
        org: Uuid,
        plan_id: Uuid,
        new: &NewFixedCharge,
    ) -> Result<Option<FixedChargeRecord>> {
        let id = Uuid::new_v4();
        let done = sqlx::query(
            "INSERT INTO fixed_charges (id, organization_id, plan_id, add_on_id, code, charge_model, properties, units, \
             pay_in_advance, prorated, invoice_display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,now(),now()) \
             ON CONFLICT (plan_id, code) WHERE deleted_at IS NULL DO NOTHING",
        )
        .bind(id)
        .bind(org)
        .bind(plan_id)
        .bind(new.add_on_id)
        .bind(&new.code)
        .bind(new.charge_model)
        .bind(&new.properties)
        .bind(new.units)
        .bind(new.pay_in_advance)
        .bind(new.prorated)
        .bind(new.invoice_display_name.as_deref())
        .execute(&self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Ok(None);
        }
        self.plan_fixed_charge(plan_id, &new.code).await
    }

    /// Update a live fixed charge by code.
    pub async fn update_plan_fixed_charge(
        &self,
        plan_id: Uuid,
        code: &str,
        changes: &FixedChargeChanges,
    ) -> Result<Option<FixedChargeRecord>> {
        sqlx::query(
            "UPDATE fixed_charges SET charge_model = COALESCE($3, charge_model), properties = COALESCE($4, properties), \
             units = COALESCE($5, units), pay_in_advance = COALESCE($6, pay_in_advance), \
             prorated = COALESCE($7, prorated), invoice_display_name = COALESCE($8, invoice_display_name), \
             updated_at = now() WHERE plan_id = $1 AND code = $2 AND deleted_at IS NULL",
        )
        .bind(plan_id)
        .bind(code)
        .bind(changes.charge_model)
        .bind(changes.properties.as_ref())
        .bind(changes.units)
        .bind(changes.pay_in_advance)
        .bind(changes.prorated)
        .bind(changes.invoice_display_name.as_deref())
        .execute(&self.pool)
        .await?;
        self.plan_fixed_charge(plan_id, code).await
    }

    /// Soft-delete a live fixed charge by code; returns it as it was.
    pub async fn delete_plan_fixed_charge(&self, plan_id: Uuid, code: &str) -> Result<Option<FixedChargeRecord>> {
        let Some(fc) = self.plan_fixed_charge(plan_id, code).await? else {
            return Ok(None);
        };
        sqlx::query("UPDATE fixed_charges SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(fc.id)
            .execute(&self.pool)
            .await?;
        Ok(Some(fc))
    }
}
