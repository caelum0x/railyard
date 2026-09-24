//! Repository extensions for the GraphQL dashboard admin CRUD surface — slice 2.
//! Covers credit-note items, usage-monitoring alerts (subscription + wallet),
//! billing entities, invoice custom sections, adjusted fees, fixed charges, plus
//! the batch invoice operations. Kept separate from [`crate::graphql_admin`] to
//! keep each admin file focused; every function is a thin, parameterized `sqlx`
//! query mirroring the Lago service it stands in for.

use rago_core::models::Fee;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

/// A usage-monitoring alert row.
#[derive(Debug, Clone)]
pub struct AlertRow {
    pub id: Uuid,
    pub alertable_type: String,
    pub alertable_id: Uuid,
    pub alert_type: String,
    pub name: Option<String>,
    pub code: String,
    pub billable_metric_id: Option<Uuid>,
}

fn alert_row(r: &sqlx::postgres::PgRow) -> AlertRow {
    AlertRow {
        id: r.get("id"),
        alertable_type: r.get("alertable_type"),
        alertable_id: r.get("alertable_id"),
        alert_type: r.get("alert_type"),
        name: r.get("name"),
        code: r.get("code"),
        billable_metric_id: r.get("billable_metric_id"),
    }
}

/// An alert threshold `(code, value, recurring)`.
pub type AlertThresholdRow = (Option<String>, Decimal, bool);

/// A billing-entity row.
#[derive(Debug, Clone)]
pub struct BillingEntityRow {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub email: Option<String>,
    pub country: Option<String>,
    pub default_currency: String,
    pub timezone: String,
    pub applied_dunning_campaign_id: Option<Uuid>,
}

fn billing_entity_row(r: &sqlx::postgres::PgRow) -> BillingEntityRow {
    BillingEntityRow {
        id: r.get("id"),
        code: r.get("code"),
        name: r.get("name"),
        email: r.get("email"),
        country: r.get("country"),
        default_currency: r.get("default_currency"),
        timezone: r.get("timezone"),
        applied_dunning_campaign_id: r.get("applied_dunning_campaign_id"),
    }
}

/// An invoice-custom-section row.
#[derive(Debug, Clone)]
pub struct InvoiceCustomSectionRow {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub details: Option<String>,
    pub display_name: Option<String>,
}

fn invoice_custom_section_row(r: &sqlx::postgres::PgRow) -> InvoiceCustomSectionRow {
    InvoiceCustomSectionRow {
        id: r.get("id"),
        code: r.get("code"),
        name: r.get("name"),
        description: r.get("description"),
        details: r.get("details"),
        display_name: r.get("display_name"),
    }
}

/// A fixed-charge row.
#[derive(Debug, Clone)]
pub struct FixedChargeRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub add_on_id: Option<Uuid>,
    pub charge_model: i32,
    pub units: Decimal,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoice_display_name: Option<String>,
}

fn fixed_charge_row(r: &sqlx::postgres::PgRow) -> FixedChargeRow {
    FixedChargeRow {
        id: r.get("id"),
        plan_id: r.get("plan_id"),
        add_on_id: r.get("add_on_id"),
        charge_model: r.get("charge_model"),
        units: r.get("units"),
        pay_in_advance: r.get("pay_in_advance"),
        prorated: r.get("prorated"),
        invoice_display_name: r.get("invoice_display_name"),
    }
}

/// A payment row `(id, provider, status, amount_cents, amount_currency)`.
pub type PaymentRow = (Uuid, String, String, i64, String);

/// A feature row `(id, code, name, description)`.
pub type FeatureRow = (Uuid, String, Option<String>, Option<String>);

impl Db {
    // ---- credit-note items ----------------------------------------------

