//! Repository support for the Lago REST routes added for SDK parity
//! (integrations list, fees, events by transaction id, wallet recurring rules,
//! quotes, dunning campaign by code, invoice metadata/payment status).
//! Every query is organization-scoped.

use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// `(id, integration_type, provider_code, name, has_access_token,
/// has_refresh_token, token_expires_at)`.
pub type IntegrationListRow = (
    Uuid,
    String,
    String,
    Option<String>,
    bool,
    bool,
    Option<NaiveDateTime>,
);

impl Db {
    /// An org's integration connections (token presence only).
    pub async fn integration_connections_overview(&self, org: Uuid) -> Result<Vec<IntegrationListRow>> {
        let rows = sqlx::query(
            "SELECT id, integration_type, provider_code, name, \
                    (access_token IS NOT NULL) AS has_access, (refresh_token IS NOT NULL) AS has_refresh, \
                    token_expires_at \
             FROM integration_connections WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    r.get("integration_type"),
                    r.get("provider_code"),
                    r.get("name"),
                    r.get("has_access"),
                    r.get("has_refresh"),
                    r.get("token_expires_at"),
                )
            })
            .collect())
    }

    /// Update an invoice's `metadata` (merged) and/or `payment_status`,
    /// org-scoped. Returns whether a row was updated.
    pub async fn update_invoice_fields(
        &self,
        org: Uuid,
        invoice_id: Uuid,
        payment_status: Option<i32>,
        metadata: Option<&Value>,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE invoices SET payment_status = COALESCE($3, payment_status), updated_at = now() \
             WHERE id = $1 AND organization_id = $2",
        )
        .bind(invoice_id)
        .bind(org)
        .bind(payment_status)
        .execute(&self.pool)
        .await?;
        if let Some(meta) = metadata {
            sqlx::query(
                "UPDATE invoices SET metadata = COALESCE(metadata, '{}'::jsonb) || $3::jsonb WHERE id = $1 AND organization_id = $2",
            )
            .bind(invoice_id)
            .bind(org)
            .bind(meta)
            .execute(&self.pool)
            .await?;
        }
        Ok(r.rows_affected() > 0)
    }
}

impl Db {
    /// An invoice's metadata object (org-scoped).
    pub async fn invoice_metadata(&self, org: Uuid, invoice_id: Uuid) -> Result<Value> {
        let row = sqlx::query("SELECT metadata FROM invoices WHERE id = $1 AND organization_id = $2")
            .bind(invoice_id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.get::<Value, _>("metadata"))
            .unwrap_or_else(|| Value::Object(Default::default())))
    }
}

impl Db {
    /// An event by its client `transaction_id` (org-scoped; most recent).
    pub async fn find_event_by_transaction_id(
        &self,
        org: Uuid,
        transaction_id: &str,
    ) -> Result<Option<rago_core::models::Event>> {
        let row = sqlx::query(
            "SELECT * FROM events WHERE organization_id = $1 AND transaction_id = $2 \
             AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1",
        )
        .bind(org)
        .bind(transaction_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(crate::mapping::event).transpose()
    }

    /// A page of the org's fees, optionally filtered by `fee_type` /
    /// `payment_status` / `external_subscription_id`, newest first.
    pub async fn list_fees_for_org(
        &self,
        org: Uuid,
        fee_type: Option<i32>,
        payment_status: Option<i32>,
        external_subscription_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<rago_core::models::Fee>, i64)> {
        let filter = "f.organization_id = $1 AND ($2::int IS NULL OR f.fee_type = $2) \
                      AND ($3::int IS NULL OR f.payment_status = $3) \
                      AND ($4::varchar IS NULL OR s.external_id = $4)";
        let rows = sqlx::query(&format!(
            "SELECT f.* FROM fees f LEFT JOIN subscriptions s ON s.id = f.subscription_id \
             WHERE {filter} ORDER BY f.created_at DESC LIMIT $5 OFFSET $6"
        ))
        .bind(org)
        .bind(fee_type)
        .bind(payment_status)
        .bind(external_subscription_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM fees f LEFT JOIN subscriptions s ON s.id = f.subscription_id WHERE {filter}"
        ))
        .bind(org)
        .bind(fee_type)
        .bind(payment_status)
        .bind(external_subscription_id)
        .fetch_one(&self.pool)
        .await?;
        let fees = rows.iter().map(crate::mapping::fee).collect::<Result<Vec<_>>>()?;
        Ok((fees, total))
    }

