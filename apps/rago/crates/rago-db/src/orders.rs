//! Quote → order form → order lookups used by the order flow
//! (`rago_jobs::orders`).

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;
use crate::{OrderFormRow, OrderRow};

/// The approved version of a quote that an order form is issued from.
#[derive(Debug, Clone)]
pub struct ApprovedQuoteVersion {
    pub quote_id: Uuid,
    pub version_id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub currency: String,
    pub total_amount_cents: i64,
    pub line_items: Value,
}

/// An order with what it was ordered from.
#[derive(Debug, Clone)]
pub struct OrderContext {
    pub order_id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Option<Uuid>,
    pub order_form_id: Option<Uuid>,
    pub quote_id: Option<Uuid>,
    pub status: String,
    pub number: String,
}

impl Db {
    /// The latest approved, non-expired version of a quote.
    pub async fn approved_quote_version(&self, quote_id: Uuid) -> Result<Option<ApprovedQuoteVersion>> {
        let row = sqlx::query(
            "SELECT v.id AS version_id, v.quote_id, v.organization_id, q.customer_id, v.currency, \
                    v.total_amount_cents, v.line_items \
             FROM quote_versions v JOIN quotes q ON q.id = v.quote_id \
             WHERE v.quote_id = $1 AND v.status = 'approved' \
               AND (v.expires_at IS NULL OR v.expires_at > now()) \
             ORDER BY v.version_number DESC LIMIT 1",
        )
        .bind(quote_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(ApprovedQuoteVersion {
                quote_id: r.try_get("quote_id")?,
                version_id: r.try_get("version_id")?,
                organization_id: r.try_get("organization_id")?,
                customer_id: r.try_get("customer_id")?,
                currency: r.try_get("currency")?,
                total_amount_cents: r.try_get("total_amount_cents")?,
                line_items: r.try_get("line_items")?,
            })
        })
        .transpose()
    }

    /// The live (non-voided) order form already issued for a quote, if any.
    pub async fn open_order_form_for_quote(&self, quote_id: Uuid) -> Result<Option<OrderFormRow>> {
        let row = sqlx::query(
            "SELECT id, customer_id, number, status, execution_mode FROM order_forms \
             WHERE quote_id = $1 AND status <> 'voided' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(quote_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"), r.get("execution_mode"))))
    }

    /// `(organization_id, quote_id)` of an order form.
    pub async fn order_form_origin(&self, order_form_id: Uuid) -> Result<Option<(Uuid, Option<Uuid>)>> {
        let row = sqlx::query("SELECT organization_id, quote_id FROM order_forms WHERE id = $1")
            .bind(order_form_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("organization_id"), r.get("quote_id"))))
    }

    /// The order created from an order form, if any.
    pub async fn order_for_form(&self, order_form_id: Uuid) -> Result<Option<OrderRow>> {
        let row = sqlx::query(
            "SELECT id, customer_id, number, status FROM orders WHERE order_form_id = $1 \
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(order_form_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"))))
    }

    /// An order with its order form's quote.
    pub async fn order_context(&self, order_id: Uuid) -> Result<Option<OrderContext>> {
        let row = sqlx::query(
            "SELECT o.id, o.organization_id, o.customer_id, o.order_form_id, o.status, o.number, f.quote_id \
             FROM orders o LEFT JOIN order_forms f ON f.id = o.order_form_id WHERE o.id = $1",
        )
        .bind(order_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(OrderContext {
                order_id: r.try_get("id")?,
                organization_id: r.try_get("organization_id")?,
                customer_id: r.try_get("customer_id")?,
                order_form_id: r.try_get("order_form_id")?,
                quote_id: r.try_get("quote_id")?,
                status: r.try_get("status")?,
                number: r.try_get("number")?,
            })
        })
        .transpose()
    }

    /// Mark an order executed only if it is still pending (exactly-once).
    pub async fn claim_order_execution(&self, order_id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE orders SET status = 'executed', executed_at = now(), updated_at = now() \
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(order_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Put a claimed order back to pending (its invoice could not be issued).
    pub async fn release_order_execution(&self, order_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE orders SET status = 'pending', executed_at = NULL, updated_at = now() \
             WHERE id = $1 AND status = 'executed'",
        )
        .bind(order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
