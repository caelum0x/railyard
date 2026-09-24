//! Read models and guarded state transitions behind the REST quote → order
//! form → order surface (and the operator console's Quotes / Orders pages).
//!
//! The GraphQL admin rows ([`crate::QuoteRow`], [`crate::OrderFormRow`], ...)
//! stay minimal for Lago parity; these detail rows carry what a console needs to
//! walk the flow: the customer, the quote an order form / order came from, the
//! versions' line items and their approval / expiry timestamps, and the invoice
//! an executed order produced. Every list is scoped to one organization.

use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A quote with its customer and version summary.
#[derive(Debug, Clone)]
pub struct QuoteDetail {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_external_id: Option<String>,
    pub customer_name: Option<String>,
    pub number: String,
    pub status: String,
    pub currency: String,
    pub sub_total_amount_cents: i64,
    pub taxes_amount_cents: i64,
    pub total_amount_cents: i64,
    pub line_items: Value,
    pub accepted_invoice_id: Option<Uuid>,
    pub versions_count: i64,
    pub latest_version_status: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// One version of a quote, with its line items.
#[derive(Debug, Clone)]
pub struct QuoteVersionDetail {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub version_number: i32,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub line_items: Value,
    pub expires_at: Option<NaiveDateTime>,
    pub approved_at: Option<NaiveDateTime>,
    pub voided_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

/// An order form with its customer and quote.
#[derive(Debug, Clone)]
pub struct OrderFormDetail {
    pub id: Uuid,
    pub customer_id: Option<Uuid>,
    pub customer_external_id: Option<String>,
    pub customer_name: Option<String>,
    pub quote_id: Option<Uuid>,
    pub quote_number: Option<String>,
    pub number: String,
    pub status: String,
    pub execution_mode: Option<String>,
    pub execute_at: Option<NaiveDateTime>,
    pub signed_document: Option<String>,
    pub signed_at: Option<NaiveDateTime>,
    pub order_id: Option<Uuid>,
    pub created_at: NaiveDateTime,
}

/// An order with its customer, order form, quote and resulting invoice.
#[derive(Debug, Clone)]
pub struct OrderDetail {
    pub id: Uuid,
    pub customer_id: Option<Uuid>,
    pub customer_external_id: Option<String>,
    pub customer_name: Option<String>,
    pub order_form_id: Option<Uuid>,
    pub order_form_number: Option<String>,
    pub quote_id: Option<Uuid>,
    pub quote_number: Option<String>,
    pub number: String,
    pub status: String,
    pub executed_at: Option<NaiveDateTime>,
    pub invoice_id: Option<Uuid>,
    pub total_amount_cents: Option<i64>,
    pub currency: Option<String>,
    pub created_at: NaiveDateTime,
}

const QUOTE_SELECT: &str = "SELECT q.id, q.customer_id, c.external_id AS customer_external_id, c.name AS customer_name, \
       q.number, q.status, q.currency, q.sub_total_amount_cents, q.taxes_amount_cents, q.total_amount_cents, \
       q.line_items, q.accepted_invoice_id, q.created_at, q.updated_at, \
       (SELECT COUNT(*) FROM quote_versions v WHERE v.quote_id = q.id) AS versions_count, \
       (SELECT v.status FROM quote_versions v WHERE v.quote_id = q.id ORDER BY v.version_number DESC LIMIT 1) AS latest_version_status \
     FROM quotes q LEFT JOIN customers c ON c.id = q.customer_id";

fn quote_detail(r: &sqlx::postgres::PgRow) -> QuoteDetail {
    QuoteDetail {
        id: r.get("id"),
        customer_id: r.get("customer_id"),
        customer_external_id: r.get("customer_external_id"),
        customer_name: r.get("customer_name"),
        number: r.get("number"),
        status: r.get("status"),
        currency: r.get("currency"),
        sub_total_amount_cents: r.get("sub_total_amount_cents"),
        taxes_amount_cents: r.get("taxes_amount_cents"),
        total_amount_cents: r.get("total_amount_cents"),
        line_items: r.get("line_items"),
        accepted_invoice_id: r.get("accepted_invoice_id"),
        versions_count: r.get("versions_count"),
        latest_version_status: r.get("latest_version_status"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

fn version_detail(r: &sqlx::postgres::PgRow) -> QuoteVersionDetail {
    QuoteVersionDetail {
        id: r.get("id"),
        quote_id: r.get("quote_id"),
        version_number: r.get("version_number"),
        status: r.get("status"),
        currency: r.get("currency"),
        total_amount_cents: r.get("total_amount_cents"),
        line_items: r.get("line_items"),
        expires_at: r.get("expires_at"),
        approved_at: r.get("approved_at"),
        voided_at: r.get("voided_at"),
        created_at: r.get("created_at"),
    }
}

const ORDER_FORM_SELECT: &str = "SELECT f.id, f.customer_id, c.external_id AS customer_external_id, c.name AS customer_name, \
       f.quote_id, q.number AS quote_number, f.number, f.status, f.execution_mode, f.execute_at, f.signed_document, \
       f.signed_at, f.created_at, \
       (SELECT o.id FROM orders o WHERE o.order_form_id = f.id ORDER BY o.created_at LIMIT 1) AS order_id \
     FROM order_forms f LEFT JOIN customers c ON c.id = f.customer_id LEFT JOIN quotes q ON q.id = f.quote_id";

fn order_form_detail(r: &sqlx::postgres::PgRow) -> OrderFormDetail {
    OrderFormDetail {
        id: r.get("id"),
        customer_id: r.get("customer_id"),
        customer_external_id: r.get("customer_external_id"),
        customer_name: r.get("customer_name"),
        quote_id: r.get("quote_id"),
        quote_number: r.get("quote_number"),
        number: r.get("number"),
        status: r.get("status"),
        execution_mode: r.get("execution_mode"),
        execute_at: r.get("execute_at"),
        signed_document: r.get("signed_document"),
        signed_at: r.get("signed_at"),
        order_id: r.get("order_id"),
        created_at: r.get("created_at"),
    }
}

const ORDER_SELECT: &str = "SELECT o.id, o.customer_id, c.external_id AS customer_external_id, c.name AS customer_name, \
       o.order_form_id, f.number AS order_form_number, f.quote_id, q.number AS quote_number, o.number, o.status, \
       o.executed_at, o.created_at, \
       CASE WHEN o.status = 'executed' THEN q.accepted_invoice_id END AS invoice_id, \
       i.total_amount_cents, i.currency \
     FROM orders o LEFT JOIN customers c ON c.id = o.customer_id \
       LEFT JOIN order_forms f ON f.id = o.order_form_id \
       LEFT JOIN quotes q ON q.id = f.quote_id \
       LEFT JOIN invoices i ON i.id = q.accepted_invoice_id AND o.status = 'executed'";

fn order_detail(r: &sqlx::postgres::PgRow) -> OrderDetail {
    OrderDetail {
        id: r.get("id"),
        customer_id: r.get("customer_id"),
        customer_external_id: r.get("customer_external_id"),
        customer_name: r.get("customer_name"),
        order_form_id: r.get("order_form_id"),
        order_form_number: r.get("order_form_number"),
        quote_id: r.get("quote_id"),
        quote_number: r.get("quote_number"),
        number: r.get("number"),
        status: r.get("status"),
        executed_at: r.get("executed_at"),
        invoice_id: r.get("invoice_id"),
        total_amount_cents: r.get("total_amount_cents"),
        currency: r.get("currency"),
        created_at: r.get("created_at"),
    }
}

impl Db {
    /// Quotes of an organization (newest first), optionally filtered by status
    /// and customer.
    pub async fn quote_details(
        &self,
        org: Uuid,
        status: Option<&str>,
        customer_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<QuoteDetail>, i64)> {
        let rows = sqlx::query(&format!(
            "{QUOTE_SELECT} WHERE q.organization_id = $1 AND ($2::varchar IS NULL OR q.status = $2) \
               AND ($3::uuid IS NULL OR q.customer_id = $3) \
             ORDER BY q.created_at DESC LIMIT $4 OFFSET $5"
        ))
        .bind(org)
        .bind(status)
        .bind(customer_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM quotes q WHERE q.organization_id = $1 AND ($2::varchar IS NULL OR q.status = $2) \
               AND ($3::uuid IS NULL OR q.customer_id = $3)",
        )
        .bind(org)
        .bind(status)
        .bind(customer_id)
        .fetch_one(&self.pool)
        .await?
        .get("n");
        Ok((rows.iter().map(quote_detail).collect(), total))
    }

    /// One quote of an organization.
    pub async fn quote_detail(&self, org: Uuid, id: Uuid) -> Result<Option<QuoteDetail>> {
        let row = sqlx::query(&format!("{QUOTE_SELECT} WHERE q.organization_id = $1 AND q.id = $2"))
            .bind(org)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(quote_detail))
    }

    /// Every version of a quote (oldest first), with line items.
    pub async fn quote_version_details(&self, org: Uuid, quote_id: Uuid) -> Result<Vec<QuoteVersionDetail>> {
        let rows = sqlx::query(
            "SELECT id, quote_id, version_number, status, currency, total_amount_cents, line_items, expires_at, \
                    approved_at, voided_at, created_at \
             FROM quote_versions WHERE organization_id = $1 AND quote_id = $2 ORDER BY version_number",
        )
        .bind(org)
        .bind(quote_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(version_detail).collect())
    }

    /// One quote version of an organization.
    pub async fn quote_version_detail(&self, org: Uuid, id: Uuid) -> Result<Option<QuoteVersionDetail>> {
        let row = sqlx::query(
            "SELECT id, quote_id, version_number, status, currency, total_amount_cents, line_items, expires_at, \
                    approved_at, voided_at, created_at \
             FROM quote_versions WHERE organization_id = $1 AND id = $2",
        )
        .bind(org)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(version_detail))
    }

    /// Approve a draft version (only a draft can be approved). Any other
    /// approved version of the same quote is superseded (voided) so exactly one
    /// approved version drives the order form. Returns `false` when the version
    /// was not a draft.
    pub async fn approve_draft_quote_version(
        &self,
        id: Uuid,
        expires_at: Option<NaiveDateTime>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let approved = sqlx::query(
            "UPDATE quote_versions SET status = 'approved', approved_at = now(), expires_at = $2, updated_at = now() \
             WHERE id = $1 AND status = 'draft' RETURNING quote_id",
        )
        .bind(id)
        .bind(expires_at)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = approved else {
            tx.rollback().await?;
            return Ok(false);
        };
        let quote_id: Uuid = row.get("quote_id");
        sqlx::query(
            "UPDATE quote_versions SET status = 'voided', voided_at = now(), updated_at = now() \
             WHERE quote_id = $1 AND id <> $2 AND status = 'approved'",
        )
        .bind(quote_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // The quote mirrors its approved version's content and total.
        sqlx::query(
            "UPDATE quotes q SET line_items = v.line_items, total_amount_cents = v.total_amount_cents, \
                    version_number = v.version_number, updated_at = now() \
             FROM quote_versions v WHERE v.id = $1 AND q.id = v.quote_id",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Store a quote's re-priced amounts (after its approved version changed).
    pub async fn set_quote_amounts(&self, id: Uuid, sub_total: i64, taxes: i64, total: i64) -> Result<()> {
        sqlx::query(
            "UPDATE quotes SET sub_total_amount_cents = $2, taxes_amount_cents = $3, total_amount_cents = $4, \
                    updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(sub_total)
        .bind(taxes)
        .bind(total)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Void a version unless it is already voided. Returns `false` otherwise.
    pub async fn void_open_quote_version(&self, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE quote_versions SET status = 'voided', voided_at = now(), updated_at = now() \
             WHERE id = $1 AND status <> 'voided'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Void a quote that has not been accepted: the quote and every open
    /// version, plus any unsigned order form issued from it. Returns `false`
    /// when the quote was already accepted or voided.
    pub async fn void_open_quote(&self, org: Uuid, id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "UPDATE quotes SET status = 'voided', updated_at = now() \
             WHERE id = $1 AND organization_id = $2 AND status NOT IN ('accepted', 'voided')",
        )
        .bind(id)
        .bind(org)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE quote_versions SET status = 'voided', voided_at = now(), updated_at = now() \
             WHERE quote_id = $1 AND status <> 'voided'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE order_forms SET status = 'voided', updated_at = now() WHERE quote_id = $1 AND status = 'draft'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Order forms of an organization (newest first), optionally for one quote.
    pub async fn order_form_details(&self, org: Uuid, quote_id: Option<Uuid>) -> Result<Vec<OrderFormDetail>> {
        let rows = sqlx::query(&format!(
            "{ORDER_FORM_SELECT} WHERE f.organization_id = $1 AND ($2::uuid IS NULL OR f.quote_id = $2) \
             ORDER BY f.created_at DESC LIMIT 500"
        ))
        .bind(org)
        .bind(quote_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(order_form_detail).collect())
    }

    /// One order form of an organization.
    pub async fn order_form_detail(&self, org: Uuid, id: Uuid) -> Result<Option<OrderFormDetail>> {
        let row = sqlx::query(&format!("{ORDER_FORM_SELECT} WHERE f.organization_id = $1 AND f.id = $2"))
            .bind(org)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(order_form_detail))
    }

    /// Void an order form that has not been signed. Returns `false` otherwise.
    pub async fn void_draft_order_form(&self, org: Uuid, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE order_forms SET status = 'voided', updated_at = now() \
             WHERE id = $1 AND organization_id = $2 AND status = 'draft'",
        )
        .bind(id)
        .bind(org)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Orders of an organization (newest first), optionally filtered by status
    /// or quote.
    pub async fn order_details(
        &self,
        org: Uuid,
        status: Option<&str>,
        quote_id: Option<Uuid>,
    ) -> Result<Vec<OrderDetail>> {
        let rows = sqlx::query(&format!(
            "{ORDER_SELECT} WHERE o.organization_id = $1 AND ($2::varchar IS NULL OR o.status = $2) \
               AND ($3::uuid IS NULL OR f.quote_id = $3) \
             ORDER BY o.created_at DESC LIMIT 500"
        ))
        .bind(org)
        .bind(status)
        .bind(quote_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(order_detail).collect())
    }

    /// One order of an organization.
    pub async fn order_detail(&self, org: Uuid, id: Uuid) -> Result<Option<OrderDetail>> {
        let row = sqlx::query(&format!("{ORDER_SELECT} WHERE o.organization_id = $1 AND o.id = $2"))
            .bind(org)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(order_detail))
    }

    /// Cancel a pending order. Returns `false` when it was not pending.
    pub async fn void_pending_order(&self, org: Uuid, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE orders SET status = 'voided', updated_at = now() \
             WHERE id = $1 AND organization_id = $2 AND status = 'pending'",
        )
        .bind(id)
        .bind(org)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }
}