    /// Set a fee's `payment_status` (org-scoped). Returns whether it existed.
    pub async fn update_fee_payment_status(&self, org: Uuid, id: Uuid, payment_status: i32) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE fees SET payment_status = $3, updated_at = now() WHERE id = $1 AND organization_id = $2",
        )
        .bind(id)
        .bind(org)
        .bind(payment_status)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Delete a fee that is not attached to a finalized invoice (Lago only
    /// allows destroying pay-in-advance fees not yet invoiced / draft fees).
    /// Returns `false` when the fee is invoiced (or does not exist).
    pub async fn delete_uninvoiced_fee(&self, org: Uuid, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "DELETE FROM fees WHERE id = $1 AND organization_id = $2 \
               AND (invoice_id IS NULL OR invoice_id IN (SELECT id FROM invoices WHERE status = 0))",
        )
        .bind(id)
        .bind(org)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// A wallet's recurring top-up rules
    /// `(id, rule_type, interval, threshold_credits, paid_credits, granted_credits)`.
    #[allow(clippy::type_complexity)]
    pub async fn wallet_recurring_rules(
        &self,
        org: Uuid,
        wallet_id: Uuid,
    ) -> Result<Vec<(Uuid, String, Option<String>, Option<rust_decimal::Decimal>, rust_decimal::Decimal, rust_decimal::Decimal)>> {
        let rows = sqlx::query(
            "SELECT id, rule_type, interval, threshold_credits, paid_credits, granted_credits \
             FROM wallet_recurring_rules WHERE organization_id = $1 AND wallet_id = $2 ORDER BY created_at",
        )
        .bind(org)
        .bind(wallet_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    r.get("rule_type"),
                    r.get("interval"),
                    r.get("threshold_credits"),
                    r.get("paid_credits"),
                    r.get("granted_credits"),
                )
            })
            .collect())
    }

    /// A dunning campaign's id by code (org-scoped).
    pub async fn dunning_campaign_id_by_code(&self, org: Uuid, code: &str) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM dunning_campaigns WHERE organization_id = $1 AND code = $2")
            .bind(org)
            .bind(code)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("id")))
    }

    /// A page of all the org's wallets (any customer), newest first, with the
    /// customer's external id.
    pub async fn list_org_wallets(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<(rago_core::models::Wallet, String)>, i64)> {
        let rows = sqlx::query(
            "SELECT w.*, c.external_id AS customer_external_id FROM wallets w \
             JOIN customers c ON c.id = w.customer_id \
             WHERE w.organization_id = $1 ORDER BY w.created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wallets WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push((crate::mapping::wallet(r)?, r.get::<String, _>("customer_external_id")));
        }
        Ok((out, total))
    }
}

impl Db {
    /// The org's most recent payment requests (any customer).
    pub async fn list_org_payment_requests(&self, org: Uuid, limit: i64) -> Result<Vec<crate::PaymentRequestRow>> {
        let rows = sqlx::query(
            "SELECT id, external_customer_id, email, amount_cents, amount_currency, payment_status \
             FROM payment_requests WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(org)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    r.get("external_customer_id"),
                    r.get("email"),
                    r.get("amount_cents"),
                    r.get("amount_currency"),
                    r.get("payment_status"),
                )
            })
            .collect())
    }
}

impl Db {
    /// A data export's filters object.
    pub async fn data_export_filters(&self, id: Uuid) -> Result<Value> {
        let row = sqlx::query("SELECT filters FROM data_exports WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.get::<Value, _>("filters"))
            .unwrap_or_else(|| Value::Object(Default::default())))
    }

    /// A finance-assistant session's Q&A `(question, answer, status, created_at)`.
    pub async fn finance_assistant_session(
        &self,
        org: Uuid,
        session_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, String, NaiveDateTime)>> {
        let rows = sqlx::query(
            "SELECT question, answer, status, created_at FROM finance_assistant_messages \
             WHERE organization_id = $1 AND session_id = $2 ORDER BY created_at",
        )
        .bind(org)
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("question"), r.get("answer"), r.get("status"), r.get("created_at")))
            .collect())
    }
}