    /// Persist a credit-note item (`CreditNoteItem`).
    pub async fn insert_credit_note_item(
        &self,
        id: Uuid,
        org: Uuid,
        credit_note_id: Uuid,
        fee_id: Uuid,
        amount_cents: i64,
        amount_currency: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO credit_note_items (id, organization_id, credit_note_id, fee_id, amount_cents, amount_currency, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(credit_note_id)
        .bind(fee_id)
        .bind(amount_cents)
        .bind(amount_currency)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Credit-note items `(fee_id, amount_cents)` for a credit note.
    pub async fn credit_note_items(&self, credit_note_id: Uuid) -> Result<Vec<(Uuid, i64)>> {
        let rows = sqlx::query("SELECT fee_id, amount_cents FROM credit_note_items WHERE credit_note_id = $1 ORDER BY created_at")
            .bind(credit_note_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get::<Uuid, _>("fee_id"), r.get::<i64, _>("amount_cents"))).collect())
    }

    // ---- fees (adjusted-fee support) ------------------------------------

    pub async fn find_fee_opt(&self, id: Uuid) -> Result<Option<Fee>> {
        let row = sqlx::query("SELECT * FROM fees WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::fee).transpose()
    }

    /// Override a fee's units + amount (adjusted-fee apply).
    pub async fn update_fee_amount(
        &self,
        id: Uuid,
        units: Decimal,
        amount_cents: i64,
        unit_amount_cents: i64,
    ) -> Result<()> {
        sqlx::query("UPDATE fees SET units = $2, amount_cents = $3, unit_amount_cents = $4, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(units)
            .bind(amount_cents)
            .bind(unit_amount_cents)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Persist an adjusted-fee record.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_adjusted_fee(
        &self,
        id: Uuid,
        org: Uuid,
        fee_id: Option<Uuid>,
        invoice_id: Uuid,
        adjusted_units: bool,
        adjusted_amount: bool,
        units: Decimal,
        unit_amount_cents: i64,
        invoice_display_name: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO adjusted_fees (id, organization_id, fee_id, invoice_id, adjusted_units, adjusted_amount, units, unit_amount_cents, invoice_display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(fee_id)
        .bind(invoice_id)
        .bind(adjusted_units)
        .bind(adjusted_amount)
        .bind(units)
        .bind(unit_amount_cents)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The `fee_id` an adjusted-fee record targets, if any.
    pub async fn adjusted_fee_target(&self, id: Uuid) -> Result<Option<Option<Uuid>>> {
        let row = sqlx::query("SELECT fee_id FROM adjusted_fees WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<Option<Uuid>, _>("fee_id")))
    }

    pub async fn delete_adjusted_fee(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM adjusted_fees WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- batch invoice operations ---------------------------------------

    /// `Invoices::Payments::RetryBatchService` — reset failed payments on
    /// finalized invoices back to pending. Returns the affected count.
    pub async fn reset_failed_invoice_payments(&self, org: Uuid) -> Result<u64> {
        let r = sqlx::query("UPDATE invoices SET payment_status = 0, updated_at = now() WHERE organization_id = $1 AND status = 1 AND payment_status = 2")
            .bind(org)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// `Invoices::RetryBatchService` — re-finalize failed-generation invoices.
    /// Each goes through the guarded finalize transition, so one left
    /// unnumbered is numbered from the organization sequence as it finalizes.
    pub async fn refinalize_failed_invoices(&self, org: Uuid) -> Result<u64> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM invoices WHERE organization_id = $1 AND status = 4 ORDER BY created_at",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        let mut finalized = 0u64;
        for id in ids {
            let mut tx = self.pool.begin().await?;
            let done = crate::numbering::finalize_exec(&mut tx, id).await?;
            tx.commit().await?;
            if done.is_some() {
                finalized += 1;
            }
        }
        Ok(finalized)
    }

    // ---- alerts (usage monitoring) --------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_alert(
        &self,
        id: Uuid,
        org: Uuid,
        alertable_type: &str,
        alertable_id: Uuid,
        alert_type: &str,
        name: Option<&str>,
        code: &str,
        billable_metric_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO alerts (id, organization_id, alertable_type, alertable_id, alert_type, name, code, billable_metric_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(alertable_type)
        .bind(alertable_id)
        .bind(alert_type)
        .bind(name)
        .bind(code)
        .bind(billable_metric_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_alert_threshold(
        &self,
        id: Uuid,
        org: Uuid,
        alert_id: Uuid,
        code: Option<&str>,
        value: Decimal,
        recurring: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO alert_thresholds (id, organization_id, alert_id, code, value, recurring, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(alert_id)
        .bind(code)
        .bind(value)
        .bind(recurring)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_alert(&self, id: Uuid) -> Result<Option<AlertRow>> {
        let row = sqlx::query("SELECT * FROM alerts WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(alert_row))
    }

    pub async fn alert_thresholds(&self, alert_id: Uuid) -> Result<Vec<AlertThresholdRow>> {
        let rows = sqlx::query("SELECT code, value, recurring FROM alert_thresholds WHERE alert_id = $1 ORDER BY value")
            .bind(alert_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<Option<String>, _>("code"), r.get::<Decimal, _>("value"), r.get::<bool, _>("recurring")))
            .collect())
    }

    pub async fn alerts_for(&self, alertable_type: &str, alertable_id: Uuid) -> Result<Vec<AlertRow>> {
        let rows = sqlx::query("SELECT * FROM alerts WHERE alertable_type = $1 AND alertable_id = $2 AND deleted_at IS NULL ORDER BY created_at")
            .bind(alertable_type)
            .bind(alertable_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(alert_row).collect())
    }

    pub async fn update_alert(&self, id: Uuid, name: Option<&str>, code: Option<&str>) -> Result<Option<AlertRow>> {
        sqlx::query("UPDATE alerts SET name = COALESCE($2, name), code = COALESCE($3, code), updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(name)
            .bind(code)
            .execute(&self.pool)
            .await?;
        self.find_alert(id).await
    }

    /// Replace an alert's thresholds atomically.
    pub async fn replace_alert_thresholds(
        &self,
        org: Uuid,
        alert_id: Uuid,
        thresholds: &[(Option<String>, Decimal, bool)],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM alert_thresholds WHERE alert_id = $1").bind(alert_id).execute(&mut *tx).await?;
        for (code, value, recurring) in thresholds {
            sqlx::query(
                "INSERT INTO alert_thresholds (id, organization_id, alert_id, code, value, recurring, created_at, updated_at) \
                 VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,now(),now())",
            )
            .bind(org)
            .bind(alert_id)
            .bind(code.as_deref())
            .bind(value)
            .bind(recurring)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn soft_delete_alert(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE alerts SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- billing entities -----------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_billing_entity(
        &self,
        id: Uuid,
        org: Uuid,
        code: &str,
        name: &str,
        email: Option<&str>,
        country: Option<&str>,
        default_currency: &str,
        timezone: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO billing_entities (id, organization_id, code, name, email, country, default_currency, timezone, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(code)
        .bind(name)
        .bind(email)
        .bind(country)
        .bind(default_currency)
        .bind(timezone)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_billing_entity(
        &self,
        id: Uuid,
        name: Option<&str>,
        email: Option<&str>,
        country: Option<&str>,
        default_currency: Option<&str>,
        timezone: Option<&str>,
    ) -> Result<Option<BillingEntityRow>> {
        sqlx::query(
            "UPDATE billing_entities SET \
             name = COALESCE($2, name), email = COALESCE($3, email), country = COALESCE($4, country), \
             default_currency = COALESCE($5, default_currency), timezone = COALESCE($6, timezone), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(email)
        .bind(country)
        .bind(default_currency)
        .bind(timezone)
        .execute(&self.pool)
        .await?;
        self.find_billing_entity(id).await
    }

    pub async fn update_billing_entity_dunning(&self, id: Uuid, campaign_id: Option<Uuid>) -> Result<Option<BillingEntityRow>> {
        sqlx::query("UPDATE billing_entities SET applied_dunning_campaign_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(campaign_id)
            .execute(&self.pool)
            .await?;
        self.find_billing_entity(id).await
    }

    pub async fn soft_delete_billing_entity(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE billing_entities SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_billing_entity(&self, id: Uuid) -> Result<Option<BillingEntityRow>> {
        let row = sqlx::query("SELECT * FROM billing_entities WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(billing_entity_row))
    }

    pub async fn list_billing_entities(&self, org: Uuid) -> Result<Vec<BillingEntityRow>> {
        let rows = sqlx::query("SELECT * FROM billing_entities WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(billing_entity_row).collect())
    }

    // ---- invoice custom sections ----------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_invoice_custom_section(
        &self,
        id: Uuid,
        org: Uuid,
        code: &str,
        name: &str,
        description: Option<&str>,
        details: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO invoice_custom_sections (id, organization_id, code, name, description, details, display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(code)
        .bind(name)
        .bind(description)
        .bind(details)
        .bind(display_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_invoice_custom_section(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        details: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<Option<InvoiceCustomSectionRow>> {
        sqlx::query(
            "UPDATE invoice_custom_sections SET \
             name = COALESCE($2, name), description = COALESCE($3, description), \
             details = COALESCE($4, details), display_name = COALESCE($5, display_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(details)
        .bind(display_name)
        .execute(&self.pool)
        .await?;
        self.find_invoice_custom_section(id).await
    }

    pub async fn soft_delete_invoice_custom_section(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE invoice_custom_sections SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_invoice_custom_section(&self, id: Uuid) -> Result<Option<InvoiceCustomSectionRow>> {
        let row = sqlx::query("SELECT * FROM invoice_custom_sections WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(invoice_custom_section_row))
    }

    pub async fn list_invoice_custom_sections(&self, org: Uuid) -> Result<Vec<InvoiceCustomSectionRow>> {
        let rows = sqlx::query("SELECT * FROM invoice_custom_sections WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(invoice_custom_section_row).collect())
    }

    // ---- fixed charges ---------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_fixed_charge(
        &self,
        id: Uuid,
        org: Uuid,
        plan_id: Uuid,
        add_on_id: Option<Uuid>,
        charge_model: i32,
        properties: &Value,
        units: Decimal,
        pay_in_advance: bool,
        prorated: bool,
        invoice_display_name: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO fixed_charges (id, organization_id, plan_id, add_on_id, charge_model, properties, units, pay_in_advance, prorated, invoice_display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(plan_id)
        .bind(add_on_id)
        .bind(charge_model)
        .bind(properties)
        .bind(units)
        .bind(pay_in_advance)
        .bind(prorated)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_fixed_charge(
        &self,
        id: Uuid,
        charge_model: Option<i32>,
        properties: Option<&Value>,
        units: Option<Decimal>,
        pay_in_advance: Option<bool>,
        prorated: Option<bool>,
        invoice_display_name: Option<&str>,
    ) -> Result<Option<FixedChargeRow>> {
        sqlx::query(
            "UPDATE fixed_charges SET \
             charge_model = COALESCE($2, charge_model), properties = COALESCE($3, properties), \
             units = COALESCE($4, units), pay_in_advance = COALESCE($5, pay_in_advance), \
             prorated = COALESCE($6, prorated), invoice_display_name = COALESCE($7, invoice_display_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(charge_model)
        .bind(properties)
        .bind(units)
        .bind(pay_in_advance)
        .bind(prorated)
        .bind(invoice_display_name)
        .execute(&self.pool)
        .await?;
        self.find_fixed_charge(id).await
    }

    pub async fn soft_delete_fixed_charge(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE fixed_charges SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_fixed_charge(&self, id: Uuid) -> Result<Option<FixedChargeRow>> {
        let row = sqlx::query("SELECT * FROM fixed_charges WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(fixed_charge_row))
    }

    pub async fn fixed_charges_for_plan(&self, plan_id: Uuid) -> Result<Vec<FixedChargeRow>> {
        let rows = sqlx::query("SELECT * FROM fixed_charges WHERE plan_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(plan_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(fixed_charge_row).collect())
    }

    // ---- subscription entitlements --------------------------------------

    /// Soft-delete an entitlement row (subscription or plan).
    pub async fn soft_delete_entitlement(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE entitlement_entitlements SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// The entitlement row id for a `(subscription, feature)` pair, if present.
    pub async fn subscription_entitlement_id(&self, subscription_id: Uuid, feature_id: Uuid) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM entitlement_entitlements WHERE subscription_id = $1 AND entitlement_feature_id = $2 AND deleted_at IS NULL")
            .bind(subscription_id)
            .bind(feature_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<Uuid, _>("id")))
    }

    // ---- features + payments (read queries) -----------------------------

    pub async fn list_features(&self, org: Uuid) -> Result<Vec<FeatureRow>> {
        let rows = sqlx::query("SELECT id, code, name, description FROM entitlement_features WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("code"), r.get::<Option<String>, _>("name"), r.get::<Option<String>, _>("description")))
            .collect())
    }

    pub async fn find_feature(&self, org: Uuid, code: &str) -> Result<Option<FeatureRow>> {
        let row = sqlx::query("SELECT id, code, name, description FROM entitlement_features WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL")
            .bind(org)
            .bind(code)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("code"), r.get::<Option<String>, _>("name"), r.get::<Option<String>, _>("description"))))
    }

    /// `Entitlement::Features::UpdateService` — update a feature's name /
    /// description, returning the refreshed row (`None` if soft-deleted/gone).
    pub async fn update_feature(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<Option<FeatureRow>> {
        sqlx::query(
            "UPDATE entitlement_features SET \
             name = COALESCE($2, name), \
             description = COALESCE($3, description), \
             updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .execute(&self.pool)
        .await?;
        let row = sqlx::query("SELECT id, code, name, description FROM entitlement_features WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("code"), r.get::<Option<String>, _>("name"), r.get::<Option<String>, _>("description"))))
    }

    /// `Entitlement::Features::DestroyService` — soft-delete a feature and its
    /// privileges so it no longer resolves.
    pub async fn soft_delete_feature(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE entitlement_features SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE entitlement_privileges SET deleted_at = now(), updated_at = now() WHERE entitlement_feature_id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_payments(&self, invoice_id: Uuid) -> Result<Vec<PaymentRow>> {
        let rows = sqlx::query("SELECT id, provider, status, amount_cents, amount_currency FROM payments WHERE invoice_id = $1 ORDER BY created_at DESC")
            .bind(invoice_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("provider"),
                    r.get::<String, _>("status"),
                    r.get::<i64, _>("amount_cents"),
                    r.get::<String, _>("amount_currency"),
                )
            })
            .collect())
    }

    /// Payments across all of a customer's invoices (the
    /// `/customers/:id/payments` nested index), joined via `invoices`.
    pub async fn payments_for_customer(&self, customer_id: Uuid) -> Result<Vec<PaymentRow>> {
        let rows = sqlx::query(
            "SELECT p.id, p.provider, p.status, p.amount_cents, p.amount_currency \
             FROM payments p JOIN invoices i ON p.invoice_id = i.id \
             WHERE i.customer_id = $1 ORDER BY p.created_at DESC",
        )
        .bind(customer_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("provider"),
                    r.get::<String, _>("status"),
                    r.get::<i64, _>("amount_cents"),
                    r.get::<String, _>("amount_currency"),
                )
            })
            .collect())
    }

    // ---- one-off invoice fee (CreateInvoice) ----------------------------

    /// Insert a minimal one-off fee row (add-on style line on a one-off
    /// invoice). Returns the fee id.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_one_off_fee(
        &self,
        org: Uuid,
        invoice_id: Uuid,
        amount_cents: i64,
        amount_currency: &str,
        units: Decimal,
        unit_amount_cents: i64,
        add_on_id: Option<Uuid>,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO fees (id, organization_id, billing_entity_id, invoice_id, fee_type, invoiceable_type, invoiceable_id, \
             amount_cents, amount_currency, unit_amount_cents, taxes_amount_cents, units, payment_status, created_at, updated_at) \
             VALUES ($1,$2,$2,$3,1,'AddOn',$4,$5,$6,$7,0,$8,0,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(invoice_id)
        .bind(add_on_id)
        .bind(amount_cents)
        .bind(amount_currency)
        .bind(unit_amount_cents)
        .bind(units)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }
}
